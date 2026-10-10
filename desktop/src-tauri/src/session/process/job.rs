//! A Windows Job Object per session, so stop, app exit and app crash kill the whole
//! process tree and not only claude.exe. This is the only file with `unsafe`.
//!
//! The job carries `KILL_ON_JOB_CLOSE` and its handle is owned by exactly one `Job`,
//! non-inheritable. When the app dies for any reason the OS closes the handle, which
//! kills every process still in the job. No breakaway is allowed.
//!
//! Known race, accepted: the child is assigned to the job after `Command::spawn` returns,
//! because std exposes no hook between creating the process and resuming it. The window
//! runs from `CreateProcessW` returning to `AssignProcessToJobObject` on the same thread,
//! and claude cannot spawn anything before its own runtime has initialised, so a
//! grandchild created in it is not expected. Children created after the assignment join
//! the job automatically.
//!
//! Closing the window on stable would need `CREATE_SUSPENDED` plus a Toolhelp thread scan
//! to find the primary thread (the one-thread premise is unverified), and any failure
//! after a suspended start strands a suspended child that must then be killed. That adds
//! more risk than it removes. Revisit when `ChildExt::main_thread_handle`
//! (rust-lang/rust#96723) is stable and gives the real thread handle with no scan.

use std::process::Child;

use windows_sys::Win32::Foundation::{CloseHandle, HANDLE};
use windows_sys::Win32::System::JobObjects::{
    AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
    JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectExtendedLimitInformation,
    SetInformationJobObject, TerminateJobObject,
};

pub struct Job(HANDLE);

// Sound because a job handle is a process-wide kernel handle with no thread affinity. The
// calls made through it are thread-safe.
unsafe impl Send for Job {}
// Sound for the same reason; shared references only make thread-safe kernel calls.
unsafe impl Sync for Job {}

impl Job {
    /// A new kill-on-close job, or `None` if the OS refuses.
    pub fn new() -> Option<Job> {
        // SAFETY: null attributes and name are allowed; the handle is non-inheritable.
        let handle = unsafe { CreateJobObjectW(std::ptr::null(), std::ptr::null()) };
        if handle.is_null() {
            return None;
        }
        let job = Job(handle);
        // SAFETY: an all-zero JOBOBJECT_EXTENDED_LIMIT_INFORMATION is a valid value.
        let mut info: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = unsafe { std::mem::zeroed() };
        info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        // SAFETY: `info` is a live struct of the size passed, and the handle is open.
        let ok = unsafe {
            SetInformationJobObject(
                job.0,
                JobObjectExtendedLimitInformation,
                std::ptr::addr_of!(info).cast(),
                std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
            )
        };
        // On failure `job` drops here and closes the handle.
        (ok != 0).then_some(job)
    }

    /// Put `child` in the job. False if the OS refuses; the caller falls back.
    pub fn assign(&self, child: &Child) -> bool {
        use std::os::windows::io::AsRawHandle;
        // SAFETY: both handles are open; `child` owns its process handle for this call.
        unsafe { AssignProcessToJobObject(self.0, child.as_raw_handle()) != 0 }
    }

    /// Kill every process in the job.
    pub fn terminate(&self) {
        // SAFETY: the handle is open for the life of `self`. Failure (an empty job) is fine.
        unsafe {
            TerminateJobObject(self.0, 1);
        }
    }
}

impl Drop for Job {
    fn drop(&mut self) {
        // SAFETY: the handle is owned by this value and closed exactly once.
        unsafe {
            CloseHandle(self.0);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::{Command, Stdio};
    use std::time::{Duration, Instant};
    use windows_sys::Win32::System::JobObjects::{
        JOBOBJECT_BASIC_ACCOUNTING_INFORMATION, JobObjectBasicAccountingInformation,
        QueryInformationJobObject,
    };

    fn active(job: &Job) -> u32 {
        // SAFETY: all-zero is a valid value for this plain struct.
        let mut info: JOBOBJECT_BASIC_ACCOUNTING_INFORMATION = unsafe { std::mem::zeroed() };
        // SAFETY: `info` is live and of the size passed; the handle is open.
        unsafe {
            QueryInformationJobObject(
                job.0,
                JobObjectBasicAccountingInformation,
                std::ptr::addr_of_mut!(info).cast(),
                std::mem::size_of::<JOBOBJECT_BASIC_ACCOUNTING_INFORMATION>() as u32,
                std::ptr::null_mut(),
            );
        }
        info.ActiveProcesses
    }

    fn wait_until(what: &str, mut f: impl FnMut() -> bool) {
        let deadline = Instant::now() + Duration::from_secs(5);
        while !f() {
            assert!(Instant::now() < deadline, "timed out: {what}");
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    /// cmd.exe with ping as its child, both inside a fresh job.
    fn tree() -> (Job, Child) {
        let child = Command::new("cmd")
            .args(["/c", "ping", "-n", "30", "127.0.0.1"])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn cmd");
        let job = Job::new().expect("create job");
        assert!(job.assign(&child), "assign");
        wait_until("cmd and ping in the job", || active(&job) == 2);
        (job, child)
    }

    #[test]
    fn terminate_kills_child_and_grandchild() {
        let (job, mut child) = tree();
        job.terminate();
        wait_until("job empty", || active(&job) == 0);
        wait_until("child reaped", || child.try_wait().unwrap().is_some());
    }

    #[test]
    fn closing_the_job_kills_the_tree() {
        let (job, mut child) = tree();
        drop(job);
        wait_until("child reaped", || child.try_wait().unwrap().is_some());
    }
}
