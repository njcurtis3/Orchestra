//! Session manager: owns projects and sessions, validates every input, and glues the
//! pure state machine (`state`) to the child process (`process`). State lives in memory and
//! is mirrored to disk through `store` after every change.

pub mod process;
pub mod protocol;
pub mod settings;
pub mod state;
pub mod store;
pub mod task;
pub mod transcript;

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard, Weak};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::Serialize;
use serde_json::Value;

use crate::git;
use crate::notify::{self, Notice};
use crate::projects::{Project, Projects};
use process::{Output, Proc, SpawnArgs};
use settings::{Model, PermissionMode, SessionSettings};
use state::{Commit, Decision, InboxItem, SessionState, SessionStatus};
use store::{Snapshot, StoredSession, Writer};
use task::{MAX_RUNNING_TASKS, Task, TaskStatus, TaskSummary};
use transcript::{Entry, Record, TranscriptBatch, TranscriptPage, TranscriptStore};

/// Largest prompt, reply or permission message accepted over IPC, in bytes.
pub const MAX_TEXT_BYTES: usize = 32768;
const MAX_ANSWERS: usize = 32;
const MAX_ANSWER_BYTES: usize = 4096;
/// How long a retired child gets to release its folder before the queue moves on anyway.
const CHILD_EXIT_WAIT: Duration = Duration::from_secs(10);

#[derive(Debug, Clone, Serialize)]
pub struct SessionSummary {
    pub id: String,
    pub project_id: String,
    pub project_name: String,
    pub status: SessionStatus,
    pub started_at: u64,
    pub last_activity_at: u64,
    pub last_message: Option<String>,
    pub claude_session_id: Option<String>,
    pub model: Option<Model>,
    pub permission_mode: Option<PermissionMode>,
}

#[derive(Debug, Clone)]
pub enum Event {
    SessionUpdated(SessionSummary),
    InboxUpdated(Vec<InboxItem>),
    TranscriptAppended(TranscriptBatch),
    TaskUpdated(TaskSummary),
}

pub type Emit = Box<dyn Fn(Event) + Send + Sync>;
pub type Sink = Box<dyn Fn(Output) + Send>;
/// Called with the project name and a notice after a change worth telling the user about.
/// Runs on the reader thread with no lock held, and must not call back into `Sessions`.
pub type Notify = Box<dyn Fn(&str, Notice) + Send + Sync>;
/// Starts a child in the given directory with the session's settings, resuming a saved
/// conversation when given its id; every stdout line and the exit go to the sink.
pub type Spawner = Box<dyn Fn(&Path, &SpawnArgs, Sink) -> Result<Arc<dyn Proc>, String> + Send + Sync>;

/// What decides whether a stdout line needs a snapshot: a change in any of these is worth a
/// write, a change in only the message text or the clock is not.
#[derive(Clone, PartialEq)]
struct SaveKey {
    status: SessionStatus,
    inbox_rev: u64,
    claude_session_id: Option<String>,
}

impl SaveKey {
    fn of(state: &SessionState) -> Self {
        Self { status: state.status, inbox_rev: state.inbox_rev, claude_session_id: state.claude_session_id.clone() }
    }
}

struct Session {
    id: String,
    project_id: String,
    project_name: String,
    started_at: u64,
    settings: SessionSettings,
    state: SessionState,
    /// The inbox revision last sent to the frontend.
    emitted_inbox_rev: u64,
    /// The key as of the last snapshot decision.
    saved_key: SaveKey,
    proc: Option<Arc<dyn Proc>>,
    /// The task this session belongs to; `None` for a plain session.
    task_id: Option<String>,
    /// Bumped whenever a process is retired or replaced, so a late line or exit from the
    /// old one is ignored.
    spawn_gen: u64,
    /// Serialises build, write and commit of answers for this session. Held across the
    /// pipe write, so it is never taken together with `Inner` and the reader never needs it.
    gate: Arc<Mutex<()>>,
}

impl Session {
    fn summary(&self) -> SessionSummary {
        SessionSummary {
            id: self.id.clone(),
            project_id: self.project_id.clone(),
            project_name: self.project_name.clone(),
            status: self.state.status,
            started_at: self.started_at,
            last_activity_at: self.state.last_activity_at,
            last_message: self.state.last_message.clone(),
            claude_session_id: self.state.claude_session_id.clone(),
            model: self.settings.model,
            permission_mode: self.settings.permission_mode,
        }
    }
}

#[derive(Default)]
struct Inner {
    projects: Projects,
    sessions: HashMap<String, Session>,
    order: Vec<String>,
    next_session: u64,
    tasks: Vec<Task>,
    next_task: u64,
    /// Bumped for every snapshot computed, so the writer can tell newest from older.
    revision: u64,
    /// Set by `shutdown`: from then on the readers change nothing and nothing is saved.
    shutting_down: bool,
}

impl Inner {
    fn inbox(&self) -> Vec<InboxItem> {
        let mut items: Vec<InboxItem> = self
            .order
            .iter()
            .filter_map(|id| self.sessions.get(id))
            .flat_map(|s| s.state.inbox.iter().cloned())
            .collect();
        items.sort_by_key(|i| i.created_at);
        items
    }

    /// The one place a session's working folder comes from: the project for a plain
    /// session, the task's worktree for a task session. Checked with `is_dir` each time, so
    /// a folder that vanished is a refusal here and not a spawn failure.
    fn cwd_of(&self, session: &Session) -> Result<PathBuf, CwdError> {
        let dir = match &session.task_id {
            Some(tid) => {
                let task = self.tasks.iter().find(|t| &t.id == tid).ok_or(CwdError::Worktree)?;
                task.worktree.clone().ok_or(CwdError::Worktree)?
            }
            None => self.projects.get(&session.project_id).ok_or(CwdError::Project)?.path.clone(),
        };
        match (dir.is_dir(), session.task_id.is_some()) {
            (true, _) => Ok(dir),
            (false, true) => Err(CwdError::Worktree),
            (false, false) => Err(CwdError::Project),
        }
    }

    /// Follow the session's status onto its task, if it has one.
    fn sync_task(&mut self, session_id: &str) {
        let Some(s) = self.sessions.get(session_id) else { return };
        let Some(tid) = s.task_id.clone() else { return };
        let (status, errored) = (s.state.status, task::last_turn_is_error(&s.state.inbox));
        if let Some(t) = self.tasks.iter_mut().find(|t| t.id == tid) {
            task::sync(t, status, errored);
        }
    }

    /// The task of this session is running or waiting on the owner.
    fn task_in_flight(&self, session_id: &str) -> bool {
        self.sessions
            .get(session_id)
            .and_then(|s| s.task_id.as_ref())
            .and_then(|tid| self.tasks.iter().find(|t| &t.id == tid))
            .is_some_and(|t| matches!(t.status, TaskStatus::Running | TaskStatus::NeedsInput))
    }

    /// Tasks holding a slot of `MAX_RUNNING_TASKS`.
    fn active_tasks(&self) -> usize {
        self.tasks
            .iter()
            .filter(|t| {
                let suspended = t
                    .session_id
                    .as_ref()
                    .and_then(|sid| self.sessions.get(sid))
                    .is_some_and(|s| s.state.status == SessionStatus::Suspended);
                t.is_active(suspended)
            })
            .count()
    }

    fn snapshot(&self) -> Snapshot {
        Snapshot {
            schema: store::SCHEMA,
            revision: self.revision,
            next_session: self.next_session,
            next_project: self.projects.counter(),
            projects: self.projects.to_stored(),
            sessions: self
                .order
                .iter()
                .filter_map(|id| self.sessions.get(id))
                .map(|s| StoredSession {
                    id: s.id.clone(),
                    project_id: s.project_id.clone(),
                    project_name: s.project_name.clone(),
                    started_at: s.started_at,
                    model: s.settings.model.map(|m| m.as_str().to_string()),
                    permission_mode: s.settings.permission_mode.map(|m| m.as_str().to_string()),
                    task_id: s.task_id.clone(),
                    state: s.state.to_persisted(),
                })
                .collect(),
            next_task: self.next_task,
            tasks: self.tasks.iter().map(Task::to_stored).collect(),
        }
    }

    /// The events for a session, and whether its status, inbox or claude id changed since the
    /// last call. The inbox event carries only this session's items and only when its inbox
    /// changed.
    fn events_for(&mut self, session_id: &str) -> (Vec<Event>, bool) {
        self.sync_task(session_id);
        let Some(s) = self.sessions.get_mut(session_id) else {
            return (Vec::new(), false);
        };
        let mut events = vec![Event::SessionUpdated(s.summary())];
        if s.emitted_inbox_rev != s.state.inbox_rev {
            s.emitted_inbox_rev = s.state.inbox_rev;
            events.push(Event::InboxUpdated(s.state.inbox.clone()));
        }
        let key = SaveKey::of(&s.state);
        let significant = s.saved_key != key;
        s.saved_key = key;
        let task_id = s.task_id.clone();
        if let Some(t) = task_id.and_then(|tid| self.tasks.iter().find(|t| t.id == tid)) {
            events.push(Event::TaskUpdated(t.summary()));
        }
        (events, significant)
    }

    fn is_expired_item(&self, item_id: &str) -> bool {
        self.sessions.values().any(|s| s.state.inbox.iter().any(|i| i.id == item_id && i.expired))
    }

    /// The session holding an unresolved request with this item id.
    fn session_of_item(&self, item_id: &str) -> Option<String> {
        self.sessions.values().find(|s| s.state.has_pending(item_id)).map(|s| s.id.clone())
    }
}

/// What the pump needs to start one claimed task, copied out so no lock is held across git.
struct TaskClaim {
    task_id: String,
    project_id: String,
    prompt: String,
    settings: SessionSettings,
    base_commit: String,
    repo: PathBuf,
}

/// Why a session has no working folder to spawn in.
enum CwdError {
    Project,
    Worktree,
}

impl CwdError {
    fn message(&self) -> &'static str {
        match self {
            CwdError::Project => "the project folder no longer exists",
            CwdError::Worktree => "the task worktree no longer exists",
        }
    }
}

pub fn now_ms() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
}

pub struct Sessions {
    inner: Mutex<Inner>,
    /// Makes publishing atomic in compute order. Lock order is always `inner` then
    /// `emit_order`. The `emit` callback runs under it and must never call back into
    /// `Sessions`.
    emit_order: Mutex<()>,
    emit: Emit,
    spawner: Spawner,
    notify: Notify,
    /// `None` when saving is off: tests, or a state file this build must not touch.
    writer: Option<Writer>,
    transcripts: TranscriptStore,
    /// Every task worktree lives under this folder, in `<project id>/<task id>`.
    worktree_base: PathBuf,
    /// Lets `&self` methods hand an owning handle to a reader thread.
    me: Weak<Sessions>,
}

impl Sessions {
    pub fn with_writer(
        emit: Emit,
        spawner: Spawner,
        notify: Notify,
        writer: Option<Writer>,
        transcripts: TranscriptStore,
        worktree_base: PathBuf,
    ) -> Arc<Self> {
        Arc::new_cyclic(|me| Self {
            inner: Mutex::new(Inner::default()),
            emit_order: Mutex::new(()),
            emit,
            spawner,
            notify,
            writer,
            transcripts,
            worktree_base,
            me: me.clone(),
        })
    }

