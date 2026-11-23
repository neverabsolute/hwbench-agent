//! Ask Windows not to throttle the benchmark.
//!
//! Windows 11 applies "EcoQoS" power throttling to programs it considers background work,
//! for example when the terminal running the benchmark isn't the active window. On CPUs
//! with performance and efficiency cores that moves the work onto efficiency cores, and a
//! Core i9-13900K measured below a Core i7-12700K. Opting out of execution-speed
//! throttling (the documented switch for this) keeps the threads at full speed. Other
//! systems need nothing.

#[cfg(windows)]
mod imp {
    use std::ffi::c_void;

    #[repr(C)]
    struct PowerThrottlingState {
        version: u32,
        control_mask: u32,
        state_mask: u32,
    }
    const VERSION: u32 = 1;
    const EXECUTION_SPEED: u32 = 0x1;
    const PROCESS_POWER_THROTTLING: i32 = 4; // PROCESS_INFORMATION_CLASS::ProcessPowerThrottling
    const THREAD_POWER_THROTTLING: i32 = 3; // THREAD_INFORMATION_CLASS::ThreadPowerThrottling

    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn GetCurrentProcess() -> *mut c_void;
        fn GetCurrentThread() -> *mut c_void;
        fn SetProcessInformation(process: *mut c_void, class: i32, info: *const c_void, size: u32) -> i32;
        fn SetThreadInformation(thread: *mut c_void, class: i32, info: *const c_void, size: u32) -> i32;
    }

    /// Control the execution-speed policy and set it to "not throttled".
    const OFF: PowerThrottlingState = PowerThrottlingState { version: VERSION, control_mask: EXECUTION_SPEED, state_mask: 0 };

    pub fn process() {
        let size = std::mem::size_of::<PowerThrottlingState>() as u32;
        // Fails harmlessly on Windows 10 builds without the API.
        unsafe { SetProcessInformation(GetCurrentProcess(), PROCESS_POWER_THROTTLING, (&OFF as *const PowerThrottlingState).cast(), size) };
    }

    pub fn thread() {
        let size = std::mem::size_of::<PowerThrottlingState>() as u32;
        unsafe { SetThreadInformation(GetCurrentThread(), THREAD_POWER_THROTTLING, (&OFF as *const PowerThrottlingState).cast(), size) };
    }
}

#[cfg(not(windows))]
mod imp {
    pub fn process() {}
    pub fn thread() {}
}

/// Opt the whole process out of power throttling. Call once at start-up.
pub fn full_speed_process() {
    imp::process();
}

/// Opt the calling thread out of power throttling. Call at the start of each worker thread.
pub fn full_speed_thread() {
    imp::thread();
}
