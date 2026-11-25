//! Spica Native — High-Performance Rust Bridge for Purpur/Paper 1.21.4.
//!
//! Exposes both:
//! - Zero-copy JNI (`GetDirectBufferAddress`) for stock Java 21+ Purpur servers (zero extra JVM flags)
//! - Pure C ABI (`extern "C"`) for Java 22+ Panama FFM or native tooling.

pub mod c_abi;
pub mod farm_governor;
pub mod flowfield;
pub mod governor;
pub mod heatmap;
pub mod jni_bridge;
pub mod kernels;
pub mod mob_tracker;
pub mod noise;
pub mod spatial;
pub mod surge_guard;

use std::sync::OnceLock;

use rayon::{ThreadPool, ThreadPoolBuilder};

static WORKER_POOL: OnceLock<ThreadPool> = OnceLock::new();

/// Initialize the dedicated Rayon worker pool once.
///
/// Leaving `requested == 0` reserves headroom for the Minecraft Main Thread,
/// Netty I/O threads, and JVM GC threads (`max(2, cpus / 2)` capped at 8).
pub fn init_thread_pool(requested: usize) -> usize {
    let pool = WORKER_POOL.get_or_init(|| {
        let threads = if requested > 0 {
            requested.clamp(1, 64)
        } else {
            let cpus = std::thread::available_parallelism()
                .map(|n| n.get())
                .unwrap_or(4);
            (cpus / 2).clamp(2, 8)
        };

        ThreadPoolBuilder::new()
            .num_threads(threads)
            .thread_name(|idx| format!("spica-rust-worker-{idx}"))
            .build()
            .expect("failed to build Spica Rust Rayon thread pool")
    });

    pool.current_num_threads()
}

/// Number of active worker threads in the Rust pool.
pub fn active_threads() -> usize {
    WORKER_POOL
        .get()
        .map(|p| p.current_num_threads())
        .unwrap_or_else(|| init_thread_pool(0))
}

/// Execute a closure inside the dedicated Spica Rayon pool.
#[inline]
pub fn run_in_pool<OP, R>(op: OP) -> R
where
    OP: FnOnce() -> R + Send,
    R: Send,
{
    let pool = WORKER_POOL.get_or_init(|| {
        let cpus = std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(4);
        let threads = (cpus / 2).clamp(2, 8);
        ThreadPoolBuilder::new()
            .num_threads(threads)
            .thread_name(|idx| format!("spica-rust-worker-{idx}"))
            .build()
            .expect("failed to build Spica Rust Rayon thread pool")
    });

    pool.install(op)
}