    /// Load what the state file held. Every session that was live is suspended: its process
    /// is gone, and any request it had open can no longer be answered. Nothing is spawned.
    pub fn restore(&self, snapshot: Snapshot) {
        let mut inner = self.lock();
        inner.revision = snapshot.revision;
        inner.projects = Projects::restore(snapshot.projects, snapshot.next_project);
        let mut next_session = snapshot.next_session;
        for stored in snapshot.sessions {
            if inner.sessions.contains_key(&stored.id) {
                continue;
            }
            if let Some(n) = stored.id.strip_prefix("s").and_then(|n| n.parse::<u64>().ok()) {
                next_session = next_session.max(n);
            }
            let mut state = SessionState::from_persisted(&stored.id, stored.state);
            // Closing is not activity, so the last-activity time is kept.
            let at = state.last_activity_at;
            state.suspend(at);
            inner.order.push(stored.id.clone());
            inner.sessions.insert(
                stored.id.clone(),
                Session {
                    id: stored.id,
                    project_id: stored.project_id,
                    project_name: stored.project_name,
                    started_at: stored.started_at,
                    settings: SessionSettings::from_stored(stored.model.as_deref(), stored.permission_mode.as_deref()),
                    emitted_inbox_rev: state.inbox_rev,
                    saved_key: SaveKey::of(&state),
                    state,
                    proc: None,
                    task_id: stored.task_id,
                    spawn_gen: 0,
                    gate: Arc::new(Mutex::new(())),
                },
            );
        }
        inner.next_session = next_session;

        let mut next_task = snapshot.next_task;
        for stored in snapshot.tasks {
            if inner.tasks.iter().any(|t| t.id == stored.id) {
                continue;
            }
            if let Some(n) = stored.id.strip_prefix('t').and_then(|n| n.parse::<u64>().ok()) {
                next_task = next_task.max(n);
            }
            let mut task = stored.into_task();
            self.recheck_task(&mut task);
            inner.tasks.push(task);
        }
        inner.next_task = next_task;
        // A task whose session cannot be resumed is done, not waiting on anyone.
        let linked: Vec<String> = inner.sessions.values().filter(|s| s.task_id.is_some()).map(|s| s.id.clone()).collect();
        for sid in linked {
            inner.sync_task(&sid);
        }
    }

    /// A task read from the file: a live one comes back paused, and its worktree is checked
    /// again, because the file is only a record. A path outside the worktree folder is
    /// dropped and never used; a missing folder is an error. Nothing is spawned.
    fn recheck_task(&self, task: &mut Task) {
        if matches!(task.status, TaskStatus::Running | TaskStatus::NeedsInput) {
            task.status = TaskStatus::NeedsInput;
            task.note = Some(task::PAUSED_NOTE.to_string());
        }
        let needs_worktree =
            matches!(task.status, TaskStatus::NeedsInput | TaskStatus::Done | TaskStatus::Landing);
        let Some(path) = task.worktree.clone() else {
            if needs_worktree {
                task.fail("worktree missing");
            }
            return;
        };
        let inside = git::is_within(&self.worktree_base, &path);
        if !inside {
            task.worktree = None;
        }
        if !needs_worktree {
            return;
        }
        if !path.is_dir() {
            task.fail("worktree missing");
        } else if !inside {
            task.fail("worktree is outside the worktree folder");
        }
    }

    fn lock(&self) -> MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Compute the events for a session under `inner`, take the emit lock before letting
    /// `inner` go, then fire. Two publishers therefore fire in the order they computed.
    ///
    /// The disk follows the same shape: the snapshot is computed under `inner` with the
    /// next revision and queued under the emit lock, so the writer sees snapshots in
    /// revision order and `inner` is never held across disk I/O.
    ///
    /// An explicit command always saves; a stdout line goes through `publish_line`.
    fn publish(&self, inner: MutexGuard<'_, Inner>, session_id: &str) {
        self.publish_saving(inner, session_id, true, Vec::new());
    }

    /// `publish` for a stdout line: the snapshot is skipped when only text changed.
    fn publish_line(&self, inner: MutexGuard<'_, Inner>, session_id: &str, appended: Vec<Record>) {
        self.publish_saving(inner, session_id, false, self.transcript_events(session_id, appended));
    }

    /// A transcript batch to send with the session's events; nothing when none was recorded.
    fn transcript_events(&self, session_id: &str, entries: Vec<Record>) -> Vec<Event> {
        if entries.is_empty() {
            return Vec::new();
        }
        vec![Event::TranscriptAppended(TranscriptBatch { session_id: session_id.to_string(), entries })]
    }

    fn publish_saving(&self, mut inner: MutexGuard<'_, Inner>, session_id: &str, force: bool, extra: Vec<Event>) {
        let (mut events, significant) = inner.events_for(session_id);
        events.extend(extra);
        let snapshot = if force || significant {
            self.next_snapshot(&mut inner)
        } else {
            // Text only: the next snapshot of any kind carries it, and `shutdown` always
            // snapshots, so a clean exit never loses it.
            None
        };
        let order = self.emit_order.lock().unwrap_or_else(|e| e.into_inner());
        drop(inner);
        self.queue_save(snapshot);
        for e in events {
            (self.emit)(e);
        }
        drop(order);
    }

    /// Save without an event, for a change the frontend is not told about.
    fn save_state(&self, mut inner: MutexGuard<'_, Inner>) {
        let snapshot = self.next_snapshot(&mut inner);
        let order = self.emit_order.lock().unwrap_or_else(|e| e.into_inner());
        drop(inner);
        self.queue_save(snapshot);
        drop(order);
    }

    fn next_snapshot(&self, inner: &mut Inner) -> Option<Snapshot> {
        if self.writer.is_none() || inner.shutting_down {
            return None;
        }
        inner.revision += 1;
        Some(inner.snapshot())
    }

    fn queue_save(&self, snapshot: Option<Snapshot>) {
        if let (Some(w), Some(s)) = (&self.writer, snapshot) {
            w.save(s);
        }
    }

    pub fn add_project(&self, path: &Path) -> Result<Project, String> {
        let mut inner = self.lock();
        let project = inner.projects.add(path)?;
        self.save_state(inner);
        Ok(project)
    }

    pub fn list_projects(&self) -> Vec<Project> {
        self.lock().projects.list()
    }

    pub fn list_sessions(&self) -> Vec<SessionSummary> {
        let inner = self.lock();
        inner.order.iter().filter_map(|id| inner.sessions.get(id)).map(Session::summary).collect()
    }

    pub fn list_inbox(&self) -> Vec<InboxItem> {
        self.lock().inbox()
    }

    pub fn list_tasks(&self) -> Vec<TaskSummary> {
        self.lock().tasks.iter().map(Task::summary).collect()
    }

    /// Save and announce a task that changed without a session event.
    fn publish_task(&self, mut inner: MutexGuard<'_, Inner>, task_id: &str) {
        let event = inner.tasks.iter().find(|t| t.id == task_id).map(|t| Event::TaskUpdated(t.summary()));
        let snapshot = self.next_snapshot(&mut inner);
        let order = self.emit_order.lock().unwrap_or_else(|e| e.into_inner());
        drop(inner);
        self.queue_save(snapshot);
        if let Some(e) = event {
            (self.emit)(e);
        }
        drop(order);
    }

    /// Queue a task. Git must be present and the project a repository checked out on a
    /// branch; otherwise nothing is queued. The task starts at once when a slot is free.
    pub fn create_task(
        self: &Arc<Self>,
        project_id: &str,
        prompt: &str,
        settings: SessionSettings,
    ) -> Result<TaskSummary, String> {
        check_text(prompt)?;
        git::check().map_err(|e| e.message().to_string())?;
        let (path, name) = {
            let inner = self.lock();
            let entry = inner.projects.get(project_id).ok_or("unknown project")?;
            (entry.path.clone(), entry.project.name.clone())
        };
        // Git runs with no lock held.
        let info = git::repo_info(&path).map_err(|e| match e {
            git::GitError::Failed(_) => "the project is not a git repository".to_string(),
            other => other.message().to_string(),
        })?;
        let id = {
            let mut inner = self.lock();
            inner.next_task += 1;
            let id = format!("t{}", inner.next_task);
            inner.tasks.push(Task::new(
                id.clone(),
                project_id.to_string(),
                name,
                prompt.to_string(),
                settings,
                info.head_branch.as_str().to_string(),
                info.head_commit.as_str().to_string(),
                now_ms(),
            ));
            self.publish_task(inner, &id);
            id
        };
        self.pump();
        self.lock().tasks.iter().find(|t| t.id == id).map(Task::summary).ok_or_else(|| "unknown task".to_string())
    }

    /// Cancel a task that has not started: it has no worktree, so there is nothing to clean.
    pub fn cancel_task(self: &Arc<Self>, task_id: &str) -> Result<(), String> {
        let mut inner = self.lock();
        let t = inner.tasks.iter_mut().find(|t| t.id == task_id).ok_or("unknown task")?;
        if t.status != TaskStatus::Queued || t.starting {
            return Err("only a task that has not started can be cancelled".into());
        }
        t.status = TaskStatus::Discarded;
        t.note = Some("Cancelled before it started".to_string());
        self.publish_task(inner, task_id);
        self.pump_in_background();
        Ok(())
    }

    /// Start the oldest queued tasks while fewer than `MAX_RUNNING_TASKS` hold a slot. Safe
    /// to call from any thread but a reader: it runs git and spawns children.
    pub fn pump(self: &Arc<Self>) {
        while let Some(claim) = self.claim_next() {
            self.start_task(claim);
        }
    }

    /// `pump` on a thread of its own.
    pub fn pump_in_background(&self) {
        if let Some(this) = self.me.upgrade() {
            std::thread::spawn(move || this.pump());
        }
    }

    /// The next queued task, marked as starting so that no other pump takes it and the
    /// limit counts it. A task whose project is gone goes to error and the next is tried.
    fn claim_next(&self) -> Option<TaskClaim> {
        loop {
            let mut inner = self.lock();
            if inner.shutting_down || inner.active_tasks() >= MAX_RUNNING_TASKS {
                return None;
            }
            let idx = inner.tasks.iter().position(|t| t.status == TaskStatus::Queued && !t.starting)?;
            let project_id = inner.tasks[idx].project_id.clone();
            let Some(repo) = inner.projects.get(&project_id).map(|e| e.path.clone()) else {
                let t = &mut inner.tasks[idx];
                t.fail("The project folder no longer exists");
                let id = t.id.clone();
                self.publish_task(inner, &id);
                continue;
            };
            let t = &mut inner.tasks[idx];
            t.starting = true;
            return Some(TaskClaim {
                task_id: t.id.clone(),
                project_id,
                prompt: t.prompt.clone(),
                settings: t.settings,
                base_commit: t.base_commit.clone(),
                repo,
            });
        }
    }

    /// Make the worktree and branch, then start the task's session in it. A task that
    /// cannot get either goes to error with a fixed note and the queue moves on.
    fn start_task(self: &Arc<Self>, claim: TaskClaim) {
        let worktree = self.worktree_base.join(&claim.project_id).join(&claim.task_id);
        let branch = match self.make_worktree(&claim, &worktree) {
            Ok(b) => b,
            Err(note) => return self.fail_task(&claim.task_id, note),
        };
        {
            let mut inner = self.lock();
            if let Some(t) = inner.tasks.iter_mut().find(|t| t.id == claim.task_id) {
                t.worktree = Some(worktree);
                t.branch = Some(branch);
            }
        }
        if self.start_in(&claim.project_id, &claim.prompt, claim.settings, Some(&claim.task_id)).is_err() {
            self.fail_task(&claim.task_id, "The task session could not start");
        }
    }

