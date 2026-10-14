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

use serde::Serialize;

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

/// Variables that make git ignore `-C` and act on another repo, index or object store.
const SCRUBBED_ENV: [&str; 10] = [
    "GIT_DIR",
    "GIT_WORK_TREE",
    "GIT_INDEX_FILE",
    "GIT_OBJECT_DIRECTORY",
    "GIT_ALTERNATE_OBJECT_DIRECTORIES",
    "GIT_COMMON_DIR",
    "GIT_NAMESPACE",
    "GIT_CEILING_DIRECTORIES",
    "GIT_DISCOVERY_ACROSS_FILESYSTEM",
    "GIT_PREFIX",
];

/// Drop the inherited repo-selecting variables so `-C` is the only thing that picks the repo.
fn scrub_env(cmd: &mut Command) {
    for name in SCRUBBED_ENV {
        cmd.env_remove(name);
    }
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
    scrub_env(&mut cmd);
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

/// Most entries and deepest nesting the link walk will visit; past either, removal is
/// refused rather than handed to git unchecked.
const MAX_WALK_ENTRIES: usize = 500_000;
const MAX_WALK_DEPTH: usize = 128;

/// True for a symlink, and on Windows for any reparse point (junction, mount point).
fn is_link(md: &std::fs::Metadata) -> bool {
    if md.file_type().is_symlink() {
        return true;
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;
        if md.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
            return true;
        }
    }
    false
}

/// True when a link is a directory link, which is removed with `remove_dir`.
fn is_dir_link(md: &std::fs::Metadata) -> bool {
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        const FILE_ATTRIBUTE_DIRECTORY: u32 = 0x10;
        md.file_attributes() & FILE_ATTRIBUTE_DIRECTORY != 0
    }
    #[cfg(not(windows))]
    {
        md.is_dir()
    }
}

/// Removes the link at a path, never its target: `bool` is true for a directory link.
type Unlink<'a> = &'a dyn Fn(&Path, bool) -> std::io::Result<()>;

fn unlink_real(path: &Path, dir: bool) -> std::io::Result<()> {
    #[cfg(windows)]
    {
        if dir { std::fs::remove_dir(path) } else { std::fs::remove_file(path) }
    }
    #[cfg(not(windows))]
    {
        let _ = dir;
        std::fs::remove_file(path)
    }
}

/// Walk `root` without following links and remove every link it holds (the link itself,
/// never its target), so a later recursive delete cannot reach outside. Anything
/// unexpected (a link that will not go, a walk past the bounds) is an error.
fn unlink_links(root: &Path, unlink: Unlink) -> Result<(), GitError> {
    const FAIL: GitError = GitError::Invalid("the worktree holds a link that could not be removed");
    let root_md = std::fs::symlink_metadata(root).map_err(|_| GitError::Io)?;
    if is_link(&root_md) || !root_md.is_dir() {
        return Err(FAIL);
    }
    let mut stack = vec![(root.to_path_buf(), 0usize)];
    let mut seen = 0usize;
    while let Some((dir, depth)) = stack.pop() {
        if depth > MAX_WALK_DEPTH {
            return Err(FAIL);
        }
        for entry in std::fs::read_dir(&dir).map_err(|_| GitError::Io)? {
            let path = entry.map_err(|_| GitError::Io)?.path();
            seen += 1;
            if seen > MAX_WALK_ENTRIES || !path.starts_with(root) {
                return Err(FAIL);
            }
            let md = std::fs::symlink_metadata(&path).map_err(|_| GitError::Io)?;
            if is_link(&md) {
                unlink(&path, is_dir_link(&md)).map_err(|_| FAIL)?;
                if std::fs::symlink_metadata(&path).is_ok() {
                    return Err(FAIL);
                }
            } else if md.is_dir() {
                stack.push((path, depth + 1));
            }
        }
    }
    Ok(())
}

/// Unlink every link in the worktree, then one `worktree remove --force` attempt, then
/// `prune`. `git worktree remove` follows junctions and deletes their targets, so git is
/// never called while a link remains: if one cannot be removed this errors first and the
/// caller keeps the task in cleanup. Retrying while a process still holds the folder is the
/// caller's job. `path` must pass `is_within`.
pub fn worktree_remove(repo: &Path, base: &Path, path: &Path) -> Result<(), GitError> {
    worktree_remove_with(repo, base, path, &unlink_real)
}

