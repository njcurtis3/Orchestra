//! The child process: spawn, stdout reader thread, stdin writer, kill.
//! The program and its arguments are constants. Nothing from IPC reaches either.

use std::io::{BufRead, BufReader, Write};
use std::path::Path;
use std::process::{Child, ChildStdin, Stdio};
use std::sync::{Arc, Mutex};
use std::time::Duration;

#[cfg(windows)]
mod job;
/// Off Windows there is no job; the handle never exists.
#[cfg(not(windows))]
mod job {
    pub struct Job;
    impl Job {
        pub fn terminate(&self) {}
    }
}

/// Resolved on PATH to `claude.exe`. A `.cmd` shim is not found by this lookup and is
/// reported as unsupported; nothing here ever goes through a shell.
const PROGRAM: &str = "claude";

/// Fixed spawn args (stream-json spike, `tests/fixtures/stream_json/README.md`).
pub const ARGS: [&str; 9] = [
    "-p",
    "--input-format",
    "stream-json",
    "--output-format",
    "stream-json",
    "--verbose",
    "--include-hook-events",
    "--permission-prompt-tool",
    "stdio",
];

#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

#[derive(Debug)]
pub enum Output {
    Line(String),
    Exit(Option<i32>),
}

/// What a session needs from its child. Lets tests stand in a fake.
pub trait Proc: Send + Sync {
    fn send_line(&self, line: &str) -> Result<(), String>;
    fn kill(&self);
}

/// Delay before reap poll `attempt` (0-based): 50 ms doubling, capped at 1 s.
fn reap_delay(attempt: u32) -> Duration {
    Duration::from_millis((50u64 << attempt.min(5)).min(1000))
}

/// Put the child in a kill-on-close job. `None` falls back to killing only the child.
#[cfg(windows)]
fn attach_job(child: &Child) -> Option<Arc<job::Job>> {
    let job = job::Job::new()?;
    job.assign(child).then(|| Arc::new(job))
}

#[cfg(not(windows))]
fn attach_job(_child: &Child) -> Option<Arc<job::Job>> {
    None
}

pub struct ProcessHandle {
    child: Arc<Mutex<Child>>,
    job: Option<Arc<job::Job>>,
    stdin: Mutex<ChildStdin>,
}

impl Proc for ProcessHandle {
    fn send_line(&self, line: &str) -> Result<(), String> {
        let mut stdin = self.stdin.lock().unwrap_or_else(|e| e.into_inner());
        stdin
            .write_all(line.as_bytes())
            .and_then(|_| stdin.write_all(b"\n"))
            .and_then(|_| stdin.flush())
            .map_err(|_| "failed to write to session".to_string())
    }

    fn kill(&self) {
        // With a job the reader blocks in wait() holding the child lock, so kill goes
        // through the job and takes no lock. It also reaches grandchildren.
        if let Some(job) = &self.job {
            job.terminate();
            return;
        }
        let mut child = self.child.lock().unwrap_or_else(|e| e.into_inner());
        let _ = child.kill();
    }
}

impl Drop for ProcessHandle {
    fn drop(&mut self) {
        self.kill();
    }
}

pub fn spawn(dir: &Path, sink: Box<dyn Fn(Output) + Send>) -> Result<Arc<dyn Proc>, String> {
    let mut cmd = std::process::Command::new(PROGRAM);
    // The child ends of these pipes are inheritable by design (std creates them so). A
    // grandchild that claude starts with handle inheritance also holds them: if it outlives
    // claude, the reader sees no EOF and Exit is late. With a job, stop and app exit still
    // kill it. This cannot be closed here because claude makes that CreateProcess call.
    cmd.args(ARGS)
        .current_dir(dir)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    let mut child = cmd.spawn().map_err(|e| {
        if e.kind() == std::io::ErrorKind::NotFound {
            "claude.exe was not found on PATH (a .cmd shim is not supported)".to_string()
        } else {
            "failed to start claude".to_string()
        }
    })?;
    let stdin = child.stdin.take().ok_or("failed to open session stdin")?;
    let stdout = child.stdout.take().ok_or("failed to open session stdout")?;
    let job = attach_job(&child);
    let child = Arc::new(Mutex::new(child));

    let reader_child = Arc::clone(&child);
    let has_job = job.is_some();
    std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines() {
            match line {
                Ok(l) if !l.trim().is_empty() => sink(Output::Line(l)),
                Ok(_) => {}
                Err(_) => break,
            }
        }
        // stdout closed: the child is gone or going. Reap it for the exit code.
        // With a job, kill() never takes the child lock, so block in wait(). Without one,
        // poll with a capped backoff so kill() is not held off by the lock.
        let code = if has_job {
            let mut c = reader_child.lock().unwrap_or_else(|e| e.into_inner());
            c.wait().ok().and_then(|s| s.code())
        } else {
            let mut attempt = 0;
            loop {
                let status = {
                    let mut c = reader_child.lock().unwrap_or_else(|e| e.into_inner());
                    c.try_wait()
                };
                match status {
                    Ok(Some(s)) => break s.code(),
                    Ok(None) => {
                        std::thread::sleep(reap_delay(attempt));
                        attempt += 1;
                    }
                    Err(_) => break None,
                }
            }
        };
        sink(Output::Exit(code));
    });

    Ok(Arc::new(ProcessHandle { child, job, stdin: Mutex::new(stdin) }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reap_backoff_is_capped() {
        let ms: Vec<u128> = (0..8).map(|a| reap_delay(a).as_millis()).collect();
        assert_eq!(ms, [50, 100, 200, 400, 800, 1000, 1000, 1000]);
        assert_eq!(reap_delay(u32::MAX).as_millis(), 1000);
    }

    #[test]
    fn args_are_the_recorded_set() {
        assert_eq!(ARGS[0], "-p");
        assert!(ARGS.contains(&"--permission-prompt-tool"));
        assert!(ARGS.iter().all(|a| !a.contains(' ')));
    }
}
