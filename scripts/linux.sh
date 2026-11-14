#!/bin/sh
# Run tests and benchmarks in a Linux container (Docker Desktop on macOS is fine).
# io_uring needs seccomp unconfined. Build output goes to a volume on ext4,
# because O_DIRECT fails on the macOS bind mount.
#
#   scripts/linux.sh                      cargo test --release
#   scripts/linux.sh bench uring          benchmark a backend: dram, file, uring
set -e
cd "$(dirname "$0")/.."
run() {
  docker run --rm --security-opt seccomp=unconfined \
    -v "$PWD":/src:ro -v nvmekv-target:/target -v nvmekv-cargo:/usr/local/cargo/registry \
    -e CARGO_TARGET_DIR=/target -w /src rust:1-bookworm sh -c "$1"
}
if [ "$1" = bench ]; then
  run "cargo build -q --release && /target/release/nvme-kv-cache ${2:-uring} /target/bench.bin"
else
  run "cargo test --release"
fi