    /// `orchestra/<task id>`, or the first free `-2` to `-9` when that branch exists.
    fn make_worktree(&self, claim: &TaskClaim, worktree: &Path) -> Result<String, &'static str> {
        const FAILED: &str = "The worktree could not be created";
        let commit = git::CommitId::parse(&claim.base_commit).map_err(|_| FAILED)?;
        let mut free = None;
        for n in 1..=9 {
            let name = if n == 1 {
                format!("orchestra/{}", claim.task_id)
            } else {
                format!("orchestra/{}-{n}", claim.task_id)
            };
            let branch = git::TaskBranch::parse(&name).map_err(|_| FAILED)?;
            if !git::branch_exists(&claim.repo, &branch).map_err(|_| FAILED)? {
                free = Some(branch);
                break;
            }
        }
        let branch = free.ok_or("No free branch name for this task")?;
        git::worktree_add(&claim.repo, &self.worktree_base, worktree, &branch, &commit).map_err(|_| FAILED)?;
        Ok(branch.as_str().to_string())
    }

    fn fail_task(&self, task_id: &str, note: &str) {
        let mut inner = self.lock();
        let Some(t) = inner.tasks.iter_mut().find(|t| t.id == task_id) else { return };
        t.fail(note);
        t.session_id = None;
        self.publish_task(inner, task_id);
    }

    /// Record entries for a session that exists. The session map is checked first, so a file
    /// name is only ever made from an id this manager issued.
    fn append_transcript(&self, session_id: &str, entries: Vec<Entry>) -> Vec<Record> {
        if !self.lock().sessions.contains_key(session_id) {
            return Vec::new();
        }
        self.transcripts.append(session_id, entries, now_ms())
    }

    /// The last entries of a session's transcript.
    pub fn transcript(&self, session_id: &str) -> Result<TranscriptPage, String> {
        if !self.lock().sessions.contains_key(session_id) {
            return Err("unknown session".into());
        }
        Ok(self.transcripts.read(session_id))
    }

    pub fn start(
        self: &Arc<Self>,
        project_id: &str,
        prompt: &str,
        settings: SessionSettings,
    ) -> Result<SessionSummary, String> {
        self.start_in(project_id, prompt, settings, None)
    }

    /// `start` for a plain session (`task_id` is `None`) or for the session of a task whose
    /// worktree is already set. The folder comes from `Inner::cwd_of` either way.
    fn start_in(
        self: &Arc<Self>,
        project_id: &str,
        prompt: &str,
        settings: SessionSettings,
        task_id: Option<&str>,
    ) -> Result<SessionSummary, String> {
        check_text(prompt)?;
        let now = now_ms();
        let (session_id, path) = {
            let mut inner = self.lock();
            let entry = inner.projects.get(project_id).ok_or("unknown project")?;
            let name = entry.project.name.clone();
            let id = format!("s{}", inner.next_session + 1);
            let session = Session {
                id: id.clone(),
                project_id: project_id.to_string(),
                project_name: name,
                started_at: now,
                settings,
                state: SessionState::new(&id, now),
                emitted_inbox_rev: 0,
                saved_key: SaveKey::of(&SessionState::new(&id, now)),
                proc: None,
                task_id: task_id.map(str::to_string),
                spawn_gen: 0,
                gate: Arc::new(Mutex::new(())),
            };
            let path = inner.cwd_of(&session).map_err(|e| e.message())?;
            inner.next_session += 1;
            inner.sessions.insert(id.clone(), session);
            inner.order.push(id.clone());
            if let Some(t) = task_id.and_then(|tid| inner.tasks.iter_mut().find(|t| t.id == tid)) {
                t.session_id = Some(id.clone());
                t.status = TaskStatus::Running;
                t.starting = false;
            }
            (id, path)
        };

        let this = Arc::clone(self);
        let sid = session_id.clone();
        let sink: Sink = Box::new(move |out| this.handle_output(&sid, Some(0), out));
        let proc = match (self.spawner)(&path, &SpawnArgs { resume: None, settings }, sink) {
            Ok(p) => p,
            Err(e) => {
                let mut inner = self.lock();
                inner.sessions.remove(&session_id);
                inner.order.retain(|i| i != &session_id);
                return Err(e);
            }
        };
        if let Some(s) = self.lock().sessions.get_mut(&session_id) {
            s.proc = Some(Arc::clone(&proc));
        }
        if proc.send_line(&protocol::user_message(prompt)).is_err() {
            proc.kill();
            let mut inner = self.lock();
            if let Some(s) = inner.sessions.get_mut(&session_id) {
                s.state.mark_exit(None, now_ms());
            }
            self.publish(inner, &session_id);
            return Err("failed to send the prompt".into());
        }
        let recorded = self.append_transcript(&session_id, vec![transcript::user_prompt(prompt)]);
        // The reader may already have moved the session past Starting while the prompt was
        // being written, so read the summary now, under the lock, and publish it in order.
        let inner = self.lock();
        let summary = inner.sessions.get(&session_id).map(Session::summary).ok_or("unknown session")?;
        self.publish_saving(inner, &session_id, true, self.transcript_events(&session_id, recorded));
        Ok(summary)
    }

    /// Entry point for the reader thread: one stdout line, or the exit.
    #[cfg(test)]
    pub fn on_output(&self, session_id: &str, out: Output) {
        self.handle_output(session_id, None, out);
    }

    /// `on_output` for a process of a known generation: output from a process that was since
    /// retired or replaced is dropped.
    fn handle_output(&self, session_id: &str, generation: Option<u64>, out: Output) {
        let current = |inner: &Inner| {
            inner.sessions.get(session_id).is_some_and(|s| generation.is_none_or(|g| g == s.spawn_gen))
        };
        // The line is parsed once: the state machine and the transcript both read this value.
        let (message, entries) = match &out {
            Output::Line(line) => match serde_json::from_str::<Value>(line) {
                Ok(v) => (Some(protocol::parse_value(&v)), transcript::entries_from_value(&v)),
                Err(_) => (Some(protocol::Message::Unknown), Vec::new()),
            },
            Output::Exit(_) => (None, Vec::new()),
        };
        let now = now_ms();
        let live = {
            let inner = self.lock();
            // The children are being killed on purpose; their exit is not a failure.
            !inner.shutting_down && current(&inner)
        };
        if !live {
            return;
        }
        // Disk I/O happens here, with no lock on `inner`.
        let appended = self.transcripts.append(session_id, entries, now);
        let mut inner = self.lock();
        if inner.shutting_down || !current(&inner) {
            return;
        }
        let in_flight = inner.task_in_flight(session_id);
        let before = inner.sessions.get(session_id).map(|s| (s.state.status, s.state.inbox.len()));
        if let Some(s) = inner.sessions.get_mut(session_id) {
            match (message, out) {
                (Some(m), _) => s.state.apply(m, now),
                (None, Output::Exit(code)) => s.state.mark_exit(code, now),
                (None, Output::Line(_)) => {}
            }
        }
        // Decided under the lock, told after it: the callback runs with nothing held.
        let notice = before.and_then(|(status, items)| {
            let s = inner.sessions.get(session_id)?;
            let added = if s.state.inbox.len() > items { s.state.inbox.last().map(|i| i.kind) } else { None };
            notify::decide(status, s.state.status, added).map(|n| (s.project_name.clone(), n))
        });
        // A task whose turn is over is done: its child is suspended so that nothing holds the
        // worktree and the slot is free. The kill and the wait happen off this thread.
        let mut retire: Option<Arc<dyn Proc>> = None;
        if let Some(s) = inner.sessions.get_mut(session_id) {
            if s.task_id.is_some() && s.state.status == SessionStatus::Idle {
                let at = s.state.last_activity_at;
                s.state.suspend(at);
                s.spawn_gen += 1;
                retire = s.proc.take();
            }
        }
        inner.sync_task(session_id);
        let freed = in_flight && !inner.task_in_flight(session_id);
        // A line can end a session only when a resume failed; the process has no more use.
        let dead = inner
            .sessions
            .get(session_id)
            .filter(|s| matches!(s.state.status, SessionStatus::Ended | SessionStatus::Failed))
            .and_then(|s| s.proc.clone());
        self.publish_line(inner, session_id, appended);
        if let Some((project, notice)) = notice {
            (self.notify)(&project, notice);
        }
        if let Some(p) = dead {
            p.kill();
        }
        if retire.is_some() || freed {
            self.free_slot(retire);
        }
    }

    /// Kill a retired child and wait for it to let go of its folder, then give the freed
    /// slot to the queue. On a thread of its own: git runs there.
    fn free_slot(&self, retire: Option<Arc<dyn Proc>>) {
        let Some(this) = self.me.upgrade() else { return };
        std::thread::spawn(move || {
            if let Some(p) = retire {
                p.kill();
                p.wait_exit(CHILD_EXIT_WAIT);
            }
            this.pump();
        });
    }

    pub fn answer_permission(
        &self,
        item_id: &str,
        decision: Decision,
        message: Option<&str>,
    ) -> Result<(), String> {
        if let Some(m) = message {
            check_len(m)?;
        }
        self.answer_item(item_id, |state| state.permission_line(item_id, decision, message))
    }

    pub fn answer_question(&self, item_id: &str, answers: &BTreeMap<String, String>) -> Result<(), String> {
        if answers.len() > MAX_ANSWERS
            || answers.iter().any(|(k, v)| k.len() > MAX_ANSWER_BYTES || v.len() > MAX_ANSWER_BYTES)
        {
            return Err("answers too large".into());
        }
        self.answer_item(item_id, |state| state.question_line(item_id, answers))
    }

    fn answer_item(
        &self,
        item_id: &str,
        build: impl FnOnce(&SessionState) -> Result<(String, Commit), String>,
    ) -> Result<(), String> {
        let inner = self.lock();
        if inner.is_expired_item(item_id) {
            return Err("this request expired when Orchestra closed".into());
        }
        let sid = inner.session_of_item(item_id).ok_or("unknown or resolved item")?;
        drop(inner);
        self.send_through(&sid, build)
    }

    pub fn reply(&self, session_id: &str, text: &str) -> Result<(), String> {
        check_text(text)?;
        let gate = self.lock().sessions.get(session_id).ok_or("unknown session")?.gate.clone();
        let _gate = gate.lock().unwrap_or_else(|e| e.into_inner());
        let paused = self.lock().sessions.get(session_id).map(|s| s.state.status) == Some(SessionStatus::Suspended);
        if paused {
            return self.resume_with(session_id, text);
        }
        self.send_locked(session_id, |state| state.reply_line(text), Some(text))
    }

    /// A reply to a paused session starts a new process with `--resume` and sends the reply
    /// as its first user line. Called with the session's gate held; `Inner` is not held
    /// across the spawn or the write.
    fn resume_with(&self, session_id: &str, text: &str) -> Result<(), String> {
        let mut inner = self.lock();
        let s = inner.sessions.get(session_id).ok_or("unknown session")?;
        let settings = s.settings;
        let is_task = s.task_id.is_some();
        let path = match inner.cwd_of(s) {
            Ok(p) => p,
            Err(CwdError::Project) => {
                if let Some(s) = inner.sessions.get_mut(session_id) {
                    s.state.fail_resume("The project folder no longer exists", now_ms());
                }
                self.publish(inner, session_id);
                return Err("project folder no longer exists".into());
            }
            // The task keeps its state: the worktree is the only place this conversation
            // may continue, and it is never resumed anywhere else.
            Err(CwdError::Worktree) => return Err(CwdError::Worktree.message().into()),
        };
        if is_task && inner.active_tasks() >= MAX_RUNNING_TASKS {
            return Err("task limit reached".into());
        }
        let plan = inner.sessions.get_mut(session_id).ok_or("unknown session")?.state.begin_resume(now_ms())?;
        let generation = {
            let s = inner.sessions.get_mut(session_id).ok_or("unknown session")?;
            s.spawn_gen += 1;
            s.spawn_gen
        };
        // Working is shown before the process exists; from here the reader can only move it on.
        self.publish(inner, session_id);

        let this = self.me.upgrade().ok_or("app is closing")?;
        let sid = session_id.to_string();
        let sink: Sink = Box::new(move |out| this.handle_output(&sid, Some(generation), out));
        let proc = match (self.spawner)(&path, &SpawnArgs { resume: Some(&plan.claude_session_id), settings }, sink) {
            Ok(p) => p,
            Err(e) => {
                let mut inner = self.lock();
                if let Some(s) = inner.sessions.get_mut(session_id) {
                    s.state.abort_resume();
                }
                self.publish(inner, session_id);
                return Err(e);
            }
        };
        let mut inner = self.lock();
        let over = inner
            .sessions
            .get(session_id)
            .is_none_or(|s| matches!(s.state.status, SessionStatus::Ended | SessionStatus::Failed));
        if over || inner.shutting_down {
            // Stopped (or the app closed) while the process was starting.
            drop(inner);
            proc.kill();
            return Err("session has ended".into());
        }
        if let Some(s) = inner.sessions.get_mut(session_id) {
            s.proc = Some(Arc::clone(&proc));
        }
        drop(inner);
        if proc.send_line(&protocol::user_message(text)).is_err() {
            // The reader reports the exit, which ends the session as a failed resume.
            proc.kill();
            return Err("failed to send to session".into());
        }
        let recorded = self.append_transcript(session_id, vec![transcript::user_prompt(text)]);
        let mut inner = self.lock();
        if let Some(s) = inner.sessions.get_mut(session_id) {
            s.state.commit_resume(&plan.resolve);
        }
        self.publish_saving(inner, session_id, true, self.transcript_events(session_id, recorded));
        Ok(())
    }

    /// Build the line without changing state, write it, and only after a successful write
    /// commit and publish. The session's gate is held throughout so two callers cannot
    /// answer the same item; `Inner` is not held across the write.
    fn send_through(
        &self,
        session_id: &str,
        build: impl FnOnce(&SessionState) -> Result<(String, Commit), String>,
    ) -> Result<(), String> {
        let gate = self.lock().sessions.get(session_id).ok_or("unknown session")?.gate.clone();
        let _gate = gate.lock().unwrap_or_else(|e| e.into_inner());
        self.send_locked(session_id, build, None)
    }

    /// `send_through` with the session's gate already held. `prompt` is the user text to
    /// record in the transcript once the line has been written.
    fn send_locked(
        &self,
        session_id: &str,
        build: impl FnOnce(&SessionState) -> Result<(String, Commit), String>,
        prompt: Option<&str>,
    ) -> Result<(), String> {
        let (proc, line, commit) = {
            let inner = self.lock();
            let s = inner.sessions.get(session_id).ok_or("unknown session")?;
            let proc = s.proc.clone().ok_or("session is not running")?;
            if matches!(s.state.status, SessionStatus::Ended | SessionStatus::Failed) {
                return Err("session has ended".into());
            }
            let (line, commit) = build(&s.state)?;
            (proc, line, commit)
        };
        if proc.send_line(&line).is_err() {
            proc.kill();
            return Err("failed to send to session".into());
        }
        let recorded = prompt.map(|t| self.append_transcript(session_id, vec![transcript::user_prompt(t)])).unwrap_or_default();
        let mut inner = self.lock();
        if let Some(s) = inner.sessions.get_mut(session_id) {
            s.state.commit(commit, now_ms());
        }
        self.publish_saving(inner, session_id, true, self.transcript_events(session_id, recorded));
        Ok(())
    }

    pub fn stop(&self, session_id: &str) -> Result<(), String> {
        let mut inner = self.lock();
        let in_flight = inner.task_in_flight(session_id);
        let s = inner.sessions.get_mut(session_id).ok_or("unknown session")?;
        s.state.mark_stopped(now_ms());
        let proc = s.proc.clone();
        inner.sync_task(session_id);
        let freed = in_flight && !inner.task_in_flight(session_id);
        self.publish(inner, session_id);
        if let Some(p) = proc {
            p.kill();
        }
        if freed {
            self.free_slot(None);
        }
        Ok(())
    }

    /// `stop`, then wait for the process (and its job) to be gone. `Ok(false)` on timeout.
    /// The wait takes no lock, so readers and other commands keep running meanwhile.
    pub fn stop_and_wait(&self, session_id: &str, timeout: Duration) -> Result<bool, String> {
        self.stop(session_id)?;
        let proc = self.lock().sessions.get(session_id).and_then(|s| s.proc.clone());
        Ok(proc.is_none_or(|p| p.wait_exit(timeout)))
    }

    /// App exit: suspend every live session, write the final state and wait for it, and only
    /// then kill the children, so no child outlives the app. Once this starts the readers
    /// are ignored, so the exit that follows the kill cannot turn a session into a failure.
    pub fn shutdown(&self) {
        let mut inner = self.lock();
        inner.shutting_down = true;
        for s in inner.sessions.values_mut() {
            let at = s.state.last_activity_at;
            s.state.suspend(at);
        }
        let procs: Vec<Arc<dyn Proc>> = inner.sessions.values().filter_map(|s| s.proc.clone()).collect();
        let snapshot = self.writer.as_ref().map(|_| {
            inner.revision += 1;
            inner.snapshot()
        });
        drop(inner);
        if let (Some(w), Some(s)) = (&self.writer, snapshot) {
            w.save(s);
            w.flush();
        }
        for p in procs {
            p.kill();
        }
    }
}

