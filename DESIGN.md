# Design

## Problem

LLM serving runs out of fast memory for KV cache. Pages can be dropped and recomputed, or spilled to a slower tier and faulted back. Which wins depends on the device's latency under load. This project measures it.

## Decisions

**One trait as the seam.** `Backend` is the only thing that changes between DRAM and disk. Cache, harness and workloads stay fixed, so a change in the numbers is the tier, not the code around it.

**Bypass the page cache.** `O_DIRECT` on Linux, `F_NOCACHE` on macOS. Otherwise the OS caches the "disk" in RAM and the benchmark measures memory twice.

**Single-flight misses.** Concurrent misses on one page share one read. Under skewed load, hot misses would otherwise multiply device reads at the worst moment.

**O(1) LRU.** An intrusive linked list replaced a full scan under the cache lock. The scan stalled every request during eviction, and p99 dropped about 10x once it was gone.

**Evicted pages stay readable until written.** Eviction leaves the lock before the disk write finishes. Without a write-back buffer, a page sat in neither tier during that window, and gets returned NotFound. Writes to one page are ordered with striped locks, so an older copy never lands after a newer one.

## Failure modes

- A failed spill leaves the page in the write-back buffer and returns the error. There's no retry yet.
- The write-back buffer is unbounded. A device slower than the eviction rate grows it without limit. It needs backpressure.

## Next

Linux NVMe numbers, an `io_uring` submission path, then a recompute cost model so the benchmark answers spill vs recompute directly.
