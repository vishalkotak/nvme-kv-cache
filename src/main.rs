use std::path::PathBuf;

use nvme_kv_cache::file_backend::FileBackend;
use nvme_kv_cache::workload::Pattern;
use nvme_kv_cache::{BenchResult, DramBackend, run_workload};

// Usage:
//   nvme-kv-cache                 DRAM cold tier (baseline)
//   nvme-kv-cache file <path>     page-cache-bypassing file as the cold tier
#[tokio::main(flavor = "multi_thread", worker_threads = 4)]
async fn main() {
    let args: Vec<String> = std::env::args().collect();
    let file: Option<PathBuf> = match args.get(1).map(String::as_str) {
        None | Some("dram") => None,
        Some("file") => Some(args.get(2).expect("file backend needs a path").into()),
        Some(other) => panic!("unknown backend {other:?}, expected dram or file <path>"),
    };

    let num_pages = 10_000;
    let capacity = 1_000;
    let num_requests = 200_000;
    let seed = 42;
    let pattern = Pattern::Zipf { skew: 1.0 }; // the interesting one

    println!("backend = {}, pattern = {:?}, capacity = {}, pages = {}\n",
             if file.is_some() { "file" } else { "dram" }, pattern, capacity, num_pages);

    for qd in [1, 2, 4, 8, 16, 32] {
        let r: BenchResult = match &file {
            None => run_workload(DramBackend::new(), pattern, num_pages, capacity, num_requests, qd, seed).await,
            Some(path) => {
                let backend = FileBackend::create(path).expect("open backing file");
                run_workload(backend, pattern, num_pages, capacity, num_requests, qd, seed).await
            }
        }
        .unwrap();

        println!("queue_depth = {:>2}", qd);
        println!("  hit_rate  = {:.3}  coalesced = {}  throughput = {:.0} req/s",
                 r.hit_rate, r.coalesced, num_requests as f64 / r.elapsed.as_secs_f64());
        println!("  p50 = {:>8} ns   p99 = {:>9} ns   p999 = {:>9} ns",
                 r.histogram.value_at_quantile(0.50),
                 r.histogram.value_at_quantile(0.99),
                 r.histogram.value_at_quantile(0.999));
        println!();
    }
    if let Some(path) = file {
        let _ = std::fs::remove_file(path);
    }
}
