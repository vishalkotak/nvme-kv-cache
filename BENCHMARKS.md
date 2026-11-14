# Benchmarks

Baseline run against `DramBackend` (no NVMe tier yet), 200,000 accesses per
pattern, single-threaded, with the current O(n) victim scan. Latencies are
per `cache.get()`, measured with `--release`. These are a DRAM-only baseline
and not yet a storage result; the hit rates, however, are real and are the
signal that matters at this stage.

| Pattern | Hit rate | Hits | Misses | Evictions | p50 | p99 | p999 |
|---------|----------|------|--------|-----------|-----|-----|------|
| Sequential | 0.000 | 0 | 200,000 | 200,000 | 2,000 ns | 5,375 ns | 16,959 ns |
| Uniform | 0.100 | 20,027 | 179,973 | 179,973 | 1,958 ns | 3,333 ns | 9,215 ns |
| Zipf (skew 1.0) | 0.675 | 135,069 | 64,931 | 64,931 | 125 ns | 3,125 ns | 6,543 ns |

## Reading

- Sequential never revisits a page before it is evicted, so the hot set never
  helps: 0% hits, one eviction per access.
- Uniform random over the key space lands a 10% hit rate, roughly the hot-set
  size as a fraction of the key space.
- Zipf (skew 1.0) concentrates accesses on a small set of keys, so most
  requests are served from DRAM: 67.5% hits.
- The p50 split is the headline. Zipf p50 is 125 ns (the hot-set fast path, no
  eviction), while Sequential and Uniform sit around 2,000 ns because almost
  every access misses and pays the eviction plus fault-in cost. The tail
  (p99/p999) stays in the same band across patterns because the miss path
  dominates it.

This is the setup the NVMe tier will stress: once misses fault in from a real
device instead of a HashMap, the gap between the Zipf fast path and the miss
path is what decides when spilling to NVMe beats recomputing.

## O(1) LRU

Victim selection went from a full scan of the hot set to an intrusive linked
list. Zipf 1.0, 10,000 pages, 1,000 hot, 200,000 gets, M4 Pro, `--release`.

| Queue depth | p50 before | p50 after | p99 before | p99 after | p999 before | p999 after |
|---|---|---|---|---|---|---|
| 1 | 125 ns | 125 ns | 7,043 ns | 1,333 ns | 10,087 ns | 2,000 ns |
| 2 | 250 ns | 334 ns | 99,967 ns | 13,335 ns | 265,983 ns | 21,759 ns |
| 8 | 917 ns | 1,334 ns | 242,687 ns | 25,503 ns | 474,879 ns | 39,807 ns |
| 32 | 917 ns | 1,334 ns | 241,919 ns | 24,591 ns | 473,599 ns | 38,175 ns |

The tail is what moved. The scan ran under the cache lock, so every eviction
stalled every other request; p99 at depth 8 drops about 10x. The median under
contention rose about 45% and repeats across runs. Not explained yet.

## File backend on SSD

`FileBackend` as the cold tier, page cache bypassed (`F_NOCACHE`). Same
workload, M4 Pro internal SSD. A miss pays a 4 KiB read plus the evicted
page's 4 KiB write.

| Queue depth | Throughput | p50 | p99 | p999 |
|---|---|---|---|---|
| 1 | 17,392 req/s | 334 ns | 430 µs | 541 µs |
| 2 | 32,570 req/s | 250 ns | 482 µs | 559 µs |
| 4 | 47,444 req/s | 459 ns | 597 µs | 1.80 ms |
| 8 | 74,785 req/s | 375 ns | 698 µs | 995 µs |
| 16 | 91,457 req/s | 417 ns | 936 µs | 1.25 ms |
| 32 | 108,336 req/s | 500 ns | 1.56 ms | 2.08 ms |

- Throughput grows 6.2x from depth 1 to 32 while p99 grows 3.6x. The device
  has parallelism a single outstanding I/O leaves idle.
- p50 stays a DRAM hit (67.5% hit rate). The tail is the device.
- DRAM as the cold tier peaks at depth 1 (2.3M req/s) and drops to about
  560k at depth 2 and up: task spawn plus one cache lock costs more than a
  HashMap read. Concurrency only pays when the miss is slow.
- Next: the same table on Linux NVMe with `O_DIRECT`, then with `io_uring`.

## io_uring vs blocking pool

`scripts/linux.sh bench file` and `bench uring`. Linux 6.10 VM on the same Mac
(Docker Desktop, 12 vCPU), `O_DIRECT` on ext4. The disk is virtio backed by
the Mac SSD, so this compares submission paths, not NVMe.

| Queue depth | Blocking pool | io_uring | p99 pool | p99 io_uring |
|---|---|---|---|---|
| 1 | 17,801 req/s | 18,161 req/s | 256 µs | 250 µs |
| 8 | 40,030 req/s | 65,182 req/s | 1.23 ms | 515 µs |
| 16 | 45,897 req/s | 107,321 req/s | 3.00 ms | 681 µs |
| 32 | 48,893 req/s | 158,059 req/s | 6.93 ms | 989 µs |

- Same at depth 1: one I/O at a time costs the same either way.
- The pool flattens near 49k req/s while its p99 grows 27x. Each I/O is a
  thread handoff plus a syscall.
- io_uring keeps scaling: 3.2x the pool's throughput at depth 32 with a 7x
  lower p99. One ring thread submits a batch per syscall.
- A second run matched within 10% on throughput. p999 at depth 32 is noisy
  (4 to 11 ms) and isn't a result yet.