fn check_len(text: &str) -> Result<(), String> {
    if text.len() > MAX_TEXT_BYTES {
        return Err("text too long".into());
    }
    Ok(())
}

fn check_text(text: &str) -> Result<(), String> {
    if text.trim().is_empty() {
        return Err("text is empty".into());
    }
    check_len(text)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

    static RIG_N: AtomicUsize = AtomicUsize::new(0);

    /// Called with the number of lines written so far, after each successful write.
    type OnWrite = Box<dyn Fn(usize) + Send>;

    struct Fake {
        lines: Mutex<Vec<String>>,
        killed: AtomicBool,
        fail: AtomicBool,
        on_write: Mutex<Option<OnWrite>>,
        /// Run inside `wait_exit`, to observe what is held while waiting.
        on_wait: Mutex<Option<Box<dyn Fn() + Send>>>,
    }

    impl Proc for Fake {
        fn send_line(&self, line: &str) -> Result<(), String> {
            if self.fail.load(Ordering::SeqCst) {
                return Err("pipe closed".into());
            }
            let n = {
                let mut lines = self.lines.lock().unwrap();
                lines.push(line.to_string());
                lines.len()
            };
            if let Some(f) = self.on_write.lock().unwrap().as_ref() {
                f(n);
            }
            Ok(())
        }
        fn kill(&self) {
            self.killed.store(true, Ordering::SeqCst);
        }
        fn wait_exit(&self, _timeout: Duration) -> bool {
            if let Some(f) = self.on_wait.lock().unwrap().as_ref() {
                f();
            }
            self.killed.load(Ordering::SeqCst)
        }
    }

    struct Rig {
        sessions: Arc<Sessions>,
        fake: Arc<Fake>,
        /// Every sink handed to the spawner, in spawn order.
        sink: Arc<Mutex<Vec<Sink>>>,
        /// The worktree base this rig was built with.
        base: PathBuf,
        events: Arc<Mutex<Vec<Event>>>,
        /// Every spawn: the directory and the id passed to `--resume`, if any.
        spawns: Arc<Mutex<Vec<(std::path::PathBuf, Option<String>)>>>,
        /// The settings of every spawn, in order.
        spawn_settings: Arc<Mutex<Vec<SessionSettings>>>,
        /// Every notice the notifier was given: the project name and the kind.
        notices: Arc<Mutex<Vec<(String, crate::notify::NoticeKind)>>>,
    }

    fn rig() -> (Rig, Project) {
        rig_with(None)
    }

    fn rig_with(writer: Option<Writer>) -> (Rig, Project) {
        let fake = Arc::new(Fake {
            lines: Mutex::new(vec![]),
            killed: AtomicBool::new(false),
            fail: AtomicBool::new(false),
            on_write: Mutex::new(None),
            on_wait: Mutex::new(None),
        });
        let sink: Arc<Mutex<Vec<Sink>>> = Arc::new(Mutex::new(Vec::new()));
        let base = scratch(&format!("worktrees-{}", RIG_N.fetch_add(1, Ordering::SeqCst)));
        let events = Arc::new(Mutex::new(vec![]));
        let spawns = Arc::new(Mutex::new(vec![]));
        let spawn_settings = Arc::new(Mutex::new(vec![]));
        let notices = Arc::new(Mutex::new(vec![]));
        let (f2, s2, e2, p2, g2, n2) =
            (fake.clone(), sink.clone(), events.clone(), spawns.clone(), spawn_settings.clone(), notices.clone());
        let sessions = Sessions::with_writer(
            Box::new(move |e| e2.lock().unwrap().push(e)),
            Box::new(move |dir, spawn, sk| {
                p2.lock().unwrap().push((dir.to_path_buf(), spawn.resume.map(|r| r.as_str().to_string())));
                g2.lock().unwrap().push(spawn.settings);
                s2.lock().unwrap().push(sk);
                Ok(f2.clone() as Arc<dyn Proc>)
            }),
            Box::new(move |project, notice| n2.lock().unwrap().push((project.to_string(), notice.kind))),
            writer,
            TranscriptStore::memory(),
            base.clone(),
        );
        let project = sessions.add_project(&std::env::temp_dir()).unwrap();
        (Rig { sessions, fake, sink, base, events, spawns, spawn_settings, notices }, project)
    }

    impl Rig {
        fn feed(&self, line: &str) {
            let guard = self.sink.lock().unwrap();
            (guard.last().unwrap())(Output::Line(line.to_string()));
        }

        /// Feed `line` through the sink while the nth write is in flight.
        fn feed_on_write(&self, nth: usize, line: &'static str) {
            let sink = self.sink.clone();
            *self.fake.on_write.lock().unwrap() = Some(Box::new(move |n| {
                if n == nth {
                    let guard = sink.lock().unwrap();
                    (guard.last().unwrap())(Output::Line(line.to_string()));
                }
            }));
        }

        fn updates(&self) -> Vec<SessionSummary> {
            self.events
                .lock()
                .unwrap()
                .iter()
                .filter_map(|e| if let Event::SessionUpdated(s) = e { Some(s.clone()) } else { None })
                .collect()
        }
    }

    const INIT: &str = r#"{"type":"system","subtype":"init","session_id":"c-1"}"#;
    const PERM: &str = r#"{"type":"control_request","request_id":"r-1","request":{"subtype":"can_use_tool","tool_name":"Bash","input":{"command":"touch x"}}}"#;
    const DONE: &str = r#"{"type":"result","subtype":"success","is_error":false,"result":"done"}"#;

    // --- tasks ---

    fn git_in(dir: &Path, args: &[&str]) {
        let out = std::process::Command::new("git")
            .args(["-c", "user.name=t", "-c", "user.email=t@example.invalid", "-c", "commit.gpgsign=false"])
            .args(args)
            .current_dir(dir)
            .output()
            .unwrap();
        assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
    }

    /// A repository with one commit on `main`.
    fn git_repo(name: &str) -> PathBuf {
        let d = scratch(name);
        git_in(&d, &["init", "-b", "main"]);
        std::fs::write(d.join("a.txt"), "a").unwrap();
        git_in(&d, &["add", "."]);
        git_in(&d, &["commit", "-m", "first"]);
        d
    }

    fn rig_repo(name: &str) -> (Rig, Project, PathBuf) {
        let (r, _) = rig();
        let repo = git_repo(name);
        let p = r.sessions.add_project(&repo).unwrap();
        (r, p, repo)
    }

    fn wait_for(what: &str, f: impl Fn() -> bool) {
        for _ in 0..400 {
            if f() {
                return;
            }
            std::thread::sleep(Duration::from_millis(25));
        }
        panic!("timed out waiting for {what}");
    }

    fn task_status(r: &Rig, id: &str) -> TaskStatus {
        r.sessions.list_tasks().into_iter().find(|t| t.id == id).unwrap().status
    }

    impl Rig {
        /// Feed `line` to the nth sink handed out, that is to the nth process spawned.
        fn feed_nth(&self, n: usize, line: &str) {
            let guard = self.sink.lock().unwrap();
            (guard[n])(Output::Line(line.to_string()));
        }
    }

    #[test]
    fn queue_respects_running_limit() {
        let (r, p, repo) = rig_repo("queue");
        let t1 = r.sessions.create_task(&p.id, "one", SessionSettings::default()).unwrap();
        let t2 = r.sessions.create_task(&p.id, "two", SessionSettings::default()).unwrap();
        let t3 = r.sessions.create_task(&p.id, "three", SessionSettings::default()).unwrap();
        assert_eq!((t1.id.as_str(), t2.id.as_str(), t3.id.as_str()), ("t1", "t2", "t3"));
        assert_eq!(task_status(&r, "t1"), TaskStatus::Running);
        assert_eq!(task_status(&r, "t2"), TaskStatus::Running);
        assert_eq!(task_status(&r, "t3"), TaskStatus::Queued);
        assert_eq!(r.spawns.lock().unwrap().len(), 2);
        // A plain session is outside the limit.
        r.sessions.start(&p.id, "plain", SessionSettings::default()).unwrap();
        assert_eq!(r.spawns.lock().unwrap().len(), 3);
        assert_eq!(task_status(&r, "t3"), TaskStatus::Queued);
        // Waiting on the owner still holds the slot.
        r.feed_nth(1, INIT_UUID);
        r.feed_nth(1, PERM);
        assert_eq!(task_status(&r, "t2"), TaskStatus::NeedsInput);
        assert_eq!(task_status(&r, "t3"), TaskStatus::Queued);
        // Done frees it, and the oldest queued task takes it.
        r.feed_nth(0, INIT_UUID);
        r.feed_nth(0, DONE);
        wait_for("the third task to start", || task_status(&r, "t3") == TaskStatus::Running);
        assert_eq!(task_status(&r, "t1"), TaskStatus::Done);
        assert_eq!(r.spawns.lock().unwrap().len(), 4);
        let _ = std::fs::remove_dir_all(&repo);
    }

    #[test]
    fn task_session_spawns_in_worktree_not_project() {
        let (r, p, repo) = rig_repo("worktreecwd");
        let t = r.sessions.create_task(&p.id, "do a thing\nmore detail", SessionSettings::default()).unwrap();
        let spawns = r.spawns.lock().unwrap();
        assert_eq!(spawns.len(), 1);
        let expected = r.base.join(&p.id).join("t1");
        assert_eq!(spawns[0].0, expected);
        assert_ne!(spawns[0].0, repo);
        assert!(expected.join(".git").exists(), "a real worktree");
        assert_eq!(t.title, "do a thing");
        assert_eq!((t.branch.as_deref(), t.base_branch.as_str()), (Some("orchestra/t1"), "main"));
        assert_eq!(r.sessions.list_sessions()[0].project_id, p.id);
        let _ = std::fs::remove_dir_all(&repo);
    }

    #[test]
    fn branch_collision_takes_a_suffix() {
        let (r, p, repo) = rig_repo("collision");
        git_in(&repo, &["branch", "orchestra/t1"]);
        let t = r.sessions.create_task(&p.id, "x", SessionSettings::default()).unwrap();
        assert_eq!(t.branch.as_deref(), Some("orchestra/t1-2"));
        let _ = std::fs::remove_dir_all(&repo);
    }

    /// A task driven to done, its session suspended: the id of its session.
    fn done_task(r: &Rig, p: &Project) -> String {
        let t = r.sessions.create_task(&p.id, "work", SessionSettings::default()).unwrap();
        r.feed_nth(0, INIT_UUID);
        r.feed_nth(0, DONE);
        assert_eq!(task_status(r, &t.id), TaskStatus::Done);
        t.session_id.unwrap()
    }

    #[test]
    fn resume_of_task_session_uses_worktree() {
        let (r, p, repo) = rig_repo("taskresume");
        let sid = done_task(&r, &p);
        assert_eq!(r.sessions.list_sessions()[0].status, SessionStatus::Suspended);
        r.sessions.reply(&sid, "and then?").unwrap();
        let spawns = r.spawns.lock().unwrap();
        assert_eq!(spawns.len(), 2);
        assert_eq!(spawns[1], (r.base.join(&p.id).join("t1"), Some(CLAUDE_ID.to_string())));
        drop(spawns);
        assert_eq!(task_status(&r, "t1"), TaskStatus::Running, "a follow-up counts again");
        let _ = std::fs::remove_dir_all(&repo);
    }

    #[test]
    fn follow_up_is_refused_when_the_limit_is_full() {
        let (r, p, repo) = rig_repo("taskfull");
        let sid = done_task(&r, &p);
        r.sessions.create_task(&p.id, "two", SessionSettings::default()).unwrap();
        r.sessions.create_task(&p.id, "three", SessionSettings::default()).unwrap();
        let before = r.spawns.lock().unwrap().len();
        assert_eq!(r.sessions.reply(&sid, "more").unwrap_err(), "task limit reached");
        assert_eq!(r.spawns.lock().unwrap().len(), before);
        assert_eq!(task_status(&r, "t1"), TaskStatus::Done);
        let _ = std::fs::remove_dir_all(&repo);
    }

    #[test]
    fn resume_refused_when_worktree_missing() {
        let (r, p, repo) = rig_repo("taskgone");
        let sid = done_task(&r, &p);
        std::fs::remove_dir_all(r.base.join(&p.id).join("t1")).unwrap();
        let err = r.sessions.reply(&sid, "more").unwrap_err();
        assert_eq!(err, "the task worktree no longer exists");
        assert!(!err.contains('/') && !err.contains('\\'));
        assert_eq!(r.spawns.lock().unwrap().len(), 1, "nothing spawned, here or in the project");
        assert_eq!(r.sessions.list_sessions()[0].status, SessionStatus::Suspended);
        let _ = std::fs::remove_dir_all(&repo);
    }

    #[test]
    fn done_task_suspends_child_and_frees_slot() {
        let (r, p, repo) = rig_repo("taskdone");
        r.sessions.create_task(&p.id, "work", SessionSettings::default()).unwrap();
        assert_eq!(r.sessions.lock().active_tasks(), 1);
        r.feed_nth(0, INIT_UUID);
        r.feed_nth(0, DONE);
        let s = r.sessions.list_sessions().remove(0);
        assert_eq!(s.status, SessionStatus::Suspended);
        assert_eq!(task_status(&r, "t1"), TaskStatus::Done);
        assert_eq!(r.sessions.lock().active_tasks(), 0, "the slot is free");
        wait_for("the child to be killed", || r.fake.killed.load(Ordering::SeqCst));
        // The exit of the retired child is not a failure.
        r.sessions.handle_output(&s.id, Some(0), Output::Exit(Some(1)));
        assert_eq!(r.sessions.list_sessions()[0].status, SessionStatus::Suspended);
        assert_eq!(task_status(&r, "t1"), TaskStatus::Done);
        let task = r.sessions.list_tasks().remove(0);
        assert!(!task.session_over && !task.last_turn_error);
        let _ = std::fs::remove_dir_all(&repo);
    }

    #[test]
    fn create_task_refuses_non_repo_and_detached_head() {
        let (r, _) = rig();
        let plain = scratch("notrepo");
        let p = r.sessions.add_project(&plain).unwrap();
        let err = r.sessions.create_task(&p.id, "x", SessionSettings::default()).unwrap_err();
        assert_eq!(err, "the project is not a git repository");
        let repo = git_repo("detached");
        let p = r.sessions.add_project(&repo).unwrap();
        git_in(&repo, &["checkout", "--detach"]);
        let err = r.sessions.create_task(&p.id, "x", SessionSettings::default()).unwrap_err();
        assert_eq!(err, "the project is not on a branch");
        assert!(r.sessions.create_task("p99", "x", SessionSettings::default()).is_err());
        assert!(r.sessions.create_task(&p.id, "  ", SessionSettings::default()).is_err());
        assert!(r.sessions.list_tasks().is_empty(), "nothing was queued");
        assert!(r.spawns.lock().unwrap().is_empty());
        let _ = std::fs::remove_dir_all(&plain);
        let _ = std::fs::remove_dir_all(&repo);
    }

    #[test]
    fn cancel_applies_to_queued_tasks_only() {
        let (r, p, repo) = rig_repo("cancel");
        for n in 0..3 {
            r.sessions.create_task(&p.id, &format!("job {n}"), SessionSettings::default()).unwrap();
        }
        assert!(r.sessions.cancel_task("t1").is_err(), "running");
        assert!(r.sessions.cancel_task("t9").is_err());
        r.sessions.cancel_task("t3").unwrap();
        assert_eq!(task_status(&r, "t3"), TaskStatus::Discarded);
        assert!(r.sessions.cancel_task("t3").is_err(), "already over");
        assert!(!r.base.join(&p.id).join("t3").exists(), "it never had a worktree");
        let _ = std::fs::remove_dir_all(&repo);
    }

    fn stored_task(id: &str, status: &str, worktree: Option<PathBuf>, session: Option<&str>) -> store::StoredTask {
        store::StoredTask {
            id: id.into(),
            project_id: "p1".into(),
            project_name: "proj".into(),
            prompt: "work".into(),
            model: None,
            permission_mode: None,
            created_at: 1,
            status: status.into(),
            session_id: session.map(str::to_string),
            worktree,
            branch: Some(format!("orchestra/{id}")),
            base_branch: "main".into(),
            base_commit: "a".repeat(40),
            note: None,
            last_turn_error: false,
            session_over: false,
        }
    }

    fn restore_tasks(r: &Rig, dir: &Path, sessions: Vec<StoredSession>, tasks: Vec<store::StoredTask>) {
        r.sessions.restore(Snapshot {
            schema: store::SCHEMA,
            revision: 1,
            next_session: 1,
            next_project: 1,
            projects: vec![crate::projects::StoredProject { id: "p1".into(), name: "proj".into(), path: dir.to_path_buf() }],
            sessions,
            next_task: 1,
            tasks,
        });
    }

    #[test]
    fn restore_marks_missing_worktree_error_and_spawns_nothing() {
        let (r, _) = rig();
        let dir = scratch("restoretasks");
        let live = r.base.join("p1").join("t3");
        std::fs::create_dir_all(&live).unwrap();
        let mut linked = stored_session("s1", SessionStatus::Working, vec![], 1);
        linked.task_id = Some("t3".into());
        restore_tasks(
            &r,
            &dir,
            vec![linked],
            vec![
                stored_task("t1", "running", Some(r.base.join("p1").join("t1")), None),
                stored_task("t2", "queued", None, None),
                stored_task("t3", "running", Some(live), Some("s1")),
                stored_task("t4", "frozen", None, None),
            ],
        );
        let by_id = |id: &str| r.sessions.list_tasks().into_iter().find(|t| t.id == id).unwrap();
        assert_eq!((by_id("t1").status, by_id("t1").note.as_deref()), (TaskStatus::Error, Some("worktree missing")));
        assert_eq!(by_id("t2").status, TaskStatus::Queued);
        let t3 = by_id("t3");
        assert_eq!((t3.status, t3.note.as_deref()), (TaskStatus::NeedsInput, Some(task::PAUSED_NOTE)));
        assert_eq!((by_id("t4").status, by_id("t4").note.as_deref()), (TaskStatus::Error, Some("unknown status")));
        assert_eq!(r.sessions.lock().active_tasks(), 0, "a paused task holds no slot");
        assert!(r.spawns.lock().unwrap().is_empty() && r.sink.lock().unwrap().is_empty(), "nothing spawned");
        assert_eq!(r.sessions.lock().next_task, 4, "ids continue past the largest stored");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn stored_worktree_outside_base_is_rejected_on_load() {
        let (r, _) = rig();
        let dir = scratch("outside");
        let outside = scratch("outside-wt");
        // A path that climbs out through .. is no better than one that simply lies outside.
        let sneaky = r.base.join("p1").join("..").join("..").join(outside.file_name().unwrap());
        restore_tasks(
            &r,
            &dir,
            vec![],
            vec![
                stored_task("t1", "done", Some(outside.clone()), None),
                stored_task("t2", "done", Some(sneaky), None),
            ],
        );
        for id in ["t1", "t2"] {
            let t = r.sessions.list_tasks().into_iter().find(|t| t.id == id).unwrap();
            assert_eq!(t.status, TaskStatus::Error, "{id}");
        }
        let t1 = r.sessions.list_tasks().remove(0);
        assert_eq!(t1.note.as_deref(), Some("worktree is outside the worktree folder"));
        assert!(r.sessions.lock().tasks.iter().all(|t| t.worktree.is_none()), "never used again");
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::remove_dir_all(&outside);
    }

    #[test]
    fn task_summary_has_no_path() {
        let (r, p, repo) = rig_repo("nopath");
        let t = r.sessions.create_task(&p.id, "work", SessionSettings::default()).unwrap();
        let json = serde_json::to_string(&t).unwrap();
        let escaped = |p: &Path| {
            let s = serde_json::to_string(&p.to_string_lossy()).unwrap();
            s[1..s.len() - 1].to_string()
        };
        assert!(!json.contains(&escaped(&r.base)) && !json.contains(&escaped(&repo)), "{json}");
        let v: serde_json::Value = serde_json::from_str(&json).unwrap();
        let mut keys: Vec<_> = v.as_object().unwrap().keys().cloned().collect();
        keys.sort();
        assert_eq!(
            keys,
            [
                "base_branch", "branch", "created_at", "id", "last_turn_error", "note", "project_id",
                "project_name", "session_id", "session_over", "status", "title"
            ]
        );
        let events = r.events.lock().unwrap();
        let updates = events.iter().filter(|e| matches!(e, Event::TaskUpdated(_))).count();
        assert!(updates >= 2, "queued and running were both announced");
        let _ = std::fs::remove_dir_all(&repo);
    }

    #[test]
    fn task_is_saved_with_its_session_link() {
        let d = scratch("tasksave");
        let file = d.join("state.json");
        let (r, _) = rig_with(Some(Writer::new(file.clone())));
        let repo = git_repo("tasksave-repo");
        let p = r.sessions.add_project(&repo).unwrap();
        let t = r.sessions.create_task(&p.id, "work", SessionSettings::default()).unwrap();
        r.sessions.shutdown();
        let snap = store::load(&file).unwrap().unwrap();
        assert_eq!((snap.schema, snap.next_task, snap.tasks.len()), (2, 1, 1));
        assert_eq!(snap.tasks[0].session_id, t.session_id);
        assert_eq!(snap.tasks[0].worktree, Some(r.base.join(&p.id).join("t1")));
        assert_eq!(snap.sessions[0].task_id.as_deref(), Some("t1"));
        let _ = std::fs::remove_dir_all(&repo);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn rejects_unknown_project_without_leaking_a_path() {
        let (r, _) = rig();
        let err = r.sessions.start("nope", "hi", SessionSettings::default()).unwrap_err();
        assert_eq!(err, "unknown project");
        assert!(!err.contains('/') && !err.contains('\\') && !err.contains("Users") && !err.contains(":"));
    }

    #[test]
    fn rejects_empty_and_oversize_prompt() {
        let (r, p) = rig();
        assert!(r.sessions.start(&p.id, "", SessionSettings::default()).is_err());
        assert!(r.sessions.start(&p.id, "   ", SessionSettings::default()).is_err());
        assert!(r.sessions.start(&p.id, &"a".repeat(MAX_TEXT_BYTES + 1), SessionSettings::default()).is_err());
        assert!(r.sessions.start(&p.id, &"a".repeat(MAX_TEXT_BYTES), SessionSettings::default()).is_ok());
    }

    #[test]
    fn start_sends_prompt_over_stdin_and_emits() {
        let (r, p) = rig();
        let s = r.sessions.start(&p.id, "hello", SessionSettings::default()).unwrap();
        assert_eq!(s.status, SessionStatus::Starting);
        let lines = r.fake.lines.lock().unwrap();
        assert_eq!(lines.len(), 1);
        let v: serde_json::Value = serde_json::from_str(&lines[0]).unwrap();
        assert_eq!(v["message"]["content"], "hello");
        assert!(!r.events.lock().unwrap().is_empty());
    }

    #[test]
    fn answer_rejects_unknown_and_resolved_items() {
        let (r, p) = rig();
        r.sessions.start(&p.id, "go", SessionSettings::default()).unwrap();
        r.feed(INIT);
        assert!(r.sessions.answer_permission("zzz", Decision::Allow, None).is_err());
        r.feed(PERM);
        assert_eq!(r.sessions.list_sessions()[0].status, SessionStatus::WaitingPermission);
        let item = r.sessions.list_inbox().into_iter().find(|i| !i.resolved).unwrap();
        r.sessions.answer_permission(&item.id, Decision::Allow, None).unwrap();
        assert!(r.sessions.answer_permission(&item.id, Decision::Allow, None).is_err());
        assert_eq!(r.fake.lines.lock().unwrap().len(), 2);
        assert_eq!(r.sessions.list_sessions()[0].status, SessionStatus::Working);
    }

    #[test]
    fn answer_rejects_oversize_message_and_answers() {
        let (r, p) = rig();
        r.sessions.start(&p.id, "go", SessionSettings::default()).unwrap();
        r.feed(INIT);
        r.feed(PERM);
        let item = r.sessions.list_inbox().remove(0);
        let big = "a".repeat(MAX_TEXT_BYTES + 1);
        assert!(r.sessions.answer_permission(&item.id, Decision::Deny, Some(&big)).is_err());
        let mut answers = BTreeMap::new();
        for i in 0..=MAX_ANSWERS {
            answers.insert(i.to_string(), "x".into());
        }
        assert!(r.sessions.answer_question(&item.id, &answers).is_err());
    }

    #[test]
    fn reply_validates_session_text_and_status() {
        let (r, p) = rig();
        let s = r.sessions.start(&p.id, "go", SessionSettings::default()).unwrap();
        assert!(r.sessions.reply("nope", "x").is_err());
        assert!(r.sessions.reply(&s.id, "").is_err());
        assert!(r.sessions.reply(&s.id, &"a".repeat(MAX_TEXT_BYTES + 1)).is_err());
        assert!(r.sessions.reply(&s.id, "more").is_err(), "not idle yet");
        r.feed(INIT);
        r.feed(DONE);
        r.sessions.reply(&s.id, "more").unwrap();
        assert_eq!(r.sessions.list_sessions()[0].status, SessionStatus::Working);
    }

    #[test]
    fn stop_kills_child_and_shutdown_covers_app_exit() {
        let (r, p) = rig();
        let s = r.sessions.start(&p.id, "go", SessionSettings::default()).unwrap();
        r.sessions.stop(&s.id).unwrap();
        assert!(r.fake.killed.load(Ordering::SeqCst));
        assert_eq!(r.sessions.list_sessions()[0].status, SessionStatus::Ended);
        assert!(r.sessions.stop("nope").is_err());

        let (r2, p2) = rig();
        r2.sessions.start(&p2.id, "go", SessionSettings::default()).unwrap();
        r2.sessions.shutdown();
        assert!(r2.fake.killed.load(Ordering::SeqCst));
    }

    #[test]
    fn stop_and_wait_holds_no_lock_while_waiting() {
        let (r, p) = rig();
        let s = r.sessions.start(&p.id, "go", SessionSettings::default()).unwrap();
        let free = Arc::new(AtomicBool::new(false));
        let (sessions, f2) = (r.sessions.clone(), free.clone());
        *r.fake.on_wait.lock().unwrap() = Some(Box::new(move || {
            f2.store(sessions.inner.try_lock().is_ok(), Ordering::SeqCst);
        }));
        assert_eq!(r.sessions.stop_and_wait(&s.id, Duration::from_secs(1)), Ok(true));
        assert!(free.load(Ordering::SeqCst), "Inner was held during wait_exit");
        assert!(r.sessions.stop_and_wait("nope", Duration::from_secs(1)).is_err());
    }

    #[test]
    fn start_publishes_post_send_summary_in_order() {
        let (r, p) = rig();
        r.feed_on_write(1, INIT);
        let s = r.sessions.start(&p.id, "go", SessionSettings::default()).unwrap();
        assert_eq!(s.status, SessionStatus::Working);
        let updates = r.updates();
        assert_eq!(updates.last().unwrap().status, SessionStatus::Working);
        assert!(updates.iter().all(|u| u.status == SessionStatus::Working), "no stale Starting event");
    }

    #[test]
    fn failed_answer_send_leaves_item_pending() {
        let (r, p) = rig();
        r.sessions.start(&p.id, "go", SessionSettings::default()).unwrap();
        r.feed(INIT);
        r.feed(PERM);
        let item = r.sessions.list_inbox().remove(0);
        let before = r.events.lock().unwrap().len();
        r.fake.fail.store(true, Ordering::SeqCst);
        assert!(r.sessions.answer_permission(&item.id, Decision::Allow, None).is_err());
        assert!(!r.sessions.list_inbox()[0].resolved);
        assert_eq!(r.sessions.list_sessions()[0].status, SessionStatus::WaitingPermission);
        assert_eq!(r.events.lock().unwrap().len(), before, "a failed write fires nothing");
        assert!(r.fake.killed.load(Ordering::SeqCst));
    }

    #[test]
    fn failed_reply_send_leaves_session_idle() {
        let (r, p) = rig();
        let s = r.sessions.start(&p.id, "go", SessionSettings::default()).unwrap();
        r.feed(INIT);
        r.feed(DONE);
        r.fake.fail.store(true, Ordering::SeqCst);
        assert!(r.sessions.reply(&s.id, "more").is_err());
        assert_eq!(r.sessions.list_sessions()[0].status, SessionStatus::Idle);
        assert!(!r.sessions.list_inbox()[0].resolved);
    }

    #[test]
    fn reader_update_during_reply_send_wins() {
        let (r, p) = rig();
        let s = r.sessions.start(&p.id, "go", SessionSettings::default()).unwrap();
        r.feed(INIT);
        r.feed(DONE);
        r.feed_on_write(2, DONE);
        r.sessions.reply(&s.id, "more").unwrap();
        let inbox = r.sessions.list_inbox();
        assert_eq!(inbox.len(), 2);
        assert!(inbox[0].resolved && !inbox[1].resolved, "the new Finished item stays open");
        assert_eq!(r.sessions.list_sessions()[0].status, SessionStatus::Idle);
    }

    #[test]
    fn concurrent_answers_send_once() {
        let (r, p) = rig();
        r.sessions.start(&p.id, "go", SessionSettings::default()).unwrap();
        r.feed(INIT);
        r.feed(PERM);
        *r.fake.on_write.lock().unwrap() = Some(Box::new(|_| std::thread::sleep(std::time::Duration::from_millis(50))));
        let item = r.sessions.list_inbox().remove(0);
        let results: Vec<Result<(), String>> = std::thread::scope(|sc| {
            let hs: Vec<_> = (0..2)
                .map(|_| sc.spawn(|| r.sessions.answer_permission(&item.id, Decision::Allow, None)))
                .collect();
            hs.into_iter().map(|h| h.join().unwrap()).collect()
        });
        assert_eq!(results.iter().filter(|x| x.is_ok()).count(), 1);
        assert_eq!(r.fake.lines.lock().unwrap().len(), 2);
    }

    #[test]
    fn unexpected_exit_marks_failed() {
        let (r, p) = rig();
        let s = r.sessions.start(&p.id, "go", SessionSettings::default()).unwrap();
        r.sessions.on_output(&s.id, Output::Exit(Some(1)));
        assert_eq!(r.sessions.list_sessions()[0].status, SessionStatus::Failed);
        assert!(r.sessions.list_inbox().iter().any(|i| i.kind == state::InboxKind::Error));
    }

    const CLAUDE_ID: &str = "0b9f6a2c-3d4e-4f50-8a1b-2c3d4e5f6a7b";
    const INIT_UUID: &str = r#"{"type":"system","subtype":"init","session_id":"0b9f6a2c-3d4e-4f50-8a1b-2c3d4e5f6a7b"}"#;

    fn scratch(name: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("orchestra-sessions-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn open_permission(id: &str, session_id: &str) -> InboxItem {
        InboxItem {
            id: id.into(),
            session_id: session_id.into(),
            kind: state::InboxKind::Permission,
            created_at: 5,
            resolved: false,
            expired: false,
            summary: "Permission to use Bash".into(),
            tool_name: Some("Bash".into()),
            tool_input_preview: None,
            questions: None,
            result_text: None,
        }
    }

    fn stored_session(id: &str, status: SessionStatus, inbox: Vec<InboxItem>, next_item: u64) -> StoredSession {
        StoredSession {
            id: id.into(),
            project_id: "p1".into(),
            project_name: "proj".into(),
            started_at: 1,
            model: None,
            permission_mode: None,
            task_id: None,
            state: state::PersistedSession {
                status,
                last_activity_at: 7,
                last_message: Some("hello".into()),
                claude_session_id: Some(CLAUDE_ID.into()),
                inbox,
                next_item,
                unknown_count: 0,
            },
        }
    }

    #[test]
    fn quit_records_suspended_not_failed() {
        let d = scratch("quit");
        let file = d.join("state.json");
        let (r, p) = rig_with(Some(Writer::new(file.clone())));
        let s = r.sessions.start(&p.id, "go", SessionSettings::default()).unwrap();
        r.feed(INIT_UUID);
        r.feed(PERM);
        r.sessions.shutdown();
        assert!(r.fake.killed.load(Ordering::SeqCst), "children are killed after the flush");
        // The reader sees the kill as an exit with code 1 and must not act on it.
        r.sessions.on_output(&s.id, Output::Exit(Some(1)));
        assert_eq!(r.sessions.list_sessions()[0].status, SessionStatus::Suspended);
        assert!(r.sessions.list_inbox().iter().all(|i| i.kind != state::InboxKind::Error));
        let saved = store::load(&file).unwrap().unwrap();
        assert_eq!(saved.sessions[0].state.status, SessionStatus::Suspended);
        assert!(saved.sessions[0].state.inbox.iter().all(|i| i.kind != state::InboxKind::Error));
        assert!(saved.sessions[0].state.inbox[0].expired);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn restore_continues_counters() {
        let (r, _) = rig();
        let dir = scratch("counters");
        let snapshot = Snapshot {
            schema: store::SCHEMA,
            next_task: 0,
            tasks: vec![],
            revision: 9,
            next_session: 5,
            next_project: 3,
            projects: vec![crate::projects::StoredProject { id: "p3".into(), name: "proj".into(), path: dir.clone() }],
            sessions: vec![stored_session("s2", SessionStatus::Idle, vec![], 4)],
        };
        r.sessions.restore(snapshot);
        let listed = r.sessions.list_sessions();
        assert_eq!((listed[0].id.as_str(), listed[0].status), ("s2", SessionStatus::Suspended));
        assert_eq!(listed[0].last_activity_at, 7, "closing is not activity");
        assert!(r.fake.lines.lock().unwrap().is_empty() && r.sink.lock().unwrap().is_empty(), "nothing spawned");
        let other = dir.join("other");
        std::fs::create_dir_all(&other).unwrap();
        assert_eq!(r.sessions.add_project(&other).unwrap().id, "p4");
        let started = r.sessions.start("p3", "again", SessionSettings::default()).unwrap();
        assert_eq!(started.id, "s6");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn restore_expires_items_left_open_by_crash() {
        let (r, _) = rig();
        let snapshot = Snapshot {
            schema: store::SCHEMA,
            next_task: 0,
            tasks: vec![],
            revision: 1,
            next_session: 1,
            next_project: 0,
            projects: vec![],
            sessions: vec![stored_session("s1", SessionStatus::WaitingPermission, vec![open_permission("s1-i1", "s1")], 1)],
        };
        r.sessions.restore(snapshot);
        assert_eq!(r.sessions.list_sessions()[0].status, SessionStatus::Suspended);
        let item = &r.sessions.list_inbox()[0];
        assert!(item.resolved && item.expired);
        assert!(r.sessions.answer_permission(&item.id, Decision::Allow, None).is_err());
    }

    #[test]
    fn ipc_types_carry_no_path() {
        let dir = scratch("nopath");
        let (r, _) = rig();
        let project = r.sessions.add_project(&dir).unwrap();
        let s = r.sessions.start(&project.id, "go", SessionSettings::default()).unwrap();
        r.feed(INIT_UUID);
        r.feed(PERM);
        let json = [
            serde_json::to_string(&s).unwrap(),
            serde_json::to_string(&r.sessions.list_sessions()).unwrap(),
            serde_json::to_string(&r.sessions.list_inbox()).unwrap(),
            serde_json::to_string(&r.sessions.list_projects()).unwrap(),
            serde_json::to_string(&store::StorageStatus::unreadable()).unwrap(),
        ]
        .join("\n");
        let raw = dir.to_string_lossy().into_owned();
        let escaped = serde_json::to_string(&raw).unwrap();
        assert!(!json.contains(&raw) && !json.contains(escaped.trim_matches('"')), "no project path over IPC");
        assert_eq!(project.name, dir.file_name().unwrap().to_string_lossy(), "only the folder name is exposed");
        let _ = std::fs::remove_dir_all(&dir);
    }

    const RESUME_UNKNOWN_ID: &str = include_str!("../../tests/fixtures/stream_json/resume_unknown_id.jsonl");

    fn finished_item(id: &str, session_id: &str) -> InboxItem {
        InboxItem { kind: state::InboxKind::Finished, summary: "all done".into(), ..open_permission(id, session_id) }
    }

    /// A restored, paused session s1 in a real scratch folder registered as p1.
    fn paused(r: &Rig, dir: &std::path::Path) {
        r.sessions.restore(Snapshot {
            schema: store::SCHEMA,
            next_task: 0,
            tasks: vec![],
            revision: 1,
            next_session: 1,
            next_project: 1,
            projects: vec![crate::projects::StoredProject { id: "p1".into(), name: "proj".into(), path: dir.to_path_buf() }],
            sessions: vec![stored_session("s1", SessionStatus::Idle, vec![finished_item("s1-i1", "s1")], 1)],
        });
    }

    #[test]
    fn no_spawn_on_restore() {
        let (r, _) = rig();
        let dir = scratch("nospawn");
        paused(&r, &dir);
        assert_eq!(r.sessions.list_sessions()[0].status, SessionStatus::Suspended);
        assert!(r.spawns.lock().unwrap().is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn reply_to_suspended_respawns_with_resume() {
        let (r, _) = rig();
        let dir = scratch("respawn");
        paused(&r, &dir);
        r.sessions.reply("s1", "and then?").unwrap();
        assert_eq!(*r.spawns.lock().unwrap(), [(dir.clone(), Some(CLAUDE_ID.to_string()))]);
        let lines = r.fake.lines.lock().unwrap();
        assert_eq!(lines.len(), 1, "the reply is the first line written");
        let v: serde_json::Value = serde_json::from_str(&lines[0]).unwrap();
        assert_eq!(v["message"]["content"], "and then?");
        drop(lines);
        assert_eq!(r.sessions.list_sessions()[0].status, SessionStatus::Working);
        assert!(r.sessions.list_inbox()[0].resolved, "the reply resolves the finished item");
        r.feed(INIT_UUID);
        r.feed(DONE);
        assert_eq!(r.sessions.list_sessions()[0].status, SessionStatus::Idle);
        r.sessions.reply("s1", "once more").unwrap();
        assert_eq!(r.spawns.lock().unwrap().len(), 1, "a live session is not respawned");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn resume_failure_falls_back_to_ended() {
        let (r, _) = rig();
        let dir = scratch("resumefail");
        paused(&r, &dir);
        r.sessions.reply("s1", "hello again").unwrap();
        for l in RESUME_UNKNOWN_ID.lines() {
            let d: serde_json::Value = serde_json::from_str(l).unwrap();
            if d["_dir"] == "in" {
                r.feed(&d["msg"].to_string());
            }
        }
        r.sessions.on_output("s1", Output::Exit(Some(1)));
        let s = &r.sessions.list_sessions()[0];
        assert_eq!(s.status, SessionStatus::Ended);
        assert_eq!(s.claude_session_id.as_deref(), Some(CLAUDE_ID), "the id is kept");
        let errors: Vec<_> = r.sessions.list_inbox().into_iter().filter(|i| i.kind == state::InboxKind::Error).collect();
        assert_eq!(errors.len(), 1);
        assert_eq!(errors[0].summary, "Could not resume this session");
        assert!(r.fake.killed.load(Ordering::SeqCst));
        // The same outcome when the process only exits.
        let (r, _) = rig();
        paused(&r, &dir);
        r.sessions.reply("s1", "hello again").unwrap();
        r.sessions.on_output("s1", Output::Exit(Some(1)));
        assert_eq!(r.sessions.list_sessions()[0].status, SessionStatus::Ended);
        assert_eq!(r.sessions.list_inbox().iter().filter(|i| i.kind == state::InboxKind::Error).count(), 1);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn missing_project_folder_ends_without_spawn() {
        let (r, _) = rig();
        let dir = scratch("gone");
        paused(&r, &dir);
        std::fs::remove_dir_all(&dir).unwrap();
        // The folder vanished after the state was loaded: the project is re-checked on restore.
        let (r2, _) = rig();
        let snapshot = store::Snapshot {
            schema: store::SCHEMA,
            next_task: 0,
            tasks: vec![],
            revision: 1,
            next_session: 1,
            next_project: 1,
            projects: vec![crate::projects::StoredProject { id: "p1".into(), name: "proj".into(), path: dir.clone() }],
            sessions: vec![stored_session("s1", SessionStatus::Idle, vec![finished_item("s1-i1", "s1")], 1)],
        };
        r2.sessions.restore(snapshot);
        let err = r2.sessions.reply("s1", "hi").unwrap_err();
        assert!(err.contains("no longer exists") && !err.contains(':'));
        assert!(r2.spawns.lock().unwrap().is_empty());
        assert_eq!(r2.sessions.list_sessions()[0].status, SessionStatus::Ended);
        let items = r2.sessions.list_inbox();
        assert_eq!(items.iter().filter(|i| i.kind == state::InboxKind::Error).count(), 1);
        assert!(items.last().unwrap().summary.contains("no longer exists"));
        drop(r);
    }

    #[test]
    fn answering_expired_item_is_refused() {
        let (r, _) = rig();
        r.sessions.restore(Snapshot {
            schema: store::SCHEMA,
            next_task: 0,
            tasks: vec![],
            revision: 1,
            next_session: 1,
            next_project: 0,
            projects: vec![],
            sessions: vec![stored_session("s1", SessionStatus::WaitingPermission, vec![open_permission("s1-i1", "s1")], 1)],
        });
        let err = r.sessions.answer_permission("s1-i1", Decision::Allow, None).unwrap_err();
        assert!(err.contains("expired"));
        let err = r.sessions.answer_question("s1-i1", &BTreeMap::new()).unwrap_err();
        assert!(err.contains("expired"));
        assert!(r.spawns.lock().unwrap().is_empty() && r.fake.lines.lock().unwrap().is_empty());
    }
    const TEXT: &str = r#"{"type":"assistant","message":{"content":[{"type":"text","text":"working on it"}]}}"#;

    fn inbox_events(r: &Rig) -> Vec<Vec<InboxItem>> {
        r.events
            .lock()
            .unwrap()
            .iter()
            .filter_map(|e| if let Event::InboxUpdated(i) = e { Some(i.clone()) } else { None })
            .collect()
    }

    #[test]
    fn text_lines_emit_no_inbox_event_and_queue_no_save() {
        let d = scratch("textlines");
        let (r, p) = rig_with(Some(Writer::new(d.join("state.json"))));
        r.sessions.start(&p.id, "go", SessionSettings::default()).unwrap();
        r.feed(INIT_UUID);
        let (inbox_before, rev_before) = (inbox_events(&r).len(), r.sessions.lock().revision);
        for _ in 0..5 {
            r.feed(TEXT);
        }
        assert_eq!(inbox_events(&r).len(), inbox_before, "no inbox event for text lines");
        assert_eq!(r.sessions.lock().revision, rev_before, "no snapshot queued for text lines");
        assert_eq!(r.sessions.list_sessions()[0].last_message.as_deref(), Some("working on it"));
        r.feed(PERM);
        assert_eq!(inbox_events(&r).len(), inbox_before + 1, "a new item does emit");
        assert!(r.sessions.lock().revision > rev_before, "and does save");
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn inbox_event_carries_only_that_sessions_items() {
        let (r, _) = rig();
        let dir = scratch("onlyown");
        paused(&r, &dir);
        let s = r.sessions.start("p1", "go", SessionSettings::default()).unwrap();
        assert_eq!(s.id, "s2");
        r.feed(INIT_UUID);
        r.feed(PERM);
        let last = inbox_events(&r).pop().unwrap();
        assert_eq!(last.len(), 1);
        assert!(last.iter().all(|i| i.session_id == "s2"), "s1's finished item is not resent");
        assert_eq!(r.sessions.list_inbox().len(), 2);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn shutdown_saves_text_only_changes() {
        let d = scratch("textshutdown");
        let file = d.join("state.json");
        let (r, p) = rig_with(Some(Writer::new(file.clone())));
        r.sessions.start(&p.id, "go", SessionSettings::default()).unwrap();
        r.feed(INIT_UUID);
        let rev = r.sessions.lock().revision;
        r.feed(TEXT);
        assert_eq!(r.sessions.lock().revision, rev, "the text line itself saved nothing");
        r.sessions.shutdown();
        let saved = store::load(&file).unwrap().unwrap();
        assert_eq!(saved.sessions[0].state.last_message.as_deref(), Some("working on it"));
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn resume_repasses_settings() {
        let (r, p) = rig();
        let chosen = SessionSettings { model: Some(Model::Opus), permission_mode: Some(PermissionMode::Plan) };
        let s = r.sessions.start(&p.id, "go", chosen).unwrap();
        assert_eq!((s.model, s.permission_mode), (Some(Model::Opus), Some(PermissionMode::Plan)));
        assert_eq!(*r.spawn_settings.lock().unwrap(), [chosen]);

        let (r, _) = rig();
        let dir = scratch("settings");
        let mut stored = stored_session("s1", SessionStatus::Idle, vec![finished_item("s1-i1", "s1")], 1);
        stored.model = Some("sonnet".into());
        stored.permission_mode = Some("acceptEdits".into());
        r.sessions.restore(Snapshot {
            schema: store::SCHEMA,
            next_task: 0,
            tasks: vec![],
            revision: 1,
            next_session: 1,
            next_project: 1,
            projects: vec![crate::projects::StoredProject { id: "p1".into(), name: "proj".into(), path: dir.clone() }],
            sessions: vec![stored],
        });
        r.sessions.reply("s1", "again").unwrap();
        assert_eq!(
            *r.spawn_settings.lock().unwrap(),
            [SessionSettings { model: Some(Model::Sonnet), permission_mode: Some(PermissionMode::AcceptEdits) }]
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn unknown_stored_mode_loads_as_none() {
        let (r, _) = rig();
        let mut stored = stored_session("s1", SessionStatus::Idle, vec![], 0);
        stored.model = Some("haiku".into());
        stored.permission_mode = Some("bypassPermissions".into());
        r.sessions.restore(Snapshot {
            schema: store::SCHEMA,
            next_task: 0,
            tasks: vec![],
            revision: 1,
            next_session: 1,
            next_project: 0,
            projects: vec![],
            sessions: vec![stored],
        });
        let listed = r.sessions.list_sessions();
        assert_eq!(listed.len(), 1, "the session is still listed");
        assert_eq!((listed[0].model, listed[0].permission_mode), (None, None));
    }

    #[test]
    fn settings_survive_a_save_and_load() {
        let d = scratch("settingsave");
        let file = d.join("state.json");
        let (r, p) = rig_with(Some(Writer::new(file.clone())));
        let chosen = SessionSettings { model: Some(Model::Fable), permission_mode: Some(PermissionMode::Manual) };
        r.sessions.start(&p.id, "go", chosen).unwrap();
        r.sessions.shutdown();
        let saved = store::load(&file).unwrap().unwrap();
        assert_eq!((saved.sessions[0].model.as_deref(), saved.sessions[0].permission_mode.as_deref()), (Some("fable"), Some("manual")));
        let _ = std::fs::remove_dir_all(&d);
    }

    const TOOL_USE: &str = r#"{"type":"assistant","message":{"content":[{"type":"tool_use","id":"t1","name":"Bash","input":{"command":"echo SECRET_INPUT_MARKER"}}]}}"#;
    const TOOL_RESULT: &str = r#"{"type":"user","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"t1","content":"SECRET_RESULT_MARKER"}]}}"#;

    fn transcript_events(r: &Rig) -> Vec<transcript::TranscriptBatch> {
        r.events
            .lock()
            .unwrap()
            .iter()
            .filter_map(|e| if let Event::TranscriptAppended(b) = e { Some(b.clone()) } else { None })
            .collect()
    }

    #[test]
    fn transcript_text_never_in_snapshot() {
        let (r, p) = rig();
        let s = r.sessions.start(&p.id, "go", SessionSettings::default()).unwrap();
        r.feed(INIT_UUID);
        r.feed(TOOL_USE);
        r.feed(TOOL_RESULT);
        let page = r.sessions.transcript(&s.id).unwrap();
        let kinds: Vec<_> = page.entries.iter().map(|e| std::mem::discriminant(&e.entry)).collect();
        assert_eq!(kinds.len(), 3, "prompt, tool call, tool result");
        let json = serde_json::to_string(&r.sessions.lock().snapshot()).unwrap();
        assert!(!json.contains("SECRET_RESULT_MARKER") && !json.contains("SECRET_INPUT_MARKER"));
        let inbox = serde_json::to_string(&r.sessions.list_inbox()).unwrap();
        assert!(!inbox.contains("SECRET_RESULT_MARKER") && !inbox.contains("SECRET_INPUT_MARKER"));
    }

    #[test]
    fn lines_are_recorded_and_emitted_in_order() {
        let (r, p) = rig();
        let s = r.sessions.start(&p.id, "first", SessionSettings::default()).unwrap();
        r.feed(INIT_UUID);
        r.feed(TOOL_USE);
        r.feed(TOOL_RESULT);
        r.feed(DONE);
        r.sessions.reply(&s.id, "second").unwrap();
        let page = r.sessions.transcript(&s.id).unwrap();
        let seqs: Vec<u64> = page.entries.iter().map(|e| e.seq).collect();
        assert_eq!(seqs, [1, 2, 3, 4, 5]);
        assert_eq!(page.next_seq, 6);
        assert!(matches!(&page.entries[0].entry, Entry::UserPrompt { text, .. } if text == "first"));
        assert!(matches!(&page.entries[3].entry, Entry::TurnResult { .. }));
        assert!(matches!(&page.entries[4].entry, Entry::UserPrompt { text, .. } if text == "second"));
        let emitted: Vec<u64> = transcript_events(&r).iter().flat_map(|b| b.entries.iter().map(|e| e.seq)).collect();
        assert_eq!(emitted, seqs, "every recorded entry was emitted once, in order");
        assert!(transcript_events(&r).iter().all(|b| b.session_id == s.id));
        assert!(r.sessions.transcript("nope").is_err());
    }

    #[test]
    fn a_resume_records_its_reply() {
        let (r, _) = rig();
        let dir = scratch("transcriptresume");
        paused(&r, &dir);
        r.sessions.reply("s1", "and then?").unwrap();
        let page = r.sessions.transcript("s1").unwrap();
        assert!(matches!(&page.entries[0].entry, Entry::UserPrompt { text, .. } if text == "and then?"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    mod notify {
        use super::*;
        use crate::notify::NoticeKind;

        fn kinds(r: &Rig) -> Vec<NoticeKind> {
            r.notices.lock().unwrap().iter().map(|(_, k)| *k).collect()
        }

        #[test]
        fn a_session_walks_through_its_notices() {
            let (r, p) = rig();
            let s = r.sessions.start(&p.id, "go", SessionSettings::default()).unwrap();
            r.feed(INIT);
            assert!(kinds(&r).is_empty(), "starting to working is silent");
            r.feed(PERM);
            assert_eq!(kinds(&r), [NoticeKind::NeedsInput]);
            let name = std::env::temp_dir().file_name().unwrap().to_string_lossy().into_owned();
            assert_eq!(r.notices.lock().unwrap()[0].0, name, "the project name goes with it");
            let item = r.sessions.list_inbox().remove(0);
            r.sessions.answer_permission(&item.id, Decision::Allow, None).unwrap();
            assert_eq!(kinds(&r).len(), 1, "answering notifies nobody");
            r.feed(DONE);
            assert_eq!(kinds(&r), [NoticeKind::NeedsInput, NoticeKind::Finished]);
            r.sessions.reply(&s.id, "more").unwrap();
            r.feed(r#"{"type":"result","subtype":"error","is_error":true,"result":"boom"}"#);
            assert_eq!(kinds(&r).last(), Some(&NoticeKind::Errored));
        }

        #[test]
        fn a_second_request_while_waiting_is_silent() {
            let (r, p) = rig();
            r.sessions.start(&p.id, "go", SessionSettings::default()).unwrap();
            r.feed(INIT);
            r.feed(PERM);
            r.feed(&PERM.replace("r-1", "r-2"));
            assert_eq!(kinds(&r), [NoticeKind::NeedsInput]);
        }

        #[test]
        fn a_crash_notifies_but_stop_does_not() {
            let (r, p) = rig();
            let s = r.sessions.start(&p.id, "go", SessionSettings::default()).unwrap();
            r.sessions.on_output(&s.id, Output::Exit(Some(1)));
            assert_eq!(kinds(&r), [NoticeKind::Failed]);

            let (r, p) = rig();
            let s = r.sessions.start(&p.id, "go", SessionSettings::default()).unwrap();
            r.feed(INIT);
            r.sessions.stop(&s.id).unwrap();
            r.sessions.on_output(&s.id, Output::Exit(Some(1)));
            assert!(kinds(&r).is_empty(), "a stop is the user's own act");
        }

        #[test]
        fn restore_notifies_nothing() {
            let (r, _) = rig();
            let dir = scratch("notifyrestore");
            r.sessions.restore(Snapshot {
                schema: store::SCHEMA,
                next_task: 0,
                tasks: vec![],
                revision: 1,
                next_session: 2,
                next_project: 1,
                projects: vec![crate::projects::StoredProject { id: "p1".into(), name: "proj".into(), path: dir.clone() }],
                sessions: vec![
                    stored_session("s1", SessionStatus::WaitingPermission, vec![open_permission("s1-i1", "s1")], 1),
                    stored_session("s2", SessionStatus::Failed, vec![], 0),
                    stored_session("s3", SessionStatus::Idle, vec![finished_item("s3-i1", "s3")], 1),
                ],
            });
            assert!(r.notices.lock().unwrap().is_empty());
            let _ = std::fs::remove_dir_all(&dir);
        }

        #[test]
        fn shutdown_notifies_nothing() {
            let (r, p) = rig();
            let s = r.sessions.start(&p.id, "go", SessionSettings::default()).unwrap();
            r.feed(INIT);
            r.sessions.shutdown();
            // The kill shows up as an exit, and late lines may still arrive.
            r.sessions.on_output(&s.id, Output::Exit(Some(1)));
            r.feed(PERM);
            r.feed(DONE);
            assert!(r.notices.lock().unwrap().is_empty());
        }
    }

}
