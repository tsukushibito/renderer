//! Windows boundary clocks. No callbacks, owned handles, or per-frame FFI.

use std::{ffi::c_void, io};

#[repr(C)]
#[derive(Default)]
struct FileTime {
    low: u32,
    high: u32,
}
impl FileTime {
    fn ticks(&self) -> u64 {
        u64::from(self.low) | (u64::from(self.high) << 32)
    }
}

#[link(name = "kernel32")]
unsafe extern "system" {
    fn QueryPerformanceCounter(value: *mut i64) -> i32;
    fn QueryPerformanceFrequency(value: *mut i64) -> i32;
    fn GetCurrentProcess() -> *mut c_void;
    fn GetCurrentThread() -> *mut c_void;
    fn GetCurrentThreadId() -> u32;
    fn GetProcessTimes(
        handle: *mut c_void,
        created: *mut FileTime,
        exited: *mut FileTime,
        kernel: *mut FileTime,
        user: *mut FileTime,
    ) -> i32;
    fn GetThreadTimes(
        handle: *mut c_void,
        created: *mut FileTime,
        exited: *mut FileTime,
        kernel: *mut FileTime,
        user: *mut FileTime,
    ) -> i32;
}

pub(super) fn frequency() -> io::Result<i64> {
    let mut value = 0;
    // SAFETY: The output is live writable LARGE_INTEGER-compatible storage.
    if unsafe { QueryPerformanceFrequency(&mut value) } == 0 {
        return Err(io::Error::last_os_error());
    }
    if value <= 0 {
        return Err(io::Error::other("non-positive QPC frequency"));
    }
    Ok(value)
}

pub(super) fn thread_id() -> u32 {
    // SAFETY: This parameterless API returns the current calling thread's ID.
    unsafe { GetCurrentThreadId() }
}

#[derive(Clone, Copy)]
pub(super) struct Boundary {
    pub qpc: i64,
    process: [u64; 2],
    thread: [u64; 2],
}
impl Boundary {
    pub fn capture() -> io::Result<Self> {
        let mut qpc = 0;
        // SAFETY: qpc is live writable LARGE_INTEGER-compatible storage.
        if unsafe { QueryPerformanceCounter(&mut qpc) } == 0 {
            return Err(io::Error::last_os_error());
        }
        let (mut created, mut exited) = (FileTime::default(), FileTime::default());
        let (mut kernel, mut user) = (FileTime::default(), FileTime::default());
        // SAFETY: The pseudo handle identifies this process and must not be
        // closed. Four distinct FILETIME-compatible outputs remain writable.
        if unsafe {
            GetProcessTimes(
                GetCurrentProcess(),
                &mut created,
                &mut exited,
                &mut kernel,
                &mut user,
            )
        } == 0
        {
            return Err(io::Error::last_os_error());
        }
        let process = [kernel.ticks(), user.ticks()];
        // SAFETY: The pseudo handle identifies this thread and must not be
        // closed. Outputs are distinct and live; the undefined live-process
        // exit-time fields are never read.
        if unsafe {
            GetThreadTimes(
                GetCurrentThread(),
                &mut created,
                &mut exited,
                &mut kernel,
                &mut user,
            )
        } == 0
        {
            return Err(io::Error::last_os_error());
        }
        Ok(Self {
            qpc,
            process,
            thread: [kernel.ticks(), user.ticks()],
        })
    }
    pub fn difference(self, start: Self, frequency: i64) -> io::Result<serde_json::Value> {
        let difference = |end: u64, begin: u64| {
            end.checked_sub(begin)
                .ok_or_else(|| io::Error::other("CPU time counter decreased"))
        };
        let ticks = self
            .qpc
            .checked_sub(start.qpc)
            .filter(|value| *value >= 0)
            .ok_or_else(|| io::Error::other("QPC decreased"))?;
        Ok(serde_json::json!({
            "qpc_start": start.qpc, "qpc_end": self.qpc, "qpc_frequency": frequency,
            "qpc_elapsed_ticks": ticks,
            "process_kernel_100ns": difference(self.process[0], start.process[0])?,
            "process_user_100ns": difference(self.process[1], start.process[1])?,
            "main_thread_kernel_100ns": difference(self.thread[0], start.thread[0])?,
            "main_thread_user_100ns": difference(self.thread[1], start.thread[1])?,
            "boundary_order": "QPC_then_process_times_then_thread_times",
            "cpu_scope": "process sums all threads; main_thread is the workload caller; not ready/blocked time"
        }))
    }
}
