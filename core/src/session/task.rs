//! A task: a prompt that runs in its own git worktree of a project, queued behind a small
//! concurrency limit. A task is its own entity beside the sessions: it has at most one
//! session, and its status is derived from that session by `sync`, never stored twice.
//! No I/O here, so the transitions are tested directly.
//!
//! The worktree path is held here, in Rust. `TaskSummary` is all the frontend ever sees,
//! and it carries no path.

use std::path::PathBuf;

use serde::Serialize;

use super::settings::SessionSettings;
use super::state::{InboxItem, InboxKind, SessionStatus, truncate};
use super::store::StoredTask;

/// Tasks running or waiting on the owner that may exist at once (gate G7). Plain sessions
/// are not counted.
pub const MAX_RUNNING_TASKS: usize = 2;
const MAX_TITLE_CHARS: usize = 120;
pub const PAUSED_NOTE: &str = "Paused when Orchestra closed; reply to continue";
/// Notes that tell a cleanup retry what the task was heading for, since the status alone
/// (`cleanup_pending`) does not. A landed task's note starts with `LANDED`.
pub const LANDED: &str = "Landed";
pub const DISCARDED: &str = "Discarded";
pub const PENDING_SUFFIX: &str = "; cleanup pending";

/// Which way a task in `cleanup_pending` was going, read back from its note.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    Landed,
    Discarded,
}

impl Outcome {
    /// An unrecognised note reads as discarded's opposite on purpose: the safe default is
    /// the branch delete that refuses unmerged work.
    pub fn of_note(note: Option<&str>) -> Self {
        match note {
            Some(n) if n.starts_with(DISCARDED) => Outcome::Discarded,
            _ => Outcome::Landed,
        }
    }
}

/// Closed set, deliberately not `SessionStatus`: queued, landed and discarded have no
/// session, and idle (a turn is over) is not done (the task has been handed back).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskStatus {
    Queued,
    Running,
    NeedsInput,
    Done,
    Landing,
    Landed,
    Discarded,
    CleanupPending,
    Error,
}

impl TaskStatus {
    pub const ALL: [TaskStatus; 9] = [
        TaskStatus::Queued,
        TaskStatus::Running,
        TaskStatus::NeedsInput,
        TaskStatus::Done,
        TaskStatus::Landing,
        TaskStatus::Landed,
        TaskStatus::Discarded,
        TaskStatus::CleanupPending,
        TaskStatus::Error,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            TaskStatus::Queued => "queued",
            TaskStatus::Running => "running",
            TaskStatus::NeedsInput => "needs_input",
            TaskStatus::Done => "done",
            TaskStatus::Landing => "landing",
            TaskStatus::Landed => "landed",
            TaskStatus::Discarded => "discarded",
            TaskStatus::CleanupPending => "cleanup_pending",
            TaskStatus::Error => "error",
        }
    }

    /// A stored value that is not in the set is `None`.
    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|t| t.as_str() == s)
    }
}

pub struct Task {
    pub id: String,
    pub project_id: String,
    pub project_name: String,
    pub prompt: String,
    pub settings: SessionSettings,
    pub created_at: u64,
    pub status: TaskStatus,
    pub session_id: Option<String>,
    /// Derived and set by Rust only; never leaves it.
    pub worktree: Option<PathBuf>,
    pub branch: Option<String>,
    pub base_branch: String,
    pub base_commit: String,
    pub note: Option<String>,
    /// The last turn ended in an error rather than a finished result.
    pub last_turn_error: bool,
    /// The session cannot take a follow-up (it ended or failed).
    pub session_over: bool,
    /// Claimed by the queue pump and not yet linked to a session. Not persisted.
    pub starting: bool,
    /// A land, discard, diff or cleanup retry is running on this task. Not persisted.
    pub busy: bool,
}

/// What the frontend may know about a task: no path, ever.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct TaskSummary {
    pub id: String,
    pub project_id: String,
    pub project_name: String,
    pub status: TaskStatus,
    pub title: String,
    pub created_at: u64,
    pub session_id: Option<String>,
    pub branch: Option<String>,
    pub base_branch: String,
    pub note: Option<String>,
    pub last_turn_error: bool,
    pub session_over: bool,
}

/// The first line of the prompt, capped.
pub fn title(prompt: &str) -> String {
    let first = prompt.lines().map(str::trim).find(|l| !l.is_empty()).unwrap_or("");
    truncate(first, MAX_TITLE_CHARS)
}

impl Task {
    pub fn new(
        id: String,
        project_id: String,
        project_name: String,
        prompt: String,
        settings: SessionSettings,
        base_branch: String,
        base_commit: String,
        now: u64,
    ) -> Self {
        Self {
            id,
            project_id,
            project_name,
            prompt,
            settings,
            created_at: now,
            status: TaskStatus::Queued,
            session_id: None,
            worktree: None,
            branch: None,
            base_branch,
            base_commit,
            note: None,
            last_turn_error: false,
            session_over: false,
            starting: false,
            busy: false,
        }
    }

