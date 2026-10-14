//! The child process: spawn, stdout reader thread, stdin writer, kill.
//! The program and its arguments are constants. Nothing from IPC reaches either.

use std::io::{BufRead, BufReader, Write};
use std::path::Path;
use std::process::{Child, ChildStdin, Stdio};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use super::settings::SessionSettings;
use super::state::ClaudeSessionId;

#[cfg(windows)]
mod job;
/// Off Windows there is no job; the handle never exists.
#[cfg(not(windows))]
mod job {
    pub struct Job;
    impl Job {
        pub fn terminate(&self) {}
        pub fn active(&self) -> u32 {
            0
        }
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

/// What varies per spawn: the conversation to continue, if any, and the session's settings.
/// A resumed process is a new spawn, so the settings are passed again each time.
pub struct SpawnArgs<'a> {
    pub resume: Option<&'a ClaudeSessionId>,
    pub settings: SessionSettings,
}

/// `ARGS`, then `--model` and `--permission-mode` when set, then `--resume <id>` when
/// continuing a saved conversation. The values are allowlisted enums mapped to static
/// strings and a `ClaudeSessionId` (a checked UUID), so none can read as a flag.
fn build_args(spawn: &SpawnArgs) -> Vec<String> {
    let mut args: Vec<String> = ARGS.iter().map(|a| a.to_string()).collect();
    if let Some(m) = spawn.settings.model {
        args.push("--model".into());
        args.push(m.as_str().into());
    }
    if let Some(m) = spawn.settings.permission_mode {
        args.push("--permission-mode".into());
        args.push(m.as_str().into());
    }
    if let Some(id) = spawn.resume {
        args.push("--resume".into());
        args.push(id.as_str().to_string());
    }
    args
}

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
    /// Block until the process, and on Windows every process in its job, has exited, or
    /// `timeout` passes. True when it is gone. Takes no session lock, so call it with none held.
    fn wait_exit(&self, timeout: Duration) -> bool;
}

/// Set once by the reader thread after it has reaped the child, just before `Output::Exit`.
type ExitLatch = Arc<(Mutex<bool>, Condvar)>;

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
    exited: ExitLatch,
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

    fn wait_exit(&self, timeout: Duration) -> bool {
        let deadline = Instant::now() + timeout;
        let (flag, cv) = &*self.exited;
        let mut done = flag.lock().unwrap_or_else(|e| e.into_inner());
        while !*done {
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return false;
            }
            done = cv.wait_timeout(done, left).unwrap_or_else(|e| e.into_inner()).0;
        }
        drop(done);
        // A grandchild can outlive the child and still hold the working folder. The job
        // knows: wait until it is empty too.
        if let Some(job) = &self.job {
            while job.active() > 0 {
                if Instant::now() >= deadline {
                    return false;
                }
                std::thread::sleep(Duration::from_millis(25));
            }
        }
        true
    }
}

impl Drop for ProcessHandle {
    fn drop(&mut self) {
        self.kill();
    }
}

pub fn spawn(
    dir: &Path,
    spawn_args: &SpawnArgs,
    sink: Box<dyn Fn(Output) + Send>,
) -> Result<Arc<dyn Proc>, String> {
    spawn_program(PROGRAM, &build_args(spawn_args), dir, sink)
}

