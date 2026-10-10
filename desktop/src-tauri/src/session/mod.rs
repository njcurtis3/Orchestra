//! Session manager: owns projects and sessions, validates every input, and glues the
//! pure state machine (`state`) to the child process (`process`). All state is in memory.

pub mod process;
pub mod protocol;
pub mod state;

use std::collections::{BTreeMap, HashMap};
use std::path::Path;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::Serialize;

use crate::projects::{Project, Projects};
use process::{Output, Proc};
use state::{Commit, Decision, InboxItem, SessionState, SessionStatus};

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
}

#[derive(Debug, Clone)]
pub enum Event {
    SessionUpdated(SessionSummary),
    InboxUpdated(Vec<InboxItem>),
}

pub type Emit = Box<dyn Fn(Event) + Send + Sync>;
pub type Sink = Box<dyn Fn(Output) + Send>;
/// Starts a child in the given directory; every stdout line and the exit go to the sink.
pub type Spawner = Box<dyn Fn(&Path, Sink) -> Result<Arc<dyn Proc>, String> + Send + Sync>;

struct Session {
    id: String,
    project_id: String,
    project_name: String,
    started_at: u64,
    state: SessionState,
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
        }
    }
}

#[derive(Default)]
struct Inner {
    projects: Projects,
    sessions: HashMap<String, Session>,
    order: Vec<String>,
    next_session: u64,
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

    fn events_for(&self, session_id: &str) -> Vec<Event> {
        match self.sessions.get(session_id) {
            Some(s) => vec![Event::SessionUpdated(s.summary()), Event::InboxUpdated(self.inbox())],
            None => Vec::new(),
        }
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
}

impl Sessions {
    pub fn new(emit: Emit, spawner: Spawner) -> Arc<Self> {
        Arc::new(Self { inner: Mutex::new(Inner::default()), emit_order: Mutex::new(()), emit, spawner })
    }

    fn lock(&self) -> MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Compute the events for a session under `inner`, take the emit lock before letting
    /// `inner` go, then fire. Two publishers therefore fire in the order they computed.
    fn publish(&self, inner: MutexGuard<'_, Inner>, session_id: &str) {
        let events = inner.events_for(session_id);
        let order = self.emit_order.lock().unwrap_or_else(|e| e.into_inner());
        drop(inner);
        for e in events {
            (self.emit)(e);
        }
        drop(order);
    }

    pub fn add_project(&self, path: &Path) -> Result<Project, String> {
        self.lock().projects.add(path)
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

    pub fn start(self: &Arc<Self>, project_id: &str, prompt: &str) -> Result<SessionSummary, String> {
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
                state: SessionState::new(&id, now),
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
        let proc = match (self.spawner)(&path, sink) {
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
        let now = now_ms();
        if let Some(s) = inner.sessions.get_mut(session_id) {
            match out {
                Output::Line(line) => s.state.apply(protocol::parse_line(&line), now),
                Output::Exit(code) => s.state.mark_exit(code, now),
            }
        }
        self.publish(inner, session_id);
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
        let sid = self.lock().session_of_item(item_id).ok_or("unknown or resolved item")?;
        self.send_through(&sid, build)
    }

    pub fn reply(&self, session_id: &str, text: &str) -> Result<(), String> {
        check_text(text)?;
        self.send_through(session_id, |state| state.reply_line(text))
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

    /// App exit: no child outlives the app.
    pub fn kill_all(&self) {
        let procs: Vec<Arc<dyn Proc>> = self.lock().sessions.values().filter_map(|s| s.proc.clone()).collect();
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
    }

    fn rig() -> (Rig, Project) {
        let fake = Arc::new(Fake {
            lines: Mutex::new(vec![]),
            killed: AtomicBool::new(false),
            fail: AtomicBool::new(false),
            on_write: Mutex::new(None),
        });
        let sink: Arc<Mutex<Option<Sink>>> = Arc::new(Mutex::new(None));
        let events = Arc::new(Mutex::new(vec![]));
        let (f2, s2, e2) = (fake.clone(), sink.clone(), events.clone());
        let sessions = Sessions::new(
            Box::new(move |e| e2.lock().unwrap().push(e)),
            Box::new(move |_dir, sk| {
                *s2.lock().unwrap() = Some(sk);
                Ok(f2.clone() as Arc<dyn Proc>)
            }),
        );
        let project = sessions.add_project(&std::env::temp_dir()).unwrap();
        (Rig { sessions, fake, sink, events }, project)
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
        let err = r.sessions.start("nope", "hi").unwrap_err();
        assert_eq!(err, "unknown project");
        assert!(!err.contains('/') && !err.contains('\\') && !err.contains("Users") && !err.contains(":"));
    }

    #[test]
    fn rejects_empty_and_oversize_prompt() {
        let (r, p) = rig();
        assert!(r.sessions.start(&p.id, "").is_err());
        assert!(r.sessions.start(&p.id, "   ").is_err());
        assert!(r.sessions.start(&p.id, &"a".repeat(MAX_TEXT_BYTES + 1)).is_err());
        assert!(r.sessions.start(&p.id, &"a".repeat(MAX_TEXT_BYTES)).is_ok());
    }

    #[test]
    fn start_sends_prompt_over_stdin_and_emits() {
        let (r, p) = rig();
        let s = r.sessions.start(&p.id, "hello").unwrap();
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
        r.sessions.start(&p.id, "go").unwrap();
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
        r.sessions.start(&p.id, "go").unwrap();
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
        let s = r.sessions.start(&p.id, "go").unwrap();
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
    fn stop_kills_child_and_kill_all_covers_app_exit() {
        let (r, p) = rig();
        let s = r.sessions.start(&p.id, "go").unwrap();
        r.sessions.stop(&s.id).unwrap();
        assert!(r.fake.killed.load(Ordering::SeqCst));
        assert_eq!(r.sessions.list_sessions()[0].status, SessionStatus::Ended);
        assert!(r.sessions.stop("nope").is_err());

        let (r2, p2) = rig();
        r2.sessions.start(&p2.id, "go").unwrap();
        r2.sessions.kill_all();
        assert!(r2.fake.killed.load(Ordering::SeqCst));
    }

    #[test]
    fn start_publishes_post_send_summary_in_order() {
        let (r, p) = rig();
        r.feed_on_write(1, INIT);
        let s = r.sessions.start(&p.id, "go").unwrap();
        assert_eq!(s.status, SessionStatus::Working);
        let updates = r.updates();
        assert_eq!(updates.last().unwrap().status, SessionStatus::Working);
        assert!(updates.iter().all(|u| u.status == SessionStatus::Working), "no stale Starting event");
    }

    #[test]
    fn failed_answer_send_leaves_item_pending() {
        let (r, p) = rig();
        r.sessions.start(&p.id, "go").unwrap();
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
        let s = r.sessions.start(&p.id, "go").unwrap();
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
        let s = r.sessions.start(&p.id, "go").unwrap();
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
        r.sessions.start(&p.id, "go").unwrap();
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
        let s = r.sessions.start(&p.id, "go").unwrap();
        r.sessions.on_output(&s.id, Output::Exit(Some(1)));
        assert_eq!(r.sessions.list_sessions()[0].status, SessionStatus::Failed);
        assert!(r.sessions.list_inbox().iter().any(|i| i.kind == state::InboxKind::Error));
    }
}
