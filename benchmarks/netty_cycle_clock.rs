//! Win32 JNI clock for the isolated Netty benchmark agent.

use std::ffi::c_void;
use std::panic::catch_unwind;

#[link(name = "kernel32")]
extern "system" {
    fn GetCurrentThread() -> *mut c_void;
    fn GetCurrentThreadId() -> u32;
    fn QueryThreadCycleTime(thread: *mut c_void, cycles: *mut u64) -> i32;
}

#[no_mangle]
pub extern "system" fn Java_net_spicanetwork_rustnative_benchmark_ChunkTrackingNettyClock_cycles(
    _env: *mut c_void,
    _class: *mut c_void,
) -> i64 {
    catch_unwind(|| {
        let mut cycles = 0;
        // SAFETY: The Win32 pseudo-handle refers to this thread, and cycles is writable.
        if unsafe { QueryThreadCycleTime(GetCurrentThread(), &mut cycles) } == 0 {
            return -1;
        }
        i64::try_from(cycles).unwrap_or(-1)
    })
    .unwrap_or(-1)
}

#[no_mangle]
pub extern "system" fn Java_net_spicanetwork_rustnative_benchmark_ChunkTrackingNettyClock_threadId(
    _env: *mut c_void,
    _class: *mut c_void,
) -> i64 {
    // SAFETY: This call has no arguments and returns the calling thread's ID.
    catch_unwind(|| i64::from(unsafe { GetCurrentThreadId() })).unwrap_or(-1)
}
