//! Session manager: owns projects and sessions, validates every input, and glues the
//! pure state machine (`state`) to the child process (`process`). State lives in memory and
//! is mirrored to disk through `store` after every change.

pub mod process;
pub mod protocol;
pub mod settings;
pub mod state;
pub mod store;

use std::collections::{BTreeMap, HashMap};
use std::path::Path;
use std::sync::{Arc, Mutex, MutexGuard, Weak};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::Serialize;

use crate::projects::{Project, Projects};
use process::{Output, Proc, SpawnArgs};
use settings::{Model, PermissionMode, SessionSettings};
use state::{Commit, Decision, InboxItem, SessionState, SessionStatus};
use store::{Snapshot, StoredSession, Writer};

/// Largest prompt, reply or permission message accepted over IPC, in bytes.
pub const MAX_TEXT_BYTES: usize = 32768;
const MAX_ANSWERS: usize = 32;
const MAX_ANSWER_BYTES: usize = 4096;

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
}

pub type Emit = Box<dyn Fn(Event) + Send + Sync>;
pub type Sink = Box<dyn Fn(Output) + Send>;
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
                    state: s.state.to_persisted(),
                })
                .collect(),
        }
    }

    /// The events for a session, and whether its status, inbox or claude id changed since the
    /// last call. The inbox event carries only this session's items and only when its inbox
    /// changed.
    fn events_for(&mut self, session_id: &str) -> (Vec<Event>, bool) {
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
    /// `None` when saving is off: tests, or a state file this build must not touch.
    writer: Option<Writer>,
    /// Lets `&self` methods hand an owning handle to a reader thread.
    me: Weak<Sessions>,
}

