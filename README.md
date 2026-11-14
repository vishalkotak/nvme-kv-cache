# nvme-kv-cache

A tiered key-value cache that keeps hot pages in DRAM and spills cold pages to
a slower backing tier, built to study NVMe storage behavior under realistic
access patterns. Inspired by how LLM inference engines (e.g. vLLM's
PagedAttention) offload KV-cache pages from scarce GPU/host memory down a
memory hierarchy.

The central question this project exists to answer: **when is it worth pushing
cache pages down to NVMe and faulting them back, versus recomputing them?**

## Architecture

Three layers, deliberately separated so the storage tier can be swapped without
touching anything above it:

```
Harness   generates access patterns, controls queue depth, records latency
   |
Cache     tiering policy: bounded DRAM hot set, LRU eviction, fault-in on miss
   |
Backend   stores pages by id. Two implementations behind one trait:
          DramBackend (HashMap, baseline) | FileBackend (O_DIRECT / F_NOCACHE)
```

The `Backend` trait is the seam: the same `Cache` and `Harness` run against
either backend, so any change in the measured numbers is attributable to the
storage tier and not the surrounding code.

Design: [DESIGN.md](DESIGN.md). Results: [BENCHMARKS.md](BENCHMARKS.md).

## Status

- [x] Core types: `Page` (fixed 4 KiB), `PageId`, `Backend` trait
- [x] `DramBackend`: in-memory baseline
- [x] `Cache`: tiering, LRU eviction, hit/miss/eviction accounting
- [x] Harness: sequential / uniform / zipfian workloads, HDR latency histograms
- [x] Concurrency: configurable queue depth, single-flight coalescing of concurrent misses
- [x] O(1) LRU: intrusive linked list, p99 down ~10x under concurrency
- [x] `FileBackend`: real device I/O, page cache bypassed, blocking-pool queue depth
- [x] Eviction write-back buffer: a page is never in neither tier
- [ ] Linux NVMe numbers
- [ ] `io_uring` submission path

## Platform note

The NVMe backend requires Linux: `O_DIRECT` and `io_uring` do not exist on
macOS, and Apple Silicon storage sits behind the Apple Fabric controller with
no reachable device queues. The portable layers (everything except
`NvmeBackend`) run anywhere; device experiments target a Linux instance with
**local/instance NVMe** (not network-attached block storage).

## Running

```
cargo run --release                        # DRAM cold tier
cargo run --release -- file /mnt/nvme/kv    # file cold tier on a real disk
cargo test            # unit tests for the backend and tiering logic
```

`--release` matters: timing a debug build measures the absence of the optimizer.

## Results

Current DRAM-only baseline numbers are in [BENCHMARKS.md](./BENCHMARKS.md).

## Page size

`PAGE_SIZE = 4096` matches the common NVMe device block size, which keeps
`O_DIRECT` alignment on the future NVMe backend trivial (one page = one block).
