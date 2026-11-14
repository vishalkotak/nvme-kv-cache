#![cfg(target_os = "linux")]
mod common;

backend_tests!(nvme_kv_cache::uring_backend::UringBackend::create);
