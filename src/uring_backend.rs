// Cold tier on io_uring. Same on-disk layout as FileBackend (page N at
// N * PAGE_SIZE, O_DIRECT), different submission path.
//
// FileBackend parks one blocking thread per in-flight I/O. Here one ring
// thread owns the io_uring instance: it drains every queued request into the
// submission queue, submits them with one syscall, and replies as completions
// arrive. Queue depth is what the device sees, not how many threads exist.

use std::collections::HashSet;
use std::fs::{File, OpenOptions};
use std::io;
use std::os::fd::AsRawFd;
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;
use std::sync::Mutex;
use std::sync::mpsc;
use std::thread;

use io_uring::{IoUring, opcode, types};
use tokio::sync::oneshot;

use crate::file_backend::AlignedBuf;
use crate::{Backend, PAGE_SIZE, Page, PageId};

const RING_ENTRIES: u32 = 256;

enum Op {
    Read,
    Write,
}

struct Request {
    op: Op,
    offset: u64,
    buf: AlignedBuf,
    reply: oneshot::Sender<(io::Result<()>, AlignedBuf)>,
}

pub struct UringBackend {
    submit: Mutex<Option<mpsc::Sender<Request>>>,
    ring_thread: Mutex<Option<thread::JoinHandle<()>>>,
    present: Mutex<HashSet<PageId>>,
}

impl UringBackend {
    pub fn create(path: &Path) -> io::Result<Self> {
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(true)
            .custom_flags(libc::O_DIRECT)
            .open(path)?;
        let ring = IoUring::new(RING_ENTRIES)?;
        let (tx, rx) = mpsc::channel();
        let handle = thread::Builder::new()
            .name("uring".into())
            .spawn(move || ring_loop(ring, file, rx))?;
        Ok(UringBackend {
            submit: Mutex::new(Some(tx)),
            ring_thread: Mutex::new(Some(handle)),
            present: Mutex::new(HashSet::new()),
        })
    }

    async fn io(&self, op: Op, id: PageId, buf: AlignedBuf) -> (io::Result<()>, AlignedBuf) {
        let (reply, done) = oneshot::channel();
        let req = Request { op, offset: id * PAGE_SIZE as u64, buf, reply };
        let sent = match self.submit.lock().unwrap().as_ref() {
            Some(tx) => tx.send(req).map_err(|e| e.0),
            None => Err(req),
        };
        if let Err(req) = sent {
            return (Err(io::Error::other("ring thread stopped")), req.buf);
        }
        done.await.expect("ring thread replies to every request")
    }
}

impl Drop for UringBackend {
    fn drop(&mut self) {
        // Closing the channel tells the ring thread to finish in-flight I/O
        // and exit; join so the file and ring close before we return.
        self.submit.lock().unwrap().take();
        if let Some(h) = self.ring_thread.lock().unwrap().take() {
            let _ = h.join();
        }
    }
}

fn ring_loop(mut ring: IoUring, file: File, rx: mpsc::Receiver<Request>) {
    let fd = types::Fd(file.as_raw_fd());
    // Slot index is the io_uring user_data, so a completion finds its request.
    let mut slots: Vec<Option<Request>> = Vec::new();
    let mut free: Vec<usize> = Vec::new();
    let mut in_flight = 0usize;
    let mut open = true;

    loop {
        // Idle: block for work. Busy: take only what is already queued.
        if in_flight == 0 {
            if !open {
                return;
            }
            match rx.recv() {
                Ok(req) => push(&mut ring, fd, &mut slots, &mut free, &mut in_flight, req),
                Err(_) => return,
            }
        }
        while open && in_flight < RING_ENTRIES as usize {
            match rx.try_recv() {
                Ok(req) => push(&mut ring, fd, &mut slots, &mut free, &mut in_flight, req),
                Err(mpsc::TryRecvError::Empty) => break,
                Err(mpsc::TryRecvError::Disconnected) => {
                    open = false;
                    break;
                }
            }
        }

        // One syscall submits the whole batch and waits for at least one
        // completion. New requests queue in the channel meanwhile and join
        // the next batch.
        if let Err(e) = ring.submit_and_wait(1) {
            if e.kind() == io::ErrorKind::Interrupted {
                continue;
            }
            // The ring is unusable. Fail everything in flight and stop, so
            // callers get an error instead of waiting forever.
            for req in slots.iter_mut().filter_map(Option::take) {
                let _ = req.reply.send((Err(io::Error::new(e.kind(), e.to_string())), req.buf));
            }
            return;
        }

        for cqe in ring.completion() {
            let slot = cqe.user_data() as usize;
            let req = slots[slot].take().expect("completion for a live slot");
            free.push(slot);
            in_flight -= 1;
            let res = cqe.result();
            let outcome = if res < 0 {
                Err(io::Error::from_raw_os_error(-res))
            } else if res as usize != PAGE_SIZE {
                Err(io::Error::new(io::ErrorKind::UnexpectedEof, "short transfer"))
            } else {
                Ok(())
            };
            let _ = req.reply.send((outcome, req.buf));
        }
    }
}

fn push(
    ring: &mut IoUring,
    fd: types::Fd,
    slots: &mut Vec<Option<Request>>,
    free: &mut Vec<usize>,
    in_flight: &mut usize,
    req: Request,
) {
    let slot = free.pop().unwrap_or_else(|| {
        slots.push(None);
        slots.len() - 1
    });
    let ptr = req.buf.ptr;
    let entry = match req.op {
        Op::Read => opcode::Read::new(fd, ptr, PAGE_SIZE as u32).offset(req.offset).build(),
        Op::Write => opcode::Write::new(fd, ptr, PAGE_SIZE as u32).offset(req.offset).build(),
    }
    .user_data(slot as u64);
    // The buffer moves into the slot and stays put until its completion, so
    // the kernel's pointer into it stays valid.
    slots[slot] = Some(req);
    // in_flight < RING_ENTRIES, so the submission queue has room.
    unsafe { ring.submission().push(&entry).expect("submission queue has room") };
    *in_flight += 1;
}

impl Backend for UringBackend {
    async fn put(&self, id: PageId, page: &Page) -> io::Result<()> {
        let mut buf = AlignedBuf::new();
        buf.as_mut_slice().copy_from_slice(&page.data);
        let (res, _) = self.io(Op::Write, id, buf).await;
        res?;
        self.present.lock().unwrap().insert(id);
        Ok(())
    }

    async fn get(&self, id: PageId, out: &mut Page) -> io::Result<()> {
        if !self.present.lock().unwrap().contains(&id) {
            return Err(io::Error::new(io::ErrorKind::NotFound, "page not found"));
        }
        let (res, buf) = self.io(Op::Read, id, AlignedBuf::new()).await;
        res?;
        out.data.copy_from_slice(buf.as_slice());
        Ok(())
    }

    async fn delete(&self, id: PageId) -> io::Result<()> {
        self.present.lock().unwrap().remove(&id);
        Ok(())
    }
}
