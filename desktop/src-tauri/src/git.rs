//! Git, run as a child process. `run` is the only place `Command::new(GIT)` appears.
//! The program and the leading args are constants; every other arg is typed (`TaskBranch`,
//! `CommitId`, `BaseBranch`) or a path that Rust derived. Nothing from IPC reaches either.
//! Errors are short and fixed: git's stderr is discarded and never echoed.
// The task code that calls these lands in later slices.
#![allow(dead_code)]

use std::io::Read;
use std::path::{Component, Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

/// Resolved on PATH by std: no shell, no `.cmd` shim.
const GIT: &str = "git";

/// Default timeout for a call; commit and merge pass `LONG_TIMEOUT`.
pub const TIMEOUT: Duration = Duration::from_secs(30);
pub const LONG_TIMEOUT: Duration = Duration::from_secs(120);
/// Default stdout cap for the small answers (rev-parse, symbolic-ref, status).
pub const SMALL_CAP: usize = 64 * 1024;

#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GitError {
    /// `git` is not on PATH.
    NotFound,
    /// Spawn or pipe failure other than a missing program.
    Io,
    /// Killed after the timeout.
    Timeout,
    /// Exited non-zero; the code is kept for callers that branch on it.
    Failed(Option<i32>),
    /// An input did not pass validation, so git was never run.
    Invalid(&'static str),
}

impl GitError {
    /// The only text that may reach a user or a log.
    pub fn message(&self) -> &'static str {
        match self {
            GitError::NotFound => "Git was not found on PATH; tasks need Git for Windows",
            GitError::Io => "failed to run git",
            GitError::Timeout => "git took too long and was stopped",
            GitError::Failed(_) => "git could not complete the operation",
            GitError::Invalid(m) => m,
        }
    }
}

pub struct Output {
    pub stdout: Vec<u8>,
    /// More stdout arrived than the cap; the rest was read and dropped.
    pub truncated: bool,
    pub code: Option<i32>,
}

impl Output {
    pub fn success(&self) -> bool {
        self.code == Some(0)
    }

    /// stdout as trimmed text (lossy), for the one-line answers.
    pub fn text(&self) -> String {
        String::from_utf8_lossy(&self.stdout).trim().to_string()
    }
}

/// Run git in `repo` with fixed args. A non-zero exit is an `Ok(Output)` so a caller can
/// read the code (`show-ref --quiet`); use `run_ok` when it should be an error.
pub fn run(repo: &Path, args: &[&str], cap: usize, timeout: Duration) -> Result<Output, GitError> {
    let mut cmd = Command::new(GIT);
    cmd.args(["-c", "core.longpaths=true", "-c", "core.quotepath=false", "--no-pager", "-C"])
        .arg(repo)
        .args(args)
        .env("GIT_TERMINAL_PROMPT", "0");
    if args.first() == Some(&"status") {
        cmd.env("GIT_OPTIONAL_LOCKS", "0");
    }
    exec(cmd, cap, timeout)
}

/// `run`, with a non-zero exit as `GitError::Failed`.
pub fn run_ok(
    repo: &Path,
    args: &[&str],
    cap: usize,
    timeout: Duration,
) -> Result<Output, GitError> {
    let out = run(repo, args, cap, timeout)?;
    if out.success() { Ok(out) } else { Err(GitError::Failed(out.code)) }
}

/// Spawn, read stdout up to `cap` (draining the rest so the child never blocks on a full
/// pipe), and kill the child when `timeout` passes. The reader is not joined after a kill:
/// a grandchild holding the pipe would keep it open.
fn exec(mut cmd: Command, cap: usize, timeout: Duration) -> Result<Output, GitError> {
    cmd.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    let mut child = cmd.spawn().map_err(|e| {
        if e.kind() == std::io::ErrorKind::NotFound { GitError::NotFound } else { GitError::Io }
    })?;
    let mut pipe = child.stdout.take().ok_or(GitError::Io)?;
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let mut kept = Vec::new();
        let mut truncated = false;
        let mut buf = [0u8; 8192];
        loop {
            match pipe.read(&mut buf) {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    let room = cap.saturating_sub(kept.len());
                    if n > room {
                        truncated = true;
                    }
                    kept.extend_from_slice(&buf[..n.min(room)]);
                }
            }
        }
        let _ = tx.send((kept, truncated));
    });
    let deadline = Instant::now() + timeout;
    let code = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status.code(),
            Ok(None) => {}
            Err(_) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(GitError::Io);
            }
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            return Err(GitError::Timeout);
        }
        std::thread::sleep(Duration::from_millis(10));
    };
    let left = deadline.saturating_duration_since(Instant::now()).max(Duration::from_secs(1));
    let (stdout, truncated) = rx.recv_timeout(left).map_err(|_| GitError::Timeout)?;
    Ok(Output { stdout, truncated, code })
}