    pub fn summary(&self) -> TaskSummary {
        TaskSummary {
            id: self.id.clone(),
            project_id: self.project_id.clone(),
            project_name: self.project_name.clone(),
            status: self.status,
            title: title(&self.prompt),
            created_at: self.created_at,
            session_id: self.session_id.clone(),
            branch: self.branch.clone(),
            base_branch: self.base_branch.clone(),
            note: self.note.clone(),
            last_turn_error: self.last_turn_error,
            session_over: self.session_over,
        }
    }

    pub fn to_stored(&self) -> StoredTask {
        StoredTask {
            id: self.id.clone(),
            project_id: self.project_id.clone(),
            project_name: self.project_name.clone(),
            prompt: self.prompt.clone(),
            model: self.settings.model.map(|m| m.as_str().to_string()),
            permission_mode: self.settings.permission_mode.map(|m| m.as_str().to_string()),
            created_at: self.created_at,
            status: self.status.as_str().to_string(),
            session_id: self.session_id.clone(),
            worktree: self.worktree.clone(),
            branch: self.branch.clone(),
            base_branch: self.base_branch.clone(),
            base_commit: self.base_commit.clone(),
            note: self.note.clone(),
            last_turn_error: self.last_turn_error,
            session_over: self.session_over,
        }
    }

    /// Counts against `MAX_RUNNING_TASKS`: claimed by the pump, running, or waiting on the
    /// owner with a live session. A paused task (its session suspended) does not count
    /// until it is replied to.
    pub fn is_active(&self, session_suspended: bool) -> bool {
        self.starting
            || self.status == TaskStatus::Running
            || (self.status == TaskStatus::NeedsInput && !session_suspended)
    }

    pub fn fail(&mut self, note: &str) {
        self.status = TaskStatus::Error;
        self.starting = false;
        self.note = Some(note.to_string());
    }
}

impl StoredTask {
    /// Back from the file. An unknown status loads as `error` rather than failing the whole
    /// file. Checking the worktree is the caller's job: it needs the base folder.
    pub fn into_task(self) -> Task {
        let parsed = TaskStatus::parse(&self.status);
        let mut task = Task {
            id: self.id,
            project_id: self.project_id,
            project_name: self.project_name,
            prompt: self.prompt,
            settings: SessionSettings::from_stored(self.model.as_deref(), self.permission_mode.as_deref()),
            created_at: self.created_at,
            status: parsed.unwrap_or(TaskStatus::Error),
            session_id: self.session_id,
            worktree: self.worktree,
            branch: self.branch,
            base_branch: self.base_branch,
            base_commit: self.base_commit,
            note: self.note,
            last_turn_error: self.last_turn_error,
            session_over: self.session_over,
            starting: false,
            busy: false,
        };
        if parsed.is_none() {
            task.note = Some("unknown status".to_string());
        }
        task
    }
}

/// Whether the last finished turn was an error, from the newest Finished or Error item.
pub fn last_turn_is_error(inbox: &[InboxItem]) -> bool {
    inbox
        .iter()
        .rev()
        .find(|i| matches!(i.kind, InboxKind::Finished | InboxKind::Error))
        .is_some_and(|i| i.kind == InboxKind::Error)
}