impl Sessions {
    pub fn with_writer(emit: Emit, spawner: Spawner, writer: Option<Writer>) -> Arc<Self> {
        Arc::new_cyclic(|me| Self {
            inner: Mutex::new(Inner::default()),
            emit_order: Mutex::new(()),
            emit,
            spawner,
            writer,
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
                    gate: Arc::new(Mutex::new(())),
                },
            );
        }
        inner.next_session = next_session;
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
        self.publish_saving(inner, session_id, true);
    }

    /// `publish` for a stdout line: the snapshot is skipped when only text changed.
    fn publish_line(&self, inner: MutexGuard<'_, Inner>, session_id: &str) {
        self.publish_saving(inner, session_id, false);
    }

    fn publish_saving(&self, mut inner: MutexGuard<'_, Inner>, session_id: &str, force: bool) {
        let (events, significant) = inner.events_for(session_id);
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

    pub fn start(
        self: &Arc<Self>,
        project_id: &str,
        prompt: &str,
        settings: SessionSettings,
    ) -> Result<SessionSummary, String> {
        check_text(prompt)?;
        let now = now_ms();
        let (session_id, path) = {
            let mut inner = self.lock();
            let entry = inner.projects.get(project_id).ok_or("unknown project")?;
            let (path, name) = (entry.path.clone(), entry.project.name.clone());
            inner.next_session += 1;
            let id = format!("s{}", inner.next_session);
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
                gate: Arc::new(Mutex::new(())),
            };
            inner.sessions.insert(id.clone(), session);
            inner.order.push(id.clone());
            (id, path)
        };

        let this = Arc::clone(self);
        let sid = session_id.clone();
        let sink: Sink = Box::new(move |out| this.on_output(&sid, out));
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
        // The reader may already have moved the session past Starting while the prompt was
        // being written, so read the summary now, under the lock, and publish it in order.
        let inner = self.lock();
        let summary = inner.sessions.get(&session_id).map(Session::summary).ok_or("unknown session")?;
        self.publish(inner, &session_id);
        Ok(summary)
    }

    /// Entry point for the reader thread: one stdout line, or the exit.
    pub fn on_output(&self, session_id: &str, out: Output) {
        let mut inner = self.lock();
        if inner.shutting_down {
            // The children are being killed on purpose; their exit is not a failure.
            return;
        }
        let now = now_ms();
        if let Some(s) = inner.sessions.get_mut(session_id) {
            match out {
                Output::Line(line) => s.state.apply(protocol::parse_line(&line), now),
                Output::Exit(code) => s.state.mark_exit(code, now),
            }
        }
        // A line can end a session only when a resume failed; the process has no more use.
        let dead = inner
            .sessions
            .get(session_id)
            .filter(|s| matches!(s.state.status, SessionStatus::Ended | SessionStatus::Failed))
            .and_then(|s| s.proc.clone());
        self.publish_line(inner, session_id);
        if let Some(p) = dead {
            p.kill();
        }
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
        self.send_locked(session_id, |state| state.reply_line(text))
    }

    /// A reply to a paused session starts a new process with `--resume` and sends the reply
    /// as its first user line. Called with the session's gate held; `Inner` is not held
    /// across the spawn or the write.
    fn resume_with(&self, session_id: &str, text: &str) -> Result<(), String> {
        let mut inner = self.lock();
        let s = inner.sessions.get(session_id).ok_or("unknown session")?;
        let settings = s.settings;
        let Some(path) = inner.projects.get(&s.project_id).map(|e| e.path.clone()) else {
            if let Some(s) = inner.sessions.get_mut(session_id) {
                s.state.fail_resume("The project folder no longer exists", now_ms());
            }
            self.publish(inner, session_id);
            return Err("project folder no longer exists".into());
        };
        let plan = inner.sessions.get_mut(session_id).ok_or("unknown session")?.state.begin_resume(now_ms())?;
        // Working is shown before the process exists; from here the reader can only move it on.
        self.publish(inner, session_id);

        let this = self.me.upgrade().ok_or("app is closing")?;
        let sid = session_id.to_string();
        let sink: Sink = Box::new(move |out| this.on_output(&sid, out));
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
        let mut inner = self.lock();
        if let Some(s) = inner.sessions.get_mut(session_id) {
            s.state.commit_resume(&plan.resolve);
        }
        self.publish(inner, session_id);
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
        self.send_locked(session_id, build)
    }

    /// `send_through` with the session's gate already held.
    fn send_locked(
        &self,
        session_id: &str,
        build: impl FnOnce(&SessionState) -> Result<(String, Commit), String>,
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
        let mut inner = self.lock();
        if let Some(s) = inner.sessions.get_mut(session_id) {
            s.state.commit(commit, now_ms());
        }
        self.publish(inner, session_id);
        Ok(())
    }

    pub fn stop(&self, session_id: &str) -> Result<(), String> {
        let mut inner = self.lock();
        let s = inner.sessions.get_mut(session_id).ok_or("unknown session")?;
        s.state.mark_stopped(now_ms());
        let proc = s.proc.clone();
        self.publish(inner, session_id);
        if let Some(p) = proc {
            p.kill();
        }
        Ok(())
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
    use std::sync::atomic::{AtomicBool, Ordering};

    /// Called with the number of lines written so far, after each successful write.
    type OnWrite = Box<dyn Fn(usize) + Send>;

    struct Fake {
        lines: Mutex<Vec<String>>,
        killed: AtomicBool,
        fail: AtomicBool,
        on_write: Mutex<Option<OnWrite>>,
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
    }

    struct Rig {
        sessions: Arc<Sessions>,
        fake: Arc<Fake>,
        sink: Arc<Mutex<Option<Sink>>>,
        events: Arc<Mutex<Vec<Event>>>,
        /// Every spawn: the directory and the id passed to `--resume`, if any.
        spawns: Arc<Mutex<Vec<(std::path::PathBuf, Option<String>)>>>,
        /// The settings of every spawn, in order.
        spawn_settings: Arc<Mutex<Vec<SessionSettings>>>,
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
        });
        let sink: Arc<Mutex<Option<Sink>>> = Arc::new(Mutex::new(None));
        let events = Arc::new(Mutex::new(vec![]));
        let spawns = Arc::new(Mutex::new(vec![]));
        let spawn_settings = Arc::new(Mutex::new(vec![]));
        let (f2, s2, e2, p2, g2) = (fake.clone(), sink.clone(), events.clone(), spawns.clone(), spawn_settings.clone());
        let sessions = Sessions::with_writer(
            Box::new(move |e| e2.lock().unwrap().push(e)),
            Box::new(move |dir, spawn, sk| {
                p2.lock().unwrap().push((dir.to_path_buf(), spawn.resume.map(|r| r.as_str().to_string())));
                g2.lock().unwrap().push(spawn.settings);
                *s2.lock().unwrap() = Some(sk);
                Ok(f2.clone() as Arc<dyn Proc>)
            }),
            writer,
        );
        let project = sessions.add_project(&std::env::temp_dir()).unwrap();
        (Rig { sessions, fake, sink, events, spawns, spawn_settings }, project)
    }

    impl Rig {
        fn feed(&self, line: &str) {
            let guard = self.sink.lock().unwrap();
            (guard.as_ref().unwrap())(Output::Line(line.to_string()));
        }

        /// Feed `line` through the sink while the nth write is in flight.
        fn feed_on_write(&self, nth: usize, line: &'static str) {
            let sink = self.sink.clone();
            *self.fake.on_write.lock().unwrap() = Some(Box::new(move |n| {
                if n == nth {
                    let guard = sink.lock().unwrap();
                    (guard.as_ref().unwrap())(Output::Line(line.to_string()));
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
        assert!(r.fake.lines.lock().unwrap().is_empty() && r.sink.lock().unwrap().is_none(), "nothing spawned");
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

}
