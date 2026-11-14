// Shared backend contract tests. Each backend's test file instantiates them
// with its constructor, so every cold tier is held to the same behavior.

#[macro_export]
macro_rules! backend_tests {
    ($create:expr) => {
        use std::io;
        use std::path::PathBuf;
        use std::sync::Arc;

        use nvme_kv_cache::{Backend, Cache, Page};

        // Under target/, which is on a real disk. tmpfs rejects O_DIRECT.
        fn scratch(name: &str) -> PathBuf {
            PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(format!("{}-{name}", module_path!()))
        }

        fn filled(byte: u8) -> Page {
            let mut p = Page::zeroed();
            p.data.fill(byte);
            p
        }

        #[tokio::test]
        async fn round_trips_pages_at_their_offsets() {
            let b = $create(&scratch("roundtrip.bin")).unwrap();
            for id in [0u64, 1, 7, 4_000] {
                b.put(id, &filled(id as u8 ^ 0x5A)).await.unwrap();
            }
            let mut out = Page::zeroed();
            for id in [0u64, 1, 7, 4_000] {
                b.get(id, &mut out).await.unwrap();
                assert!(out.data.iter().all(|&x| x == id as u8 ^ 0x5A), "page {id} corrupted");
            }
        }

        #[tokio::test]
        async fn unwritten_and_deleted_pages_are_not_found() {
            let b = $create(&scratch("missing.bin")).unwrap();
            let mut out = Page::zeroed();
            assert_eq!(b.get(3, &mut out).await.unwrap_err().kind(), io::ErrorKind::NotFound);
            b.put(3, &filled(1)).await.unwrap();
            b.delete(3).await.unwrap();
            assert_eq!(b.get(3, &mut out).await.unwrap_err().kind(), io::ErrorKind::NotFound);
        }

        #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
        async fn cache_spills_to_disk_and_faults_back_concurrently() {
            let b = $create(&scratch("cache.bin")).unwrap();
            let cache = Arc::new(Cache::new(b, 16));
            for id in 0..256u64 {
                cache.put(id, &filled(id as u8)).await.unwrap();
            }
            assert_eq!(cache.evictions(), 240);

            let mut tasks = Vec::new();
            for id in 0..256u64 {
                let c = Arc::clone(&cache);
                tasks.push(tokio::spawn(async move {
                    let mut out = Page::zeroed();
                    c.get(id, &mut out).await.unwrap();
                    (id, out.data[0], out.data[4095])
                }));
            }
            for t in tasks {
                let (id, first, last) = t.await.unwrap();
                assert_eq!((first, last), (id as u8, id as u8), "page {id} came back wrong");
            }
        }

        #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
        async fn many_concurrent_writes_and_reads() {
            // More in flight than one ring batch, so submission wraps.
            let b = Arc::new($create(&scratch("deep.bin")).unwrap());
            let mut tasks = Vec::new();
            for id in 0..1_000u64 {
                let b = Arc::clone(&b);
                tasks.push(tokio::spawn(async move { b.put(id, &filled(id as u8)).await }));
            }
            for t in tasks {
                t.await.unwrap().unwrap();
            }
            let mut tasks = Vec::new();
            for id in 0..1_000u64 {
                let b = Arc::clone(&b);
                tasks.push(tokio::spawn(async move {
                    let mut out = Page::zeroed();
                    b.get(id, &mut out).await.unwrap();
                    out.data.iter().all(|&x| x == id as u8)
                }));
            }
            for t in tasks {
                assert!(t.await.unwrap());
            }
        }
    };
}