/// `spawn` for any program. Production passes only `PROGRAM` and `build_args`; the
/// parameters exist so a test can run a real child through the real job and reader code.
fn spawn_program(
    program: &str,
    args: &[String],
    dir: &Path,
    sink: Box<dyn Fn(Output) + Send>,
) -> Result<Arc<dyn Proc>, String> {
    // Checked first, so a folder that vanished is not reported as a missing claude.exe.
    if !dir.is_dir() {
        return Err("the working folder no longer exists".to_string());
    }
    let mut cmd = std::process::Command::new(program);
    // The child ends of these pipes are inheritable by design (std creates them so). A
    // grandchild that claude starts with handle inheritance also holds them: if it outlives
    // claude, the reader sees no EOF and Exit is late. With a job, stop and app exit still
    // kill it. This cannot be closed here because claude makes that CreateProcess call.
    cmd.args(args)
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
    let exited: ExitLatch = Arc::new((Mutex::new(false), Condvar::new()));
    let reader_exited = Arc::clone(&exited);
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
        {
            let (flag, cv) = &*reader_exited;
            *flag.lock().unwrap_or_else(|e| e.into_inner()) = true;
            cv.notify_all();
        }
        sink(Output::Exit(code));
    });

    Ok(Arc::new(ProcessHandle { child, job, stdin: Mutex::new(stdin), exited }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::settings::{Model, PermissionMode};

    use std::sync::mpsc;

    /// Spawn a real `cmd.exe /c ping` (cmd is the child, ping a grandchild) in `dir`.
    #[cfg(windows)]
    fn spawn_ping(dir: &Path) -> (Arc<dyn Proc>, mpsc::Receiver<Output>) {
        let (tx, rx) = mpsc::channel();
        let tx = Mutex::new(tx);
        let args: Vec<String> = ["/c", "ping", "-n", "60", "127.0.0.1"].map(String::from).to_vec();
        let proc = spawn_program(
            "cmd",
            &args,
            dir,
            Box::new(move |o| {
                let _ = tx.lock().unwrap().send(o);
            }),
        )
        .expect("spawn cmd");
        // Let the grandchild start before the test acts on the tree.
        std::thread::sleep(Duration::from_millis(500));
        (proc, rx)
    }

    #[cfg(windows)]
    fn scratch(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("orchestra-proc-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    #[cfg(windows)]
    fn wait_exit_times_out_on_live_child() {
        let dir = scratch("live");
        let (proc, _rx) = spawn_ping(&dir);
        let t = Instant::now();
        assert!(!proc.wait_exit(Duration::from_millis(300)));
        assert!(t.elapsed() >= Duration::from_millis(250));
        proc.kill();
        assert!(proc.wait_exit(Duration::from_secs(10)));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    #[cfg(windows)]
    fn wait_exit_returns_true_after_kill() {
        let dir = scratch("kill");
        let (proc, rx) = spawn_ping(&dir);
        proc.kill();
        assert!(proc.wait_exit(Duration::from_secs(10)));
        assert!(matches!(rx.try_iter().last(), Some(Output::Exit(_))), "Exit was sent before wait_exit returned");
        assert!(proc.wait_exit(Duration::from_millis(1)), "the latch stays set");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn spawn_reports_missing_dir_not_missing_claude() {
        let gone = std::env::temp_dir().join(format!("orchestra-proc-{}-gone", std::process::id()));
        let _ = std::fs::remove_dir_all(&gone);
        let spawn_args = SpawnArgs { resume: None, settings: SessionSettings::default() };
        let err = spawn(&gone, &spawn_args, Box::new(|_| {})).err().expect("must refuse");
        assert!(err.contains("working folder"), "{err}");
        assert!(!err.contains("claude.exe"), "{err}");
    }

    /// P1: on Windows a live process with its cwd in a worktree blocks removal, and removal
    /// works once the tree is gone. The eprintln lines record what the OS actually did.
    #[test]
    #[cfg(windows)]
    fn removal_blocked_until_child_exits() {
        use crate::git;
        let run = |repo: &Path, args: &[&str]| {
            let out = std::process::Command::new("git")
                .args(["-c", "user.name=t", "-c", "user.email=t@example.invalid", "-c", "commit.gpgsign=false"])
                .args(args)
                .current_dir(repo)
                .output()
                .unwrap();
            assert!(out.status.success(), "git {args:?}");
        };
        let root = scratch("removal");
        let repo = root.join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        run(&repo, &["init", "-b", "main"]);
        std::fs::write(repo.join("a.txt"), "a").unwrap();
        run(&repo, &["add", "."]);
        run(&repo, &["commit", "-m", "init"]);
        let info = git::repo_info(&repo).unwrap();
        let base = root.join("worktrees");
        std::fs::create_dir_all(&base).unwrap();
        let path = base.join("p1").join("t1");
        let branch = git::TaskBranch::parse("orchestra/t1").unwrap();
        git::worktree_add(&repo, &base, &path, &branch, &info.head_commit).unwrap();

        // Measured on Windows: the blocked remove fails (exit 255, "Permission denied") but has
        // already deleted the checkout files and unregistered the worktree, leaving an empty
        // folder. A retry of `worktree remove` then fails (exit 128, "not a working tree"), so
        // retrying git alone never finishes the job: the leftover empty folder needs remove_dir.
        let (proc, _rx) = spawn_ping(&path);
        let blocked = git::worktree_remove(&repo, &base, &path);
        eprintln!("P1 removal while child alive: {blocked:?}; folder exists: {}", path.exists());
        assert!(blocked.is_err(), "removal must fail while a process has its cwd in the worktree");
        assert!(path.exists());

        proc.kill();
        assert!(proc.wait_exit(Duration::from_secs(10)));
        let retry = git::worktree_remove(&repo, &base, &path);
        eprintln!("P1 retry after wait_exit: {retry:?}; folder exists: {}", path.exists());
        assert!(retry.is_err(), "the first failed remove already unregistered the worktree");
        let mut gone = false;
        for ms in [0u64, 250, 500, 1000, 2000, 2000] {
            std::thread::sleep(Duration::from_millis(ms));
            if std::fs::remove_dir(&path).is_ok() {
                gone = true;
                break;
            }
        }
        eprintln!("P1 remove_dir after wait_exit: {gone}; folder exists: {}", path.exists());
        assert!(gone && !path.exists());

        // The order the cleanup must use: stop and wait first, then one remove succeeds.
        let path2 = base.join("p1").join("t2");
        let branch2 = git::TaskBranch::parse("orchestra/t2").unwrap();
        git::worktree_add(&repo, &base, &path2, &branch2, &info.head_commit).unwrap();
        let (proc2, _rx2) = spawn_ping(&path2);
        proc2.kill();
        assert!(proc2.wait_exit(Duration::from_secs(10)));
        let clean = git::worktree_remove(&repo, &base, &path2);
        eprintln!("P1 remove after wait_exit, no earlier attempt: {clean:?}; folder exists: {}", path2.exists());
        assert!(clean.is_ok(), "{clean:?}");
        assert!(!path2.exists());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn reap_backoff_is_capped() {
        let ms: Vec<u128> = (0..8).map(|a| reap_delay(a).as_millis()).collect();
        assert_eq!(ms, [50, 100, 200, 400, 800, 1000, 1000, 1000]);
        assert_eq!(reap_delay(u32::MAX).as_millis(), 1000);
    }

    const UUID: &str = "0b9f6a2c-3d4e-4f50-8a1b-2c3d4e5f6a7b";

    #[test]
    fn resume_args_follow_fixed_args() {
        let id = ClaudeSessionId::parse(UUID).unwrap();
        let args = build_args(&SpawnArgs { resume: Some(&id), settings: SessionSettings::default() });
        assert_eq!(args.len(), ARGS.len() + 2);
        assert!(args.iter().zip(ARGS).all(|(a, b)| a == b), "the fixed args are untouched and first");
        assert_eq!(&args[ARGS.len()..], ["--resume", id.as_str()]);
        assert_eq!(build_args(&SpawnArgs { resume: None, settings: SessionSettings::default() }).len(), ARGS.len());
        assert!(ClaudeSessionId::parse("--dangerous").is_none());
    }

    #[test]
    fn args_carry_settings_after_fixed_args() {
        let id = ClaudeSessionId::parse(UUID).unwrap();
        let settings = SessionSettings { model: Some(Model::Fable), permission_mode: Some(PermissionMode::AcceptEdits) };
        let args = build_args(&SpawnArgs { resume: Some(&id), settings });
        assert!(args.iter().zip(ARGS).all(|(a, b)| a == b), "the fixed args are untouched and first");
        assert_eq!(&args[ARGS.len()..], ["--model", "fable", "--permission-mode", "acceptEdits", "--resume", id.as_str()]);
        let only_mode = SessionSettings { model: None, permission_mode: Some(PermissionMode::Plan) };
        assert_eq!(&build_args(&SpawnArgs { resume: None, settings: only_mode })[ARGS.len()..], ["--permission-mode", "plan"]);
    }

    #[test]
    fn args_are_the_recorded_set() {
        assert_eq!(ARGS[0], "-p");
        assert!(ARGS.contains(&"--permission-prompt-tool"));
        assert!(ARGS.iter().all(|a| !a.contains(' ')));
    }
}
