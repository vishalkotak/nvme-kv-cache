// Cold tier on a real block device. Page `id` lives at byte offset
// id * PAGE_SIZE in one sparse file, and every I/O bypasses the OS page
// cache (O_DIRECT on Linux, F_NOCACHE on macOS), so a miss pays the device,
// not a RAM copy dressed up as a read.
//
// I/O runs on tokio's blocking pool, one pread/pwrite per request. That gives
// real queue depth without io_uring; swapping the submission path is the next
// step and does not change this interface.

use std::alloc::{Layout, alloc_zeroed, dealloc};
use std::collections::HashSet;
use std::fs::{File, OpenOptions};
use std::io;
use std::os::unix::fs::FileExt;
use std::path::Path;
use std::sync::{Arc, Mutex};

use crate::{Backend, PAGE_SIZE, Page, PageId};

// O_DIRECT needs the buffer, offset and length aligned to the device's
// logical block size. 4096 covers every device worth benchmarking.
const ALIGN: usize = 4096;

struct AlignedBuf {
    ptr: *mut u8,
}

// The buffer is owned by exactly one task at a time.
unsafe impl Send for AlignedBuf {}

impl AlignedBuf {
    fn layout() -> Layout {
        Layout::from_size_align(PAGE_SIZE, ALIGN).expect("valid layout")
    }

    fn new() -> Self {
        let ptr = unsafe { alloc_zeroed(Self::layout()) };
        assert!(!ptr.is_null(), "aligned allocation failed");
        AlignedBuf { ptr }
    }

    fn as_slice(&self) -> &[u8] {
        unsafe { std::slice::from_raw_parts(self.ptr, PAGE_SIZE) }
    }

    fn as_mut_slice(&mut self) -> &mut [u8] {
        unsafe { std::slice::from_raw_parts_mut(self.ptr, PAGE_SIZE) }
    }
}

impl Drop for AlignedBuf {
    fn drop(&mut self) {
        unsafe { dealloc(self.ptr, Self::layout()) }
    }
}

pub struct FileBackend {
    file: Arc<File>,
    // Which pages have been written. A hole in a sparse file reads back as
    // zeros, so without this a missing page would look like a valid one.
    present: Mutex<HashSet<PageId>>,
}

impl FileBackend {
    pub fn create(path: &Path) -> io::Result<Self> {
        let mut opts = OpenOptions::new();
        opts.read(true).write(true).create(true).truncate(true);
        #[cfg(target_os = "linux")]
        {
            use std::os::unix::fs::OpenOptionsExt;
            opts.custom_flags(libc::O_DIRECT);
        }
        let file = opts.open(path)?;
        #[cfg(target_os = "macos")]
        {
            use std::os::fd::AsRawFd;
            if unsafe { libc::fcntl(file.as_raw_fd(), libc::F_NOCACHE, 1) } == -1 {
                return Err(io::Error::last_os_error());
            }
        }
        Ok(FileBackend { file: Arc::new(file), present: Mutex::new(HashSet::new()) })
    }

    fn offset(id: PageId) -> u64 {
        id * PAGE_SIZE as u64
    }
}

fn joined<T>(r: Result<io::Result<T>, tokio::task::JoinError>) -> io::Result<T> {
    r.map_err(|e| io::Error::other(e))?
}

impl Backend for FileBackend {
    async fn put(&self, id: PageId, page: &Page) -> io::Result<()> {
        let mut buf = AlignedBuf::new();
        buf.as_mut_slice().copy_from_slice(&page.data);
        let file = Arc::clone(&self.file);
        joined(
            tokio::task::spawn_blocking(move || {
                file.write_all_at(buf.as_slice(), Self::offset(id))
            })
            .await,
        )?;
        self.present.lock().unwrap().insert(id);
        Ok(())
    }

    async fn get(&self, id: PageId, out: &mut Page) -> io::Result<()> {
        if !self.present.lock().unwrap().contains(&id) {
            return Err(io::Error::new(io::ErrorKind::NotFound, "page not found"));
        }
        let file = Arc::clone(&self.file);
        let buf = joined(
            tokio::task::spawn_blocking(move || {
                let mut buf = AlignedBuf::new();
                file.read_exact_at(buf.as_mut_slice(), Self::offset(id))?;
                Ok(buf)
            })
            .await,
        )?;
        out.data.copy_from_slice(buf.as_slice());
        Ok(())
    }

    async fn delete(&self, id: PageId) -> io::Result<()> {
        // The bytes stay on disk until overwritten; only membership changes.
        self.present.lock().unwrap().remove(&id);
        Ok(())
    }
}