fn worktree_remove_with(
    repo: &Path,
    base: &Path,
    path: &Path,
    unlink: Unlink,
) -> Result<(), GitError> {
    if !is_within(base, path) {
        return Err(GitError::Invalid("the worktree path is outside the worktree folder"));
    }
    // A folder already gone has no links; git still unregisters it below.
    if path.exists() {
        unlink_links(path, unlink)?;
    }
    let path = path.to_str().ok_or(GitError::Invalid("bad worktree path"))?;
    let removed = run_ok(repo, &["worktree", "remove", "--force", path], SMALL_CAP, LONG_TIMEOUT);
    let _ = run(repo, &["worktree", "prune"], SMALL_CAP, TIMEOUT);
    removed.map(|_| ())
}

/// `git worktree prune`: forget worktrees whose folder is gone.
pub fn worktree_prune(repo: &Path) -> Result<(), GitError> {
    run_ok(repo, &["worktree", "prune"], SMALL_CAP, TIMEOUT).map(|_| ())
}

/// Remove an empty folder under `base`, never recursively. This finishes a removal that
/// git left half done (the checkout deleted and unregistered, the empty folder still there).
/// `path` must pass `is_within`; a folder with anything in it is left alone. `remove_dir`
/// never recurses and on a junction removes only the link, so no link walk is needed.
pub fn remove_empty_dir(base: &Path, path: &Path) -> Result<(), GitError> {
    if !is_within(base, path) {
        return Err(GitError::Invalid("the worktree path is outside the worktree folder"));
    }
    std::fs::remove_dir(path).map_err(|_| GitError::Io)
}

/// Largest patch handed to the frontend (gate G10), cut on a line boundary.
pub const DIFF_CAP_BYTES: usize = 1024 * 1024;
/// Most files listed for one diff.
pub const MAX_DIFF_FILES: usize = 5000;
/// Longest commit body kept for display.
pub const BODY_CAP_BYTES: usize = 4096;
/// Room for the file list and the commit log; more than this is refused, not guessed at.
const LIST_CAP: usize = 4 * 1024 * 1024;

/// Stage everything in the work tree, new files included.
pub fn stage_all(worktree: &Path) -> Result<(), GitError> {
    run_ok(worktree, &["add", "-A"], SMALL_CAP, LONG_TIMEOUT).map(|_| ())
}

/// Something is staged (`diff --cached --quiet` exits 1).
pub fn has_staged(worktree: &Path) -> Result<bool, GitError> {
    match run(worktree, &["diff", "--cached", "--quiet"], SMALL_CAP, TIMEOUT)?.code {
        Some(0) => Ok(false),
        Some(1) => Ok(true),
        code => Err(GitError::Failed(code)),
    }
}

/// Commit what is staged. Hooks run. No `--author` and no trailer: the identity is the one
/// configured for the repository.
pub fn commit(worktree: &Path, message: &str) -> Result<(), GitError> {
    run_ok(worktree, &["commit", "-m", message], SMALL_CAP, LONG_TIMEOUT).map(|_| ())
}

/// No tracked file is modified or staged. Untracked files do not count.
pub fn is_clean_tracked(repo: &Path) -> Result<bool, GitError> {
    let out = run_ok(repo, &["status", "--porcelain", "--untracked-files=no"], SMALL_CAP, TIMEOUT)?;
    Ok(out.stdout.is_empty() && !out.truncated)
}

/// The branch HEAD is on; an error when HEAD is detached.
pub fn current_branch(repo: &Path) -> Result<String, GitError> {
    Ok(run_ok(repo, &["symbolic-ref", "--short", "HEAD"], SMALL_CAP, TIMEOUT)?.text())
}

pub fn head_commit(repo: &Path) -> Result<CommitId, GitError> {
    CommitId::parse(&run_ok(repo, &["rev-parse", "--verify", "HEAD"], SMALL_CAP, TIMEOUT)?.text())
}