/// `git --version`, to tell the owner early that Git is missing.
pub fn check() -> Result<String, GitError> {
    let mut cmd = Command::new(GIT);
    cmd.arg("--version").env("GIT_TERMINAL_PROMPT", "0");
    let out = exec(cmd, SMALL_CAP, TIMEOUT)?;
    if out.success() { Ok(out.text()) } else { Err(GitError::Failed(out.code)) }
}

/// `orchestra/t<N>` with an optional `-2`..`-9` collision suffix. Built only by Rust.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskBranch(String);

impl TaskBranch {
    pub fn parse(s: &str) -> Result<TaskBranch, GitError> {
        let bad = GitError::Invalid("not a task branch name");
        let rest = s.strip_prefix("orchestra/t").ok_or(bad.clone())?;
        let (num, suffix) = match rest.split_once('-') {
            Some((n, s)) => (n, Some(s)),
            None => (rest, None),
        };
        let digits = !num.is_empty() && num.bytes().all(|b| b.is_ascii_digit());
        let suffix_ok = match suffix {
            None => true,
            Some(s) => s.len() == 1 && matches!(s.as_bytes()[0], b'2'..=b'9'),
        };
        if digits && suffix_ok { Ok(TaskBranch(s.to_string())) } else { Err(bad) }
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// A full object id: 40 (sha1) or 64 (sha256) lowercase hex.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommitId(String);

impl CommitId {
    pub fn parse(s: &str) -> Result<CommitId, GitError> {
        let ok = (s.len() == 40 || s.len() == 64)
            && s.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b));
        if ok { Ok(CommitId(s.to_string())) } else { Err(GitError::Invalid("not a commit id")) }
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// The branch a project had checked out, read from git and checked as a branch name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BaseBranch(String);

impl BaseBranch {
    pub fn parse(repo: &Path, name: &str) -> Result<BaseBranch, GitError> {
        if name.is_empty() || name.starts_with('-') {
            return Err(GitError::Invalid("not a branch name"));
        }
        run_ok(repo, &["check-ref-format", "--branch", name], SMALL_CAP, TIMEOUT)
            .map_err(|_| GitError::Invalid("not a branch name"))?;
        Ok(BaseBranch(name.to_string()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RepoInfo {
    /// Canonical, and equal to the canonical project path (a subdirectory is refused).
    pub toplevel: PathBuf,
    pub head_branch: BaseBranch,
    pub head_commit: CommitId,
}

/// The project must be the root of a work tree with a HEAD commit on a branch.
pub fn repo_info(path: &Path) -> Result<RepoInfo, GitError> {
    let top = run_ok(path, &["rev-parse", "--show-toplevel"], SMALL_CAP, TIMEOUT)?.text();
    let toplevel = std::fs::canonicalize(&top).map_err(|_| GitError::Io)?;
    let project = std::fs::canonicalize(path).map_err(|_| GitError::Io)?;
    if toplevel != project {
        return Err(GitError::Invalid("the project is not the root of its repository"));
    }
    let branch = run_ok(path, &["symbolic-ref", "--short", "HEAD"], SMALL_CAP, TIMEOUT)
        .map_err(|_| GitError::Invalid("the project is not on a branch"))?
        .text();
    let commit = run_ok(path, &["rev-parse", "--verify", "HEAD"], SMALL_CAP, TIMEOUT)
        .map_err(|_| GitError::Invalid("the project has no commits"))?
        .text();
    Ok(RepoInfo {
        toplevel,
        head_branch: BaseBranch::parse(path, &branch)?,
        head_commit: CommitId::parse(&commit)?,
    })
}

pub fn branch_exists(repo: &Path, branch: &TaskBranch) -> Result<bool, GitError> {
    let reference = format!("refs/heads/{}", branch.as_str());
    let out = run(repo, &["show-ref", "--verify", "--quiet", &reference], SMALL_CAP, TIMEOUT)?;
    match out.code {
        Some(0) => Ok(true),
        Some(1) => Ok(false),
        code => Err(GitError::Failed(code)),
    }
}

/// True when `path` is strictly inside `base`. Both must be absolute and `path` may not
/// contain `..`. Symlinks are resolved: the nearest existing ancestor of `path` is
/// canonicalised, so a link or junction cannot lead out. A missing `base` is outside.
pub fn is_within(base: &Path, path: &Path) -> bool {
    if !base.is_absolute() || !path.is_absolute() {
        return false;
    }
    if path.components().any(|c| matches!(c, Component::ParentDir)) {
        return false;
    }
    let Ok(base) = std::fs::canonicalize(base) else { return false };
    let mut existing = path;
    let mut tail = Vec::new();
    while !existing.exists() {
        let (Some(name), Some(parent)) = (existing.file_name(), existing.parent()) else {
            return false;
        };
        tail.push(name.to_owned());
        existing = parent;
    }
    let Ok(mut resolved) = std::fs::canonicalize(existing) else { return false };
    resolved.extend(tail.iter().rev());
    resolved != base && resolved.starts_with(&base)
}

/// `git worktree add -b <branch> <path> <commit>`, with the parent dirs created inside
/// `base`. `path` must pass `is_within`, and the branch must not already exist.
pub fn worktree_add(
    repo: &Path,
    base: &Path,
    path: &Path,
    branch: &TaskBranch,
    commit: &CommitId,
) -> Result<(), GitError> {
    if !is_within(base, path) {
        return Err(GitError::Invalid("the worktree path is outside the worktree folder"));
    }
    let parent = path.parent().ok_or(GitError::Invalid("bad worktree path"))?;
    std::fs::create_dir_all(parent).map_err(|_| GitError::Io)?;
    // Re-checked after the parents exist: a link made in between must not lead out.
    if !is_within(base, path) {
        return Err(GitError::Invalid("the worktree path is outside the worktree folder"));
    }
    let path = path.to_str().ok_or(GitError::Invalid("bad worktree path"))?;
    run_ok(
        repo,
        &["worktree", "add", "-b", branch.as_str(), path, commit.as_str()],
        SMALL_CAP,
        LONG_TIMEOUT,
    )
    .map(|_| ())
}

/// One `worktree remove --force` attempt, then `prune`. Retrying while a process still
/// holds the folder is the caller's job. `path` must pass `is_within`.
pub fn worktree_remove(repo: &Path, base: &Path, path: &Path) -> Result<(), GitError> {
    if !is_within(base, path) {
        return Err(GitError::Invalid("the worktree path is outside the worktree folder"));
    }
    let path = path.to_str().ok_or(GitError::Invalid("bad worktree path"))?;
    let removed = run_ok(repo, &["worktree", "remove", "--force", path], SMALL_CAP, LONG_TIMEOUT);
    let _ = run(repo, &["worktree", "prune"], SMALL_CAP, TIMEOUT);
    removed.map(|_| ())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    static N: AtomicUsize = AtomicUsize::new(0);

    /// Fixed identity so the tests do not depend on the machine's git config.
    const ID: [&str; 6] = [
        "-c",
        "user.name=t",
        "-c",
        "user.email=t@example.invalid",
        "-c",
        "commit.gpgsign=false",
    ];

    fn scratch(name: &str) -> PathBuf {
        let n = N.fetch_add(1, Ordering::SeqCst);
        let dir = std::env::temp_dir().join(format!("orchestra-git-{}-{n}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn git(repo: &Path, args: &[&str]) -> Output {
        let mut all: Vec<&str> = ID.to_vec();
        all.extend_from_slice(args);
        let out = run(repo, &all, SMALL_CAP, TIMEOUT).unwrap();
        assert!(out.success(), "git {args:?} failed");
        out
    }

    fn repo_with_commit(name: &str) -> PathBuf {
        let dir = scratch(name);
        git(&dir, &["init", "-b", "main"]);
        std::fs::write(dir.join("a.txt"), "a").unwrap();
        git(&dir, &["add", "a.txt"]);
        git(&dir, &["commit", "-m", "first"]);
        dir
    }

    #[test]
    fn git_check_reports_version() {
        assert!(check().unwrap().starts_with("git version"));
    }

    #[test]
    fn repo_info_rejects_non_repo_and_detached_head() {
        let plain = scratch("plain");
        assert!(repo_info(&plain).is_err());

        let repo = repo_with_commit("detached");
        let info = repo_info(&repo).unwrap();
        assert_eq!(info.head_branch.as_str(), "main");
        assert_eq!(info.head_commit.as_str().len(), 40);
        git(&repo, &["checkout", "--detach"]);
        assert!(repo_info(&repo).is_err());

        let unborn = scratch("unborn");
        git(&unborn, &["init", "-b", "main"]);
        assert!(repo_info(&unborn).is_err());
    }

    #[test]
    fn repo_info_rejects_subdir_project() {
        let repo = repo_with_commit("subdir");
        let sub = repo.join("sub");
        std::fs::create_dir_all(&sub).unwrap();
        assert!(repo_info(&sub).is_err());
        assert!(repo_info(&repo).is_ok());
    }

    #[test]
    fn worktree_add_then_remove_roundtrip() {
        let repo = repo_with_commit("wt");
        let base = scratch("wt-base");
        let info = repo_info(&repo).unwrap();
        let branch = TaskBranch::parse("orchestra/t1").unwrap();
        let path = base.join("p1").join("t1");

        assert!(!branch_exists(&repo, &branch).unwrap());
        worktree_add(&repo, &base, &path, &branch, &info.head_commit).unwrap();
        assert!(path.join("a.txt").is_file());
        assert!(branch_exists(&repo, &branch).unwrap());
        // The branch is taken now.
        let second = base.join("p1").join("t1b");
        assert!(worktree_add(&repo, &base, &second, &branch, &info.head_commit).is_err());

        worktree_remove(&repo, &base, &path).unwrap();
        assert!(!path.exists());
        assert!(branch_exists(&repo, &branch).unwrap());
        let list = git(&repo, &["worktree", "list", "--porcelain"]).text();
        assert!(!list.contains("t1"));

        let outside = scratch("wt-outside");
        let other = TaskBranch::parse("orchestra/t2").unwrap();
        assert!(worktree_add(&repo, &base, &outside.join("x"), &other, &info.head_commit).is_err());
        assert!(worktree_remove(&repo, &base, &outside).is_err());
    }

    #[test]
    fn branch_name_rejects_leading_dash_and_foreign_names() {
        for ok in ["orchestra/t1", "orchestra/t42", "orchestra/t7-2", "orchestra/t7-9"] {
            assert!(TaskBranch::parse(ok).is_ok(), "{ok}");
        }
        for bad in [
            "",
            "-orchestra/t1",
            "--force",
            "main",
            "orchestra/",
            "orchestra/t",
            "orchestra/tx",
            "orchestra/t1-",
            "orchestra/t1-1",
            "orchestra/t1-10",
            "orchestra/t1-2-3",
            "orchestra/t1 ",
            "orchestra/t1/..",
            "refs/heads/orchestra/t1",
        ] {
            assert!(TaskBranch::parse(bad).is_err(), "{bad}");
        }
        let repo = repo_with_commit("names");
        assert!(BaseBranch::parse(&repo, "-D").is_err());
        assert!(BaseBranch::parse(&repo, "a..b").is_err());
        assert!(BaseBranch::parse(&repo, "main").is_ok());
        assert!(CommitId::parse("--upload-pack=x").is_err());
        assert!(CommitId::parse(&"A".repeat(40)).is_err());
        assert!(CommitId::parse(&"a".repeat(40)).is_ok());
    }

    #[test]
    fn is_within_rejects_dotdot_and_outside_paths() {
        let base = scratch("within");
        assert!(is_within(&base, &base.join("p").join("t")));
        assert!(!is_within(&base, &base));
        assert!(!is_within(&base, &base.join("p").join("..").join("..")));
        assert!(!is_within(&base, &base.join("..").join("elsewhere")));
        assert!(!is_within(&base, &scratch("within-other").join("t")));
        assert!(!is_within(&base, Path::new("relative/t")));
        assert!(!is_within(Path::new("relative"), &base));
        assert!(!is_within(&base.join("missing"), &base.join("missing").join("t")));
        // A sibling that shares the prefix is not inside.
        let sibling = PathBuf::from(format!("{}-x", base.display()));
        assert!(!is_within(&base, &sibling.join("t")));
    }

    #[test]
    fn runner_times_out_and_kills() {
        // A fixed command that blocks for far longer than the timeout.
        #[cfg(windows)]
        let mut cmd = Command::new("ping");
        #[cfg(windows)]
        cmd.args(["-n", "60", "127.0.0.1"]);
        #[cfg(not(windows))]
        let mut cmd = Command::new("sleep");
        #[cfg(not(windows))]
        cmd.arg("60");
        let start = Instant::now();
        let err = exec(cmd, SMALL_CAP, Duration::from_millis(300)).err().unwrap();
        assert_eq!(err, GitError::Timeout);
        assert!(start.elapsed() < Duration::from_secs(10));
    }

    #[test]
    fn output_cap_truncates() {
        let repo = repo_with_commit("cap");
        let out = run(&repo, &["log", "--format=%H"], 10, TIMEOUT).unwrap();
        assert!(out.success());
        assert!(out.truncated);
        assert_eq!(out.stdout.len(), 10);
        let full = run(&repo, &["log", "--format=%H"], SMALL_CAP, TIMEOUT).unwrap();
        assert!(!full.truncated);
    }
}