/// Follow the task's session. Only running, needs_input and done tasks follow it; every
/// other status is set explicitly and left alone here.
///
/// Idle is a turn that is over, which is `done` (the child is suspended at once, so
/// Suspended after a turn is `done` too). Suspended with a task that was waiting on the
/// owner stays as it was: that is a task paused by a restart.
pub fn sync(task: &mut Task, session: SessionStatus, last_turn_error: bool) {
    if !matches!(task.status, TaskStatus::Running | TaskStatus::NeedsInput | TaskStatus::Done) {
        return;
    }
    let next = match session {
        SessionStatus::Starting | SessionStatus::Working => TaskStatus::Running,
        SessionStatus::WaitingPermission | SessionStatus::WaitingQuestion | SessionStatus::NeedsAttention => {
            TaskStatus::NeedsInput
        }
        SessionStatus::Idle | SessionStatus::Ended | SessionStatus::Failed => TaskStatus::Done,
        SessionStatus::Suspended => match task.status {
            TaskStatus::NeedsInput => TaskStatus::NeedsInput,
            _ => TaskStatus::Done,
        },
    };
    if next != task.status {
        task.note = None;
    }
    task.status = next;
    task.session_over = matches!(session, SessionStatus::Ended | SessionStatus::Failed);
    if next == TaskStatus::Done {
        task.last_turn_error = last_turn_error;
    } else if next == TaskStatus::Running {
        task.last_turn_error = false;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn task(status: TaskStatus) -> Task {
        let mut t = Task::new(
            "t1".into(),
            "p1".into(),
            "proj".into(),
            "do it".into(),
            SessionSettings::default(),
            "main".into(),
            "a".repeat(40),
            1,
        );
        t.status = status;
        t
    }

    #[test]
    fn task_status_does_not_reuse_session_status() {
        let sessions = [
            SessionStatus::Starting,
            SessionStatus::Working,
            SessionStatus::WaitingPermission,
            SessionStatus::WaitingQuestion,
            SessionStatus::NeedsAttention,
            SessionStatus::Idle,
            SessionStatus::Suspended,
            SessionStatus::Ended,
            SessionStatus::Failed,
        ];
        for s in sessions {
            let name = serde_json::to_value(s).unwrap();
            assert_eq!(TaskStatus::parse(name.as_str().unwrap()), None, "{name}");
        }
        for t in TaskStatus::ALL {
            assert_eq!(TaskStatus::parse(t.as_str()), Some(t));
            assert_eq!(serde_json::to_value(t).unwrap(), t.as_str());
        }
        assert_eq!(TaskStatus::parse("idle"), None);
        assert_eq!(TaskStatus::ALL.len(), 9);
    }

    #[test]
    fn sync_maps_a_session_onto_the_task() {
        let cases = [
            (TaskStatus::Running, SessionStatus::WaitingQuestion, TaskStatus::NeedsInput, false),
            (TaskStatus::NeedsInput, SessionStatus::Working, TaskStatus::Running, false),
            (TaskStatus::Running, SessionStatus::Idle, TaskStatus::Done, false),
            (TaskStatus::Running, SessionStatus::Suspended, TaskStatus::Done, false),
            (TaskStatus::Done, SessionStatus::Suspended, TaskStatus::Done, false),
            (TaskStatus::NeedsInput, SessionStatus::Suspended, TaskStatus::NeedsInput, false),
            (TaskStatus::Done, SessionStatus::Working, TaskStatus::Running, false),
            (TaskStatus::Running, SessionStatus::Failed, TaskStatus::Done, true),
            (TaskStatus::Running, SessionStatus::Ended, TaskStatus::Done, true),
        ];
        for (from, session, to, over) in cases {
            let mut t = task(from);
            sync(&mut t, session, false);
            assert_eq!((t.status, t.session_over), (to, over), "{from:?} + {session:?}");
        }
    }

    #[test]
    fn sync_leaves_explicit_statuses_alone() {
        for s in [TaskStatus::Queued, TaskStatus::Landing, TaskStatus::Landed, TaskStatus::Discarded, TaskStatus::CleanupPending, TaskStatus::Error] {
            let mut t = task(s);
            sync(&mut t, SessionStatus::Working, false);
            assert_eq!(t.status, s);
        }
    }

    #[test]
    fn done_keeps_the_error_flag_of_the_last_turn() {
        let mut t = task(TaskStatus::Running);
        sync(&mut t, SessionStatus::Idle, true);
        assert!(t.last_turn_error);
        sync(&mut t, SessionStatus::Working, false);
        assert!(!t.last_turn_error && t.status == TaskStatus::Running);
    }

    #[test]
    fn title_is_first_line_capped() {
        assert_eq!(title("\n  fix the bug  \nmore"), "fix the bug");
        assert_eq!(title(&"a".repeat(500)).chars().count(), MAX_TITLE_CHARS);
    }

    #[test]
    fn stored_task_roundtrips_and_unknown_status_is_error() {
        let mut t = task(TaskStatus::Done);
        t.worktree = Some(PathBuf::from("x"));
        let back = t.to_stored().into_task();
        assert_eq!((back.status, back.worktree.clone()), (TaskStatus::Done, t.worktree.clone()));
        let mut stored = t.to_stored();
        stored.status = "frozen".into();
        let back = stored.into_task();
        assert_eq!((back.status, back.note.as_deref()), (TaskStatus::Error, Some("unknown status")));
    }

    #[test]
    fn paused_task_does_not_count() {
        let mut t = task(TaskStatus::NeedsInput);
        assert!(t.is_active(false) && !t.is_active(true));
        t.status = TaskStatus::Running;
        assert!(t.is_active(false));
        t.status = TaskStatus::Done;
        assert!(!t.is_active(false));
        t.status = TaskStatus::Queued;
        t.starting = true;
        assert!(t.is_active(false));
    }

    #[test]
    fn outcome_is_read_from_the_note() {
        assert_eq!(Outcome::of_note(Some("Discarded; cleanup pending")), Outcome::Discarded);
        assert_eq!(Outcome::of_note(Some("Landed as abc1234; cleanup pending")), Outcome::Landed);
        assert_eq!(Outcome::of_note(None), Outcome::Landed);
    }
}