/// How many commits `branch` has beyond `base`.
pub fn commits_ahead(repo: &Path, base: &CommitId, branch: &TaskBranch) -> Result<usize, GitError> {
    let range = format!("{}..refs/heads/{}", base.as_str(), branch.as_str());
    let out = run_ok(repo, &["rev-list", "--count", &range], SMALL_CAP, TIMEOUT)?;
    out.text().parse().map_err(|_| GitError::Io)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MergeOutcome {
    Merged,
    /// The merge stopped on conflicts and was aborted; nothing changed in the checkout.
    Conflict,
}

/// `merge --no-ff --no-edit -m <message> <branch>` in `repo` (gate G3). A failed merge is
/// aborted, so no merge is ever left in progress. Hooks run.
pub fn merge(repo: &Path, branch: &TaskBranch, message: &str) -> Result<MergeOutcome, GitError> {
    // The full ref: a tag of the same name must not be merged in its place.
    let full = format!("refs/heads/{}", branch.as_str());
    let args = ["merge", "--no-ff", "--no-edit", "-m", message, &full];
    let result = run(repo, &args, SMALL_CAP, LONG_TIMEOUT);
    if matches!(&result, Ok(out) if out.success()) {
        return Ok(MergeOutcome::Merged);
    }
    let merge_head = || {
        run(repo, &["rev-parse", "-q", "--verify", "MERGE_HEAD"], SMALL_CAP, TIMEOUT)
            .map(|o| o.success())
            .unwrap_or(true)
    };
    let in_progress = merge_head();
    if in_progress {
        let _ = run(repo, &["merge", "--abort"], SMALL_CAP, TIMEOUT);
        // An abort that did not take leaves the checkout mid-merge: say so, do not claim
        // that nothing changed.
        if merge_head() {
            return Err(GitError::Invalid("the merge could not be aborted; the project checkout may need attention"));
        }
    }
    match result {
        Ok(_) if in_progress => Ok(MergeOutcome::Conflict),
        Ok(out) => Err(GitError::Failed(out.code)),
        Err(e) => Err(e),
    }
}

/// `branch -d` (merged only) or `-D`. `branch` is a task branch, so it cannot be an option.
/// `git branch` only ever names `refs/heads`, so a tag of the same name is not in play.
pub fn branch_delete(repo: &Path, branch: &TaskBranch, force: bool) -> Result<(), GitError> {
    let flag = if force { "-D" } else { "-d" };
    run_ok(repo, &["branch", flag, branch.as_str()], SMALL_CAP, TIMEOUT).map(|_| ())
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DiffFile {
    pub path: String,
    /// `added`, `modified` or `deleted`.
    pub status: &'static str,
    pub added: u64,
    pub removed: u64,
    pub binary: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Diff {
    pub files: Vec<DiffFile>,
    /// More files changed than `MAX_DIFF_FILES`.
    pub files_truncated: bool,
    pub patch: String,
    /// The patch was cut at `cap` bytes.
    pub truncated: bool,
}

/// What is staged in `worktree` against `base`. Call `stage_all` first. The file list comes
/// from `--numstat`, so binary files show up as binary with no patch text.
pub fn diff(worktree: &Path, base: &CommitId, cap: usize) -> Result<Diff, GitError> {
    const COMMON: [&str; 6] = ["diff", "--cached", "--no-color", "--no-ext-diff", "--no-textconv", "--no-renames"];
    let call = |extra: &[&str], cap: usize, timeout: Duration| {
        let mut args: Vec<&str> = COMMON.to_vec();
        args.extend_from_slice(extra);
        args.push(base.as_str());
        args.push("--");
        run_ok(worktree, &args, cap, timeout)
    };
    let names = call(&["--name-status", "-z"], LIST_CAP, TIMEOUT)?;
    let numstat = call(&["--numstat", "-z"], LIST_CAP, TIMEOUT)?;
    let patch = call(&[], cap, LONG_TIMEOUT)?;

    let mut status = std::collections::HashMap::new();
    let mut fields = String::from_utf8_lossy(&names.stdout).into_owned();
    if names.truncated {
        fields.truncate(fields.rfind('\0').unwrap_or(0));
    }
    let mut it = fields.split('\0');
    while let (Some(code), Some(path)) = (it.next(), it.next()) {
        let s = match code.chars().next() {
            Some('A') => "added",
            Some('D') => "deleted",
            _ => "modified",
        };
        status.insert(path.to_string(), s);
    }

    let text = String::from_utf8_lossy(&numstat.stdout).into_owned();
    let mut records: Vec<&str> = text.split('\0').collect();
    // The last piece is empty after a final NUL, or a torn record when the output was cut.
    records.pop();
    let mut files = Vec::new();
    let mut files_truncated = numstat.truncated;
    for rec in records {
        let mut parts = rec.splitn(3, '\t');
        let (Some(a), Some(r), Some(path)) = (parts.next(), parts.next(), parts.next()) else { continue };
        if files.len() >= MAX_DIFF_FILES {
            files_truncated = true;
            break;
        }
        files.push(DiffFile {
            path: path.to_string(),
            status: status.get(path).copied().unwrap_or("modified"),
            added: a.parse().unwrap_or(0),
            removed: r.parse().unwrap_or(0),
            binary: a == "-" && r == "-",
        });
    }

    let mut bytes = patch.stdout;
    if patch.truncated {
        let keep = bytes.iter().rposition(|b| *b == b'\n').map_or(0, |i| i + 1);
        bytes.truncate(keep);
    }
    Ok(Diff {
        files,
        files_truncated,
        patch: String::from_utf8_lossy(&bytes).into_owned(),
        truncated: patch.truncated,
    })
}

/// One commit of `base..branch`. `message` is the whole message, for the attribution
/// check; `body` is the capped part that is shown.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommitInfo {
    pub id: String,
    pub subject: String,
    pub body: String,
    pub message: String,
    pub author_email: String,
    pub committer_email: String,
}

const SHORT_ID: usize = 7;
const FIELD: char = '\u{1f}';
const RECORD: char = '\u{1e}';

impl CommitInfo {
    pub fn short_id(&self) -> &str {
        &self.id[..SHORT_ID.min(self.id.len())]
    }

    pub fn carries_claude_attribution(&self) -> bool {
        carries_claude_attribution(&self.author_email, &self.committer_email, &self.message)
    }
}

/// Every commit in `base..refs/heads/<branch>`, newest first. Output that does not fit
/// `LIST_CAP` is an error: a commit that was not read cannot be cleared.
pub fn commits(repo: &Path, base: &CommitId, branch: &TaskBranch) -> Result<Vec<CommitInfo>, GitError> {
    let range = format!("{}..refs/heads/{}", base.as_str(), branch.as_str());
    // Flags that make the format the same whatever the user's git config says: signature
    // text, colour, notes and decorations would otherwise land in front of the records.
    let args = [
        "-c",
        "log.showSignature=false",
        "log",
        "--no-show-signature",
        "--no-color",
        "--no-notes",
        "--no-decorate",
        "--encoding=UTF-8",
        "--format=%H%x1f%ae%x1f%ce%x1f%B%x1e",
        &range,
    ];
    let out = run_ok(repo, &args, LIST_CAP, TIMEOUT)?;
    if out.truncated {
        return Err(GitError::Invalid("too many commits to check"));
    }
    let text = String::from_utf8_lossy(&out.stdout).into_owned();
    let mut list: Vec<CommitInfo> = Vec::new();
    for chunk in text.split(RECORD) {
        let chunk = chunk.trim_start_matches('\n');
        if chunk.is_empty() {
            continue;
        }
        let mut f = chunk.splitn(4, FIELD);
        if let (Some(id), Some(ae), Some(ce), Some(msg)) = (f.next(), f.next(), f.next(), f.next()) {
            if CommitId::parse(id).is_ok() {
                let message = msg.trim_end().to_string();
                let (subject, body) = match message.split_once('\n') {
                    Some((s, b)) => (s.to_string(), b.trim().to_string()),
                    None => (message.clone(), String::new()),
                };
                list.push(CommitInfo {
                    id: id.to_string(),
                    subject,
                    body: cap_bytes(&body, BODY_CAP_BYTES),
                    message,
                    author_email: ae.to_string(),
                    committer_email: ce.to_string(),
                });
                continue;
            }
        }
        // Not a record of ours: a message that held the separator. It stays with the
        // commit before it, so the attribution check still reads it.
        if let Some(last) = list.last_mut() {
            last.message.push('\n');
            last.message.push_str(chunk);
        }
    }
    Ok(list)
}

/// `s` cut to at most `max` bytes on a character boundary.
fn cap_bytes(s: &str, max: usize) -> String {
    if s.len() <= max {
        return s.to_string();
    }
    let mut end = max;
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    s[..end].to_string()
}

/// True when a commit carries Claude attribution: a `Co-Authored-By:` line naming Claude or
/// Anthropic, a `Claude-Session:` line, a "Generated with [Claude Code]" line, a link to a
/// Claude Code session, or the `noreply@anthropic.com` address as author, committer or in
/// the message. Case-insensitive. A message that only mentions Claude in prose is clean.
///
/// The list is the owner's rule for every repo (commits are the owner's alone), copied
/// here; nothing reads it from anywhere else.
pub fn carries_claude_attribution(author_email: &str, committer_email: &str, message: &str) -> bool {
    const NOREPLY: &str = "noreply@anthropic.com";
    if author_email.trim().eq_ignore_ascii_case(NOREPLY) || committer_email.trim().eq_ignore_ascii_case(NOREPLY) {
        return true;
    }
    let text = message.to_lowercase();
    if text.contains(NOREPLY) || text.contains("claude.ai/code") || text.contains("claude.com/claude-code") {
        return true;
    }
    text.lines().any(|line| {
        let line = line.trim();
        (key_then_colon(line, "co-authored-by") && (line.contains("claude") || line.contains("anthropic")))
            || key_then_colon(line, "claude-session")
            || generated_with_claude(line)
    })
}

/// `key` starts `line`, then optional blanks, then a colon.
fn key_then_colon(line: &str, key: &str) -> bool {
    line.strip_prefix(key).is_some_and(|rest| rest.trim_start().starts_with(':'))
}

/// "generated with", optional blanks and an optional `[`, then "claude".
fn generated_with_claude(line: &str) -> bool {
    let mut from = 0;
    while let Some(i) = line[from..].find("generated with") {
        let rest = line[from + i + "generated with".len()..].trim_start();
        let rest = rest.strip_prefix('[').map_or(rest, str::trim_start);
        if rest.starts_with("claude") {
            return true;
        }
        from += i + 1;
    }
    false
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

    fn base_of(repo: &Path) -> CommitId {
        repo_info(repo).unwrap().head_commit
    }

    #[test]
    fn diff_is_capped_and_marks_truncated() {
        let repo = repo_with_commit("diffcap");
        let base = base_of(&repo);
        let lines: String = (0..2000).map(|i| format!("line number {i}\n")).collect();
        std::fs::write(repo.join("big.txt"), &lines).unwrap();
        stage_all(&repo).unwrap();

        let capped = diff(&repo, &base, 1000).unwrap();
        assert!(capped.truncated);
        assert!(capped.patch.len() <= 1000);
        assert!(capped.patch.ends_with('\n'), "cut on a line boundary");
        assert!(capped.patch.starts_with("diff --git"));
        // The file list is whole even though the patch was cut.
        assert_eq!(capped.files.len(), 1);
        assert_eq!((capped.files[0].path.as_str(), capped.files[0].added), ("big.txt", 2000));

        let whole = diff(&repo, &base, DIFF_CAP_BYTES).unwrap();
        assert!(!whole.truncated);
        assert!(whole.patch.contains("line number 1999"));
    }

    #[test]
    fn diff_lists_binary_and_untracked_files() {
        let repo = repo_with_commit("diffkinds");
        let base = base_of(&repo);
        std::fs::write(repo.join("new.txt"), "one\ntwo\n").unwrap();
        std::fs::write(repo.join("blob.bin"), [0u8, 1, 2, 0, 255, 0]).unwrap();
        std::fs::write(repo.join("a.txt"), "a changed").unwrap();
        std::fs::create_dir_all(repo.join("dir")).unwrap();
        std::fs::write(repo.join("dir").join("deep file.txt"), "x").unwrap();
        stage_all(&repo).unwrap();

        let d = diff(&repo, &base, DIFF_CAP_BYTES).unwrap();
        let find = |p: &str| d.files.iter().find(|f| f.path == p).unwrap_or_else(|| panic!("{p} not listed"));
        let new = find("new.txt");
        assert_eq!((new.status, new.added, new.removed, new.binary), ("added", 2, 0, false));
        let blob = find("blob.bin");
        assert_eq!((blob.status, blob.binary), ("added", true));
        assert_eq!(find("a.txt").status, "modified");
        assert_eq!(find("dir/deep file.txt").status, "added");
        assert!(!d.patch.contains('\0'));
        assert!(d.patch.contains("Binary files"));

        // A deletion.
        git(&repo, &["commit", "-m", "second"]);
        let mid = base_of(&repo);
        std::fs::remove_file(repo.join("new.txt")).unwrap();
        stage_all(&repo).unwrap();
        let d = diff(&repo, &mid, DIFF_CAP_BYTES).unwrap();
        assert_eq!(d.files.len(), 1);
        assert_eq!((d.files[0].status, d.files[0].removed), ("deleted", 2));
    }

    #[test]
    fn commit_merge_and_branch_delete_roundtrip() {
        let repo = repo_with_commit("mergeflow");
        git(&repo, &["config", "user.name", "owner"]);
        git(&repo, &["config", "user.email", "owner@example.invalid"]);
        let info = repo_info(&repo).unwrap();
        let base = scratch("mergeflow-base");
        let branch = TaskBranch::parse("orchestra/t1").unwrap();
        let wt = base.join("p1").join("t1");
        worktree_add(&repo, &base, &wt, &branch, &info.head_commit).unwrap();

        std::fs::write(wt.join("b.txt"), "b").unwrap();
        stage_all(&wt).unwrap();
        assert!(has_staged(&wt).unwrap());
        commit(&wt, "t1: add b").unwrap();
        assert!(!has_staged(&wt).unwrap());
        assert_eq!(commits_ahead(&repo, &info.head_commit, &branch).unwrap(), 1);
        let list = commits(&repo, &info.head_commit, &branch).unwrap();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].subject, "t1: add b");
        assert_eq!(list[0].author_email, "owner@example.invalid");
        assert!(!list[0].carries_claude_attribution());

        assert!(is_clean_tracked(&repo).unwrap());
        assert_eq!(merge(&repo, &branch, "merge orchestra task t1: add b").unwrap(), MergeOutcome::Merged);
        let parents = git(&repo, &["rev-list", "--parents", "-n", "1", "HEAD"]).text();
        assert_eq!(parents.split_whitespace().count(), 3, "a merge commit, not a fast-forward");

        worktree_remove(&repo, &base, &wt).unwrap();
        branch_delete(&repo, &branch, false).unwrap();
        assert!(!branch_exists(&repo, &branch).unwrap());
    }

    #[test]
    fn unmerged_branch_needs_force_to_delete() {
        let repo = repo_with_commit("forcedel");
        git(&repo, &["config", "user.name", "owner"]);
        git(&repo, &["config", "user.email", "owner@example.invalid"]);
        let info = repo_info(&repo).unwrap();
        let base = scratch("forcedel-base");
        let branch = TaskBranch::parse("orchestra/t1").unwrap();
        let wt = base.join("p1").join("t1");
        worktree_add(&repo, &base, &wt, &branch, &info.head_commit).unwrap();
        std::fs::write(wt.join("b.txt"), "b").unwrap();
        stage_all(&wt).unwrap();
        commit(&wt, "t1: add b").unwrap();
        worktree_remove(&repo, &base, &wt).unwrap();
        assert!(branch_delete(&repo, &branch, false).is_err());
        branch_delete(&repo, &branch, true).unwrap();
    }

    #[test]
    fn remove_empty_dir_is_not_recursive_and_stays_inside_base() {
        let base = scratch("emptydir");
        let full = base.join("p").join("full");
        std::fs::create_dir_all(&full).unwrap();
        std::fs::write(full.join("keep.txt"), "k").unwrap();
        assert!(remove_empty_dir(&base, &full).is_err());
        assert!(full.join("keep.txt").is_file());
        std::fs::remove_file(full.join("keep.txt")).unwrap();
        remove_empty_dir(&base, &full).unwrap();
        assert!(!full.exists());

        let outside = scratch("emptydir-outside");
        assert!(remove_empty_dir(&base, &outside).is_err());
        assert!(outside.is_dir(), "a folder outside the base is left alone");
        assert!(remove_empty_dir(&base, &base).is_err());
    }

    /// `mklink /J`: a directory junction needs no privilege. Test code only.
    #[cfg(windows)]
    fn junction(link: &Path, target: &Path) {
        let st = Command::new("cmd")
            .args(["/C", "mklink", "/J"])
            .arg(link)
            .arg(target)
            .stdout(Stdio::null())
            .status()
            .unwrap();
        assert!(st.success(), "mklink failed");
    }

    /// Remove a junction itself; `remove_dir` on a link never touches the target.
    #[cfg(windows)]
    fn drop_junction(link: &Path) {
        if std::fs::symlink_metadata(link).is_ok() {
            std::fs::remove_dir(link).unwrap();
        }
    }

    /// A repo, a task worktree under a base, and a victim dir outside with `keep.txt`.
    #[cfg(windows)]
    fn worktree_fixture(name: &str) -> (PathBuf, PathBuf, PathBuf, PathBuf) {
        let repo = repo_with_commit(name);
        let base = scratch(&format!("{name}-base"));
        let victim = scratch(&format!("{name}-victim"));
        std::fs::write(victim.join("keep.txt"), "keep").unwrap();
        let info = repo_info(&repo).unwrap();
        let branch = TaskBranch::parse("orchestra/t1").unwrap();
        let path = base.join("p1").join("t1");
        worktree_add(&repo, &base, &path, &branch, &info.head_commit).unwrap();
        (repo, base, path, victim)
    }

    #[cfg(windows)]
    #[test]
    fn worktree_remove_unlinks_a_junction_and_spares_its_target() {
        let (repo, base, path, victim) = worktree_fixture("jn");
        junction(&path.join("nm"), &victim);
        let res = worktree_remove(&repo, &base, &path);
        if res.is_err() {
            drop_junction(&path.join("nm"));
        }
        res.unwrap();
        assert!(victim.join("keep.txt").is_file(), "the junction target was deleted");
        assert!(!path.exists());
        assert!(!git(&repo, &["worktree", "list", "--porcelain"]).text().contains("t1"));
    }

    #[cfg(windows)]
    #[test]
    fn worktree_remove_unlinks_a_nested_junction() {
        let (repo, base, path, victim) = worktree_fixture("jn-deep");
        let deep = path.join("a").join("b");
        std::fs::create_dir_all(&deep).unwrap();
        junction(&deep.join("nm"), &victim);
        let res = worktree_remove(&repo, &base, &path);
        if res.is_err() {
            drop_junction(&deep.join("nm"));
        }
        res.unwrap();
        assert!(victim.join("keep.txt").is_file(), "the junction target was deleted");
        assert!(!path.exists());
    }

    #[cfg(windows)]
    #[test]
    fn worktree_remove_with_a_stuck_link_errors_before_git() {
        let (repo, base, path, victim) = worktree_fixture("jn-stuck");
        junction(&path.join("nm"), &victim);
        let calls = std::cell::Cell::new(0);
        let stuck = |_: &Path, _: bool| {
            calls.set(calls.get() + 1);
            Err(std::io::Error::other("stuck"))
        };
        let res = worktree_remove_with(&repo, &base, &path, &stuck);
        assert!(res.is_err());
        assert_eq!(calls.get(), 1);
        assert!(victim.join("keep.txt").is_file());
        assert!(path.join("a.txt").is_file(), "git ran although a link remained");
        assert!(git(&repo, &["worktree", "list", "--porcelain"]).text().contains("t1"));
        // Now the real thing works.
        worktree_remove(&repo, &base, &path).unwrap();
        assert!(victim.join("keep.txt").is_file());
        assert!(!path.exists());
    }

    #[test]
    fn scrub_env_removes_every_repo_selecting_variable() {
        let mut cmd = Command::new(GIT);
        scrub_env(&mut cmd);
        for name in SCRUBBED_ENV {
            let hit = cmd.get_envs().find(|(k, _)| *k == std::ffi::OsStr::new(name));
            assert!(matches!(hit, Some((_, None))), "{name} is not removed");
        }
        for must in ["GIT_DIR", "GIT_WORK_TREE", "GIT_INDEX_FILE"] {
            assert!(SCRUBBED_ENV.contains(&must));
        }
    }

    #[test]
    fn inherited_git_dir_does_not_redirect_git() {
        let repo = repo_with_commit("envdir");
        let other = scratch("envdir-other");
        // Every git call in this module scrubs it, so setting it cannot disturb other tests.
        unsafe { std::env::set_var("GIT_DIR", &other) };
        let info = repo_info(&repo);
        unsafe { std::env::remove_var("GIT_DIR") };
        assert_eq!(info.unwrap().head_branch.as_str(), "main");
    }

    #[test]
    fn attribution_detector_flags_each_pattern() {
        let cases = [
            "fix\n\nCo-Authored-By: Claude <x@y.z>",
            "fix\n\nco-authored-by: Claude Sonnet 5.5 <someone@example.org>",
            "fix\n\nCO-AUTHORED-BY : someone at Anthropic",
            "fix\n\nClaude-Session: abc",
            "fix\n\nclaude-session : abc",
            "fix\n\nGenerated with [Claude Code]",
            "fix\n\ngenerated with Claude Code",
            "fix\n\nGENERATED WITH [ CLAUDE CODE ]",
            "fix\n\nhttps://claude.ai/code/session_1",
            "fix\n\nsee HTTPS://CLAUDE.AI/CODE/x",
            "fix\n\nhttps://claude.com/claude-code",
            "fix\n\nsent by Noreply@Anthropic.com",
        ];
        for msg in cases {
            assert!(carries_claude_attribution("a@b.c", "a@b.c", msg), "{msg:?}");
        }
        assert!(carries_claude_attribution("noreply@anthropic.com", "a@b.c", "plain"));
        assert!(carries_claude_attribution("a@b.c", "NoReply@Anthropic.com", "plain"));
    }

    #[test]
    fn attribution_detector_ignores_prose_and_human_coauthors() {
        let cases = [
            "add a claude adapter",
            "Claude handled the retry; I only reworded it",
            "document how generated code is reviewed",
            "Co-Authored-By: Ada Lovelace <ada@example.org>",
            "fix\n\nthis was generated with a script",
            "the claude-session file is read by the importer",
            "mention of anthropic in prose, co-authored discussions aside",
            "",
        ];
        for msg in cases {
            assert!(!carries_claude_attribution("owner@example.org", "owner@example.org", msg), "{msg:?}");
        }
    }

    #[test]
    fn commits_survive_a_signature_showing_config() {
        let repo = repo_with_commit("sig");
        git(&repo, &["config", "user.name", "owner"]);
        git(&repo, &["config", "user.email", "owner@example.invalid"]);
        let info = repo_info(&repo).unwrap();
        let key = scratch("sig-key").join("k");
        let st = Command::new("ssh-keygen").args(["-q", "-t", "ed25519", "-N", "", "-f"]).arg(&key).status().unwrap();
        assert!(st.success());
        let pubkey = format!("{}.pub", key.display());
        for (k, v) in [
            ("gpg.format", "ssh"),
            ("user.signingkey", pubkey.as_str()),
            ("commit.gpgsign", "true"),
            ("log.showSignature", "true"),
        ] {
            git(&repo, &["config", k, v]);
        }
        let branch = TaskBranch::parse("orchestra/t1").unwrap();
        let base = scratch("sig-base");
        let wt = base.join("p1").join("t1");
        worktree_add(&repo, &base, &wt, &branch, &info.head_commit).unwrap();
        std::fs::write(wt.join("b.txt"), "b").unwrap();
        stage_all(&wt).unwrap();
        commit(&wt, "t1: x\n\nClaude-Session: abc").unwrap();
        // The plain log of this config puts signature text in front of the record.
        let range = format!("{}..refs/heads/orchestra/t1", info.head_commit.as_str());
        let plain = git(&repo, &["log", "--format=%H", &range]).text();
        assert!(plain.len() > 40, "the config shows signatures: {plain}");
        let list = commits(&repo, &info.head_commit, &branch).unwrap();
        assert_eq!(list.len(), 1);
        assert!(list[0].carries_claude_attribution());
    }

    #[test]
    fn merge_takes_the_branch_not_a_tag_of_the_same_name() {
        let repo = repo_with_commit("tagmerge");
        git(&repo, &["config", "user.name", "owner"]);
        git(&repo, &["config", "user.email", "owner@example.invalid"]);
        let info = repo_info(&repo).unwrap();
        let branch = TaskBranch::parse("orchestra/t1").unwrap();
        let base = scratch("tagmerge-base");
        let wt = base.join("p1").join("t1");
        worktree_add(&repo, &base, &wt, &branch, &info.head_commit).unwrap();
        std::fs::write(wt.join("b.txt"), "b").unwrap();
        stage_all(&wt).unwrap();
        commit(&wt, "t1: x").unwrap();
        std::fs::write(repo.join("c.txt"), "c").unwrap();
        git(&repo, &["add", "."]);
        git(&repo, &["commit", "-m", "other"]);
        git(&repo, &["tag", "orchestra/t1", "HEAD"]);
        assert_eq!(merge(&repo, &branch, "m").unwrap(), MergeOutcome::Merged);
        assert!(repo.join("b.txt").is_file(), "the branch was merged, not the tag");
    }

    #[test]
    fn commits_reads_the_whole_message_for_the_check() {
        let repo = repo_with_commit("longmsg");
        git(&repo, &["config", "user.name", "owner"]);
        git(&repo, &["config", "user.email", "owner@example.invalid"]);
        let info = repo_info(&repo).unwrap();
        let base = scratch("longmsg-base");
        let branch = TaskBranch::parse("orchestra/t1").unwrap();
        let wt = base.join("p1").join("t1");
        worktree_add(&repo, &base, &wt, &branch, &info.head_commit).unwrap();
        std::fs::write(wt.join("b.txt"), "b").unwrap();
        stage_all(&wt).unwrap();
        // A body longer than the display cap, with the trailer after the cap.
        let message = format!("t1: x\n\n{}\n\nClaude-Session: abc", "word ".repeat(2000));
        commit(&wt, &message).unwrap();
        let list = commits(&repo, &info.head_commit, &branch).unwrap();
        assert_eq!(list.len(), 1);
        assert!(list[0].body.len() <= BODY_CAP_BYTES);
        assert!(!list[0].body.contains("Claude-Session"), "cut off for display");
        assert!(list[0].carries_claude_attribution(), "but still found");
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
