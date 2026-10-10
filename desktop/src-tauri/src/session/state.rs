//! Pure state machine for one session: protocol messages in, `SessionStatus` and inbox
//! items out. No I/O, no clock (callers pass `now`), so it replays fixtures directly.

use std::collections::{BTreeMap, HashMap};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::protocol::{self, Message};

const MAX_MESSAGE_CHARS: usize = 500;
const MAX_SUMMARY_CHARS: usize = 300;
const MAX_PREVIEW_CHARS: usize = 2000;
/// Cap on the full result text kept on a Finished/Error item.
pub const MAX_RESULT_BYTES: usize = 262_144;
const MAX_SUBTYPE_CHARS: usize = 64;
const DENY_DEFAULT: &str = "User denied";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionStatus {
    Starting,
    Working,
    WaitingPermission,
    WaitingQuestion,
    /// Claude sent a request Orchestra cannot answer; the session may be stuck. Sticky
    /// until the turn ends (`Result`) or the session is over.
    NeedsAttention,
    Idle,
    /// Not running, but holds a valid claude session id so a reply can resume it.
    Suspended,
    Ended,
    Failed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InboxKind {
    Permission,
    Question,
    Finished,
    Error,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Decision {
    Allow,
    Deny,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct QuestionOption {
    pub label: String,
    pub description: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Question {
    pub question: String,
    pub header: Option<String>,
    pub multi_select: bool,
    pub options: Vec<QuestionOption>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct InboxItem {
    pub id: String,
    pub session_id: String,
    pub kind: InboxKind,
    pub created_at: u64,
    /// Monotonic: once true it stays true.
    pub resolved: bool,
    /// The request was open when Orchestra closed and can no longer be answered.
    /// Implies `resolved`.
    pub expired: bool,
    pub summary: String,
    pub tool_name: Option<String>,
    pub tool_input_preview: Option<String>,
    pub questions: Option<Vec<Question>>,
    /// Full result text on Finished/Error items from a `Result`, capped at `MAX_RESULT_BYTES`.
    pub result_text: Option<String>,
}

/// An open `can_use_tool` request waiting for an answer.
struct Pending {
    request_id: String,
    input: Value,
}

pub struct SessionState {
    session_id: String,
    pub status: SessionStatus,
    pub last_activity_at: u64,
    pub last_message: Option<String>,
    pub claude_session_id: Option<String>,
    pub inbox: Vec<InboxItem>,
    /// Open requests by inbox item id, in arrival order for `status` recomputation.
    pending: HashMap<String, Pending>,
    pending_order: Vec<String>,
    pub unknown_count: u32,
    next_item: u64,
    /// Bumped whenever status or the open requests change, so a reply built earlier can
    /// tell whether the reader got there first.
    epoch: u64,
}

/// What survives a restart of one session. `Pending` is deliberately absent: an open
/// request cannot be answered once its process is gone.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PersistedSession {
    pub status: SessionStatus,
    pub last_activity_at: u64,
    pub last_message: Option<String>,
    pub claude_session_id: Option<String>,
    pub inbox: Vec<InboxItem>,
    pub next_item: u64,
    pub unknown_count: u32,
}

/// What to apply once the line built by `permission_line`, `question_line` or `reply_line`
/// has been written to the child.
#[derive(Debug)]
pub enum Commit {
    Answer { item_id: String },
    Reply { resolve: Vec<String>, epoch: u64 },
}

pub fn truncate(s: &str, max_chars: usize) -> String {
    if s.chars().count() <= max_chars {
        return s.to_string();
    }
    s.chars().take(max_chars).collect()
}

/// Cut to at most `max_bytes` on a char boundary.
pub fn truncate_bytes(s: &str, max_bytes: usize) -> String {
    if s.len() <= max_bytes {
        return s.to_string();
    }
    let mut end = max_bytes;
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    s[..end].to_string()
}

/// Canonical 8-4-4-4-12 hex UUID. Also keeps a leading dash out of the `--resume` argument.
pub fn is_uuid(s: &str) -> bool {
    s.len() == 36
        && s.bytes().enumerate().all(|(i, b)| match i {
            8 | 13 | 18 | 23 => b == b'-',
            _ => b.is_ascii_hexdigit(),
        })
}

fn parse_questions(input: &Value) -> Vec<Question> {
    let str_of = |v: &Value, k: &str| v.get(k).and_then(Value::as_str).map(str::to_string);
    input
        .get("questions")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .map(|q| Question {
            question: str_of(q, "question").unwrap_or_default(),
            header: str_of(q, "header"),
            multi_select: q.get("multiSelect").and_then(Value::as_bool).unwrap_or(false),
            options: q
                .get("options")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .map(|o| QuestionOption {
                    label: str_of(o, "label").unwrap_or_default(),
                    description: str_of(o, "description"),
                })
                .collect(),
        })
        .collect()
}

impl SessionState {
    pub fn new(session_id: &str, now: u64) -> Self {
        Self {
            session_id: session_id.to_string(),
            status: SessionStatus::Starting,
            last_activity_at: now,
            last_message: None,
            claude_session_id: None,
            inbox: Vec::new(),
            pending: HashMap::new(),
            pending_order: Vec::new(),
            unknown_count: 0,
            next_item: 0,
            epoch: 0,
        }
    }

    pub fn to_persisted(&self) -> PersistedSession {
        PersistedSession {
            status: self.status,
            last_activity_at: self.last_activity_at,
            last_message: self.last_message.clone(),
            claude_session_id: self.claude_session_id.clone(),
            inbox: self.inbox.clone(),
            next_item: self.next_item,
            unknown_count: self.unknown_count,
        }
    }

    pub fn from_persisted(session_id: &str, p: PersistedSession) -> Self {
        Self {
            session_id: session_id.to_string(),
            status: p.status,
            last_activity_at: p.last_activity_at,
            last_message: p.last_message,
            claude_session_id: p.claude_session_id.filter(|id| is_uuid(id)),
            inbox: p.inbox,
            pending: HashMap::new(),
            pending_order: Vec::new(),
            unknown_count: p.unknown_count,
            next_item: p.next_item,
            epoch: 0,
        }
    }

    pub fn has_pending(&self, item_id: &str) -> bool {
        self.pending.contains_key(item_id)
    }

    fn is_over(&self) -> bool {
        matches!(self.status, SessionStatus::Ended | SessionStatus::Failed)
    }

    fn push_item(
        &mut self,
        kind: InboxKind,
        now: u64,
        summary: String,
        tool_name: Option<String>,
        tool_input_preview: Option<String>,
        questions: Option<Vec<Question>>,
        result_text: Option<String>,
    ) -> String {
        self.next_item += 1;
        let id = format!("{}-i{}", self.session_id, self.next_item);
        self.inbox.push(InboxItem {
            id: id.clone(),
            session_id: self.session_id.clone(),
            kind,
            created_at: now,
            resolved: false,
            expired: false,
            summary,
            tool_name,
            tool_input_preview,
            questions,
            result_text,
        });
        id
    }

    fn resolve(&mut self, item_id: &str) {
        if let Some(i) = self.inbox.iter_mut().find(|i| i.id == item_id) {
            i.resolved = true;
        }
    }

    /// Waiting while any request is open, otherwise working. `NeedsAttention` is left
    /// alone: only a `Result` proves claude got past the request it could not answer.
    fn recompute(&mut self) {
        if self.status == SessionStatus::NeedsAttention {
            return;
        }
        self.status = match self.pending_order.first().and_then(|id| self.inbox.iter().find(|i| &i.id == id)) {
            Some(i) if i.kind == InboxKind::Question => SessionStatus::WaitingQuestion,
            Some(_) => SessionStatus::WaitingPermission,
            None => SessionStatus::Working,
        };
    }

    fn close_pending(&mut self, item_id: &str) {
        self.pending.remove(item_id);
        self.pending_order.retain(|i| i != item_id);
        self.resolve(item_id);
    }

    pub fn apply(&mut self, msg: Message, now: u64) {
        if self.is_over() {
            return;
        }
        self.last_activity_at = now;
        match msg {
            Message::Init { session_id } => {
                if let Some(id) = session_id.filter(|id| is_uuid(id)) {
                    self.claude_session_id = Some(id);
                }
                if self.status == SessionStatus::Starting {
                    self.status = SessionStatus::Working;
                }
            }
            Message::Assistant { text, tool_name } => {
                if !text.trim().is_empty() {
                    self.last_message = Some(truncate(&text, MAX_MESSAGE_CHARS));
                } else if let Some(t) = tool_name {
                    self.last_message = Some(truncate(&format!("Using {t}"), MAX_MESSAGE_CHARS));
                }
            }
            Message::CanUseTool { request_id, tool_name, input } => {
                if self.pending.values().any(|p| p.request_id == request_id) {
                    return;
                }
                self.epoch += 1;
                let preview = serde_json::to_string(&input).ok().map(|s| truncate(&s, MAX_PREVIEW_CHARS));
                let (kind, summary, questions) = if tool_name == "AskUserQuestion" {
                    let qs = parse_questions(&input);
                    let first = qs.first().map(|q| q.question.clone()).unwrap_or_default();
                    (InboxKind::Question, truncate(&first, MAX_SUMMARY_CHARS), Some(qs))
                } else {
                    (InboxKind::Permission, truncate(&format!("Permission to use {tool_name}"), MAX_SUMMARY_CHARS), None)
                };
                let id = self.push_item(kind, now, summary, Some(tool_name), preview, questions, None);
                self.pending.insert(id.clone(), Pending { request_id, input });
                self.pending_order.push(id);
                self.recompute();
            }
            Message::Result { is_error, text } => {
                self.epoch += 1;
                if !text.trim().is_empty() {
                    self.last_message = Some(truncate(&text, MAX_MESSAGE_CHARS));
                }
                let summary = truncate(&text, MAX_SUMMARY_CHARS);
                let full = (!text.trim().is_empty()).then(|| truncate_bytes(&text, MAX_RESULT_BYTES));
                if is_error {
                    let s = if summary.is_empty() { "The session reported an error".to_string() } else { summary };
                    self.push_item(InboxKind::Error, now, s, None, None, None, full);
                } else {
                    let s = if summary.is_empty() { "Finished".to_string() } else { summary };
                    self.push_item(InboxKind::Finished, now, s, None, None, None, full);
                }
                self.status = SessionStatus::Idle;
            }
            Message::UnsupportedControlRequest { subtype, .. } => {
                // Nothing is written back: the error shape is not in any fixture. The item
                // tells the user the session may be stuck, and the status says so too.
                let summary = format!(
                    "Claude sent a request Orchestra cannot answer ({}). The session may be waiting; stop it if it does not continue.",
                    truncate(&subtype, MAX_SUBTYPE_CHARS)
                );
                self.push_item(InboxKind::Error, now, summary, None, None, None, None);
                self.status = SessionStatus::NeedsAttention;
            }
            Message::Ignored => {}
            Message::Unknown => self.unknown_count += 1,
        }
    }

    fn take_pending(&self, item_id: &str, kind: InboxKind) -> Result<&Pending, String> {
        let p = self.pending.get(item_id).ok_or("unknown or resolved item")?;
        let item_kind = self.inbox.iter().find(|i| i.id == item_id).map(|i| i.kind);
        if item_kind != Some(kind) {
            return Err("item does not take that kind of answer".into());
        }
        Ok(p)
    }

    /// The stdin line for a permission answer. Read-only: apply the `Commit` with
    /// `commit` once the line has been written.
    pub fn permission_line(
        &self,
        item_id: &str,
        decision: Decision,
        message: Option<&str>,
    ) -> Result<(String, Commit), String> {
        let p = self.take_pending(item_id, InboxKind::Permission)?;
        let line = match decision {
            Decision::Allow => protocol::allow_response(&p.request_id, &p.input),
            Decision::Deny => protocol::deny_response(&p.request_id, message.filter(|m| !m.trim().is_empty()).unwrap_or(DENY_DEFAULT)),
        };
        Ok((line, Commit::Answer { item_id: item_id.to_string() }))
    }

    pub fn question_line(
        &self,
        item_id: &str,
        answers: &BTreeMap<String, String>,
    ) -> Result<(String, Commit), String> {
        let p = self.take_pending(item_id, InboxKind::Question)?;
        let line = protocol::question_response(&p.request_id, &p.input, answers);
        Ok((line, Commit::Answer { item_id: item_id.to_string() }))
    }

    /// A free-text reply is the next user turn; only valid once a turn has ended.
    pub fn reply_line(&self, text: &str) -> Result<(String, Commit), String> {
        if self.status != SessionStatus::Idle {
            return Err("session is not waiting for a reply".into());
        }
        let resolve = self
            .inbox
            .iter()
            .filter(|i| matches!(i.kind, InboxKind::Finished | InboxKind::Error))
            .map(|i| i.id.clone())
            .collect();
        Ok((protocol::user_message(text), Commit::Reply { resolve, epoch: self.epoch }))
    }

    /// Apply a built answer after its line was written. It only touches what is still
    /// true: an item the reader already closed stays closed, and a reply that raced a
    /// reader update resolves the items it captured but leaves the status to the reader.
    pub fn commit(&mut self, commit: Commit, now: u64) {
        match commit {
            Commit::Answer { item_id } => {
                if self.pending.contains_key(&item_id) {
                    self.close_pending(&item_id);
                    if matches!(self.status, SessionStatus::WaitingPermission | SessionStatus::WaitingQuestion) {
                        self.recompute();
                    }
                }
            }
            Commit::Reply { resolve, epoch } => {
                for id in &resolve {
                    self.resolve(id);
                }
                if epoch == self.epoch && self.status == SessionStatus::Idle {
                    self.status = SessionStatus::Working;
                }
            }
        }
        if !self.is_over() {
            self.last_activity_at = now;
        }
    }

    fn finish(&mut self, status: SessionStatus, now: u64) {
        self.epoch += 1;
        let open: Vec<String> = self.pending_order.drain(..).collect();
        for id in open {
            self.pending.remove(&id);
            self.resolve(&id);
        }
        self.status = status;
        self.last_activity_at = now;
    }

    /// Orchestra is closing (or this session was loaded from disk): the process is gone but
    /// the conversation may be resumable. Open requests can no longer be answered, so they
    /// are resolved and marked expired. No error item: this is not a failure.
    pub fn suspend(&mut self, now: u64) {
        if self.is_over() {
            return;
        }
        self.epoch += 1;
        self.pending.clear();
        self.pending_order.clear();
        for i in &mut self.inbox {
            if !i.resolved && matches!(i.kind, InboxKind::Permission | InboxKind::Question) {
                i.resolved = true;
                i.expired = true;
            }
        }
        self.status = if self.claude_session_id.is_some() { SessionStatus::Suspended } else { SessionStatus::Ended };
        self.last_activity_at = now;
    }

    /// The user stopped the session.
    pub fn mark_stopped(&mut self, now: u64) {
        if !self.is_over() {
            self.finish(SessionStatus::Ended, now);
        }
    }

    /// The child exited. Zero is a clean end; anything else is a failure with an error item.
    pub fn mark_exit(&mut self, code: Option<i32>, now: u64) {
        if self.is_over() {
            return;
        }
        if code == Some(0) {
            self.finish(SessionStatus::Ended, now);
        } else {
            self.finish(SessionStatus::Failed, now);
            self.push_item(InboxKind::Error, now, "The session process exited unexpectedly".into(), None, None, None, None);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::protocol::{parse_line, tests::FIXTURES};
    use super::*;

    /// Replays a fixture. Inbound lines go through the parser and state machine; whenever
    /// the recorded host reply (`out`) follows, the matching answer is produced from the
    /// inbox and must equal the recorded wire message exactly.
    /// Returns the de-duplicated status sequence and the final state.
    fn replay(name: &str) -> (Vec<SessionStatus>, SessionState) {
        let src = FIXTURES.iter().find(|(n, _)| *n == name).unwrap().1;
        let mut st = SessionState::new("s1", 0);
        let mut seq = vec![st.status];
        let mut seen_first_out = false;
        for (n, l) in src.lines().enumerate() {
            let d: Value = serde_json::from_str(l).unwrap();
            let now = n as u64 + 1;
            match d["_dir"].as_str().unwrap() {
                "in" => st.apply(parse_line(&d["msg"].to_string()), now),
                "out" => {
                    let msg = &d["msg"];
                    if !seen_first_out {
                        seen_first_out = true; // the initial prompt: sent by start(), not by state
                    } else if msg["type"] == "control_response" {
                        let item = st.inbox.iter().rev().find(|i| !i.resolved).unwrap().id.clone();
                        let r = &msg["response"]["response"];
                        let line = if st.inbox.iter().any(|i| i.id == item && i.kind == InboxKind::Question) {
                            let answers: BTreeMap<String, String> = serde_json::from_value(r["updatedInput"]["answers"].clone()).unwrap();
                            st.answer_question(&item, &answers, now).unwrap()
                        } else if r["behavior"] == "allow" {
                            st.answer_permission(&item, Decision::Allow, None, now).unwrap()
                        } else {
                            st.answer_permission(&item, Decision::Deny, None, now).unwrap()
                        };
                        assert_eq!(serde_json::from_str::<Value>(&line).unwrap(), *msg, "{name}: answer wire shape");
                    } else {
                        let text = msg["message"]["content"].as_str().unwrap();
                        let line = st.reply(text, now).unwrap();
                        assert_eq!(serde_json::from_str::<Value>(&line).unwrap(), *msg, "{name}: reply wire shape");
                    }
                }
                _ => {}
            }
            if *seq.last().unwrap() != st.status {
                seq.push(st.status);
            }
        }
        (seq, st)
    }

    use SessionStatus::*;

    /// Build and commit at once: the old single-step answer.
    impl SessionState {
        fn answer_permission(&mut self, id: &str, d: Decision, m: Option<&str>, now: u64) -> Result<String, String> {
            let (line, c) = self.permission_line(id, d, m)?;
            self.commit(c, now);
            Ok(line)
        }
        fn answer_question(&mut self, id: &str, a: &BTreeMap<String, String>, now: u64) -> Result<String, String> {
            let (line, c) = self.question_line(id, a)?;
            self.commit(c, now);
            Ok(line)
        }
        fn reply(&mut self, text: &str, now: u64) -> Result<String, String> {
            let (line, c) = self.reply_line(text)?;
            self.commit(c, now);
            Ok(line)
        }
    }

    #[test]
    fn replay_permission_allow() {
        let (seq, st) = replay("permission_allow");
        assert_eq!(seq, [Starting, Working, WaitingPermission, Working, Idle]);
        assert_eq!(st.unknown_count, 0);
        assert!(st.claude_session_id.is_some());
        let p = &st.inbox[0];
        assert_eq!((p.kind, p.tool_name.as_deref(), p.resolved), (InboxKind::Permission, Some("Bash"), true));
        assert!(p.tool_input_preview.as_deref().unwrap().contains("touch probe.txt"));
        assert!(p.questions.is_none());
        let f = st.inbox.last().unwrap();
        assert_eq!((f.kind, f.resolved), (InboxKind::Finished, false));
        assert!(st.last_message.is_some());
    }

    #[test]
    fn replay_permission_deny() {
        let (seq, st) = replay("permission_deny");
        assert_eq!(seq, [Starting, Working, WaitingPermission, Working, Idle]);
        assert_eq!(st.inbox[0].kind, InboxKind::Permission);
        assert_eq!(st.inbox.last().unwrap().kind, InboxKind::Finished);
    }

    #[test]
    fn replay_ask_user_question() {
        let (seq, st) = replay("ask_user_question");
        assert_eq!(seq, [Starting, Working, WaitingQuestion, Working, Idle]);
        let q = &st.inbox[0];
        assert_eq!((q.kind, q.tool_name.as_deref()), (InboxKind::Question, Some("AskUserQuestion")));
        let qs = q.questions.as_ref().unwrap();
        assert_eq!(qs.len(), 1);
        assert_eq!((qs[0].question.as_str(), qs[0].header.as_deref(), qs[0].multi_select), ("Which color?", Some("Color"), false));
        assert_eq!(qs[0].options.iter().map(|o| o.label.as_str()).collect::<Vec<_>>(), ["Red", "Blue"]);
        assert_eq!(qs[0].options[0].description.as_deref(), Some("Choose red"));
    }

    #[test]
    fn replay_multi_turn() {
        let (seq, st) = replay("multi_turn");
        assert_eq!(seq, [Starting, Working, Idle, Working, Idle]);
        assert_eq!(st.inbox.iter().filter(|i| i.kind == InboxKind::Finished).count(), 2);
        assert!(st.inbox[0].resolved && !st.inbox[1].resolved, "reply resolves the earlier finished item");
        assert_eq!(st.last_message.as_deref(), Some("two"));
    }

    fn waiting_permission() -> (SessionState, String) {
        let mut st = SessionState::new("s1", 0);
        st.apply(Message::Init { session_id: Some("c".into()) }, 1);
        st.apply(
            Message::CanUseTool { request_id: "r1".into(), tool_name: "Bash".into(), input: serde_json::json!({"command": "x"}) },
            2,
        );
        let id = st.inbox[0].id.clone();
        (st, id)
    }

    #[test]
    fn deny_message_defaults_and_custom() {
        let (mut st, id) = waiting_permission();
        let line = st.answer_permission(&id, Decision::Deny, Some("no thanks"), 3).unwrap();
        let v: Value = serde_json::from_str(&line).unwrap();
        assert_eq!(v["response"]["response"], serde_json::json!({"behavior": "deny", "message": "no thanks"}));
        let (mut st, id) = waiting_permission();
        let line = st.answer_permission(&id, Decision::Deny, None, 3).unwrap();
        assert!(line.contains("User denied"));
    }

    #[test]
    fn answer_wrong_kind_unknown_and_twice_are_rejected() {
        let (mut st, id) = waiting_permission();
        assert!(st.answer_question(&id, &BTreeMap::new(), 3).is_err());
        assert!(st.answer_permission("nope", Decision::Allow, None, 3).is_err());
        assert_eq!(st.status, WaitingPermission, "failed answers change nothing");
        st.answer_permission(&id, Decision::Allow, None, 3).unwrap();
        assert!(st.answer_permission(&id, Decision::Allow, None, 4).is_err());
    }

    #[test]
    fn unknown_messages_are_counted_not_fatal() {
        let mut st = SessionState::new("s1", 0);
        st.apply(parse_line("garbage"), 1);
        st.apply(parse_line(r#"{"type":"future_thing"}"#), 2);
        assert_eq!(st.unknown_count, 2);
        assert_eq!(st.status, Starting);
    }

    #[test]
    fn exit_and_stop_resolve_pending_and_set_terminal_status() {
        let (mut st, _) = waiting_permission();
        st.mark_exit(Some(1), 5);
        assert_eq!(st.status, Failed);
        assert!(st.inbox.iter().all(|i| i.resolved || i.kind == InboxKind::Error));
        assert_eq!(st.inbox.last().unwrap().kind, InboxKind::Error);
        let (mut st, _) = waiting_permission();
        st.mark_stopped(5);
        assert_eq!(st.status, Ended);
        st.mark_exit(Some(1), 6); // a kill-induced exit after stop stays Ended
        assert_eq!(st.status, Ended);
        let mut st = SessionState::new("s1", 0);
        st.mark_exit(Some(0), 1);
        assert_eq!(st.status, Ended);
    }

    #[test]
    fn unsupported_control_request_surfaces_error_item() {
        let (mut st, _) = waiting_permission();
        let long = "x".repeat(200);
        st.apply(Message::UnsupportedControlRequest { request_id: Some("r2".into()), subtype: long }, 5);
        assert_eq!(st.status, NeedsAttention);
        assert_eq!(st.unknown_count, 0);
        let e = st.inbox.last().unwrap();
        assert_eq!((e.kind, e.resolved), (InboxKind::Error, false));
        assert!(e.summary.contains(&"x".repeat(64)) && !e.summary.contains(&"x".repeat(65)));
        assert_eq!(st.pending.len(), 1, "nothing new to answer");
    }

    fn unsupported(st: &mut SessionState, now: u64) {
        st.apply(Message::UnsupportedControlRequest { request_id: None, subtype: "hook_callback".into() }, now);
    }

    fn can_use_bash() -> Message {
        Message::CanUseTool { request_id: "r1".into(), tool_name: "Bash".into(), input: serde_json::json!({"command": "x"}) }
    }

    #[test]
    fn needs_attention_survives_init() {
        let mut st = SessionState::new("s1", 0);
        unsupported(&mut st, 1);
        assert_eq!(st.status, NeedsAttention);
        st.apply(Message::Init { session_id: Some("c".into()) }, 2);
        assert_eq!(st.status, NeedsAttention);
    }

    #[test]
    fn needs_attention_survives_a_later_permission_request() {
        let mut st = SessionState::new("s1", 0);
        st.apply(Message::Init { session_id: None }, 1);
        unsupported(&mut st, 2);
        st.apply(can_use_bash(), 3);
        assert_eq!(st.status, NeedsAttention);
        assert_eq!(st.pending.len(), 1, "the request is still answerable");
    }

    #[test]
    fn needs_attention_survives_answering_a_permission() {
        let mut st = SessionState::new("s1", 0);
        unsupported(&mut st, 1);
        st.apply(can_use_bash(), 2);
        let id = st.inbox.last().unwrap().id.clone();
        let (_, c) = st.permission_line(&id, Decision::Allow, None).unwrap();
        st.commit(c, 3);
        assert_eq!(st.status, NeedsAttention);
        assert!(st.pending.is_empty());
    }

    #[test]
    fn needs_attention_clears_on_result() {
        let mut st = SessionState::new("s1", 0);
        unsupported(&mut st, 1);
        st.apply(Message::Result { is_error: false, text: "done".into() }, 2);
        assert_eq!(st.status, Idle);
    }

    #[test]
    fn needs_attention_clears_on_stop() {
        let mut st = SessionState::new("s1", 0);
        unsupported(&mut st, 1);
        st.mark_stopped(2);
        assert_eq!(st.status, Ended);
    }

    #[test]
    fn needs_attention_serializes_snake_case() {
        assert_eq!(serde_json::to_string(&NeedsAttention).unwrap(), "\"needs_attention\"");
    }

    #[test]
    fn reply_commit_yields_to_a_reader_update() {
        let mut st = SessionState::new("s1", 0);
        st.apply(Message::Result { is_error: false, text: "a".into() }, 1);
        let (_, c) = st.reply_line("more").unwrap();
        st.apply(Message::Result { is_error: false, text: "b".into() }, 2);
        st.commit(c, 3);
        assert_eq!(st.status, Idle);
        assert_eq!((st.inbox[0].resolved, st.inbox[1].resolved), (true, false));
    }

    const UUID: &str = "0b9f6a2c-3d4e-4f50-8a1b-2c3d4e5f6a7b";

    fn init(st: &mut SessionState, id: &str, now: u64) {
        st.apply(Message::Init { session_id: Some(id.into()) }, now);
    }

    #[test]
    fn dedupes_repeated_request_id() {
        let mut st = SessionState::new("s1", 0);
        st.apply(can_use_bash(), 1);
        st.apply(can_use_bash(), 2);
        assert_eq!(st.inbox.len(), 1);
        assert_eq!(st.pending.len(), 1);
    }

    #[test]
    fn suspend_expires_open_requests() {
        let mut st = SessionState::new("s1", 0);
        init(&mut st, UUID, 1);
        st.apply(can_use_bash(), 2);
        st.apply(Message::Result { is_error: false, text: "done".into() }, 3);
        st.suspend(4);
        assert_eq!(st.status, Suspended);
        assert!(st.pending.is_empty() && st.pending_order.is_empty());
        let p = &st.inbox[0];
        assert_eq!((p.resolved, p.expired), (true, true));
        let f = &st.inbox[1];
        assert_eq!((f.kind, f.resolved, f.expired), (InboxKind::Finished, false, false));
        assert_eq!(st.inbox.len(), 2, "no error item");
    }

    #[test]
    fn suspend_without_claude_id_ends() {
        let (mut st, _) = waiting_permission();
        st.claude_session_id = None;
        st.suspend(5);
        assert_eq!(st.status, Ended);
        assert!(st.inbox.iter().all(|i| i.kind != InboxKind::Error));
        assert!(st.inbox[0].expired);
    }

    #[test]
    fn finished_item_keeps_full_result_text() {
        let mut st = SessionState::new("s1", 0);
        let text = "\u{e9}".repeat(1000);
        st.apply(Message::Result { is_error: false, text: text.clone() }, 1);
        let f = &st.inbox[0];
        assert_eq!(f.result_text.as_deref(), Some(text.as_str()));
        assert_eq!(f.summary.chars().count(), 300);
        let big = "\u{e9}".repeat(MAX_RESULT_BYTES);
        st.apply(Message::Result { is_error: true, text: big }, 2);
        let e = st.inbox[1].result_text.as_ref().unwrap();
        assert_eq!(e.len(), MAX_RESULT_BYTES);
        assert!(e.chars().all(|c| c == '\u{e9}'));
        assert_eq!(truncate_bytes("a\u{e9}", 2), "a", "cut lands on a char boundary");
    }

    #[test]
    fn rejects_non_uuid_claude_session_id() {
        let mut st = SessionState::new("s1", 0);
        init(&mut st, "--evil", 1);
        init(&mut st, "0b9f6a2c-3d4e-4f50-8a1b-2c3d4e5f6a7", 2);
        init(&mut st, "0b9f6a2c-3d4e-4f50-8a1b-2c3d4e5f6a7g", 3);
        assert_eq!(st.claude_session_id, None);
        init(&mut st, UUID, 4);
        assert_eq!(st.claude_session_id.as_deref(), Some(UUID));
        init(&mut st, "x", 5);
        assert_eq!(st.claude_session_id.as_deref(), Some(UUID), "a bad id never replaces a good one");
    }

    #[test]
    fn inbox_item_serde_roundtrip() {
        let mut st = SessionState::new("s1", 0);
        init(&mut st, UUID, 1);
        st.apply(can_use_bash(), 2);
        st.apply(Message::Result { is_error: false, text: "ok".into() }, 3);
        st.suspend(4);
        let p = st.to_persisted();
        let json = serde_json::to_string(&p).unwrap();
        let back: PersistedSession = serde_json::from_str(&json).unwrap();
        assert_eq!(back, p);
        assert!(json.contains("\"suspended\"") && json.contains("\"expired\":true"));
        let st2 = SessionState::from_persisted("s1", back);
        assert_eq!(st2.status, Suspended);
        assert!(st2.pending.is_empty());
        assert_eq!(st2.inbox, st.inbox);
    }

    #[test]
    fn long_text_is_truncated() {
        let mut st = SessionState::new("s1", 0);
        st.apply(Message::Assistant { text: "é".repeat(900), tool_name: None }, 1);
        assert_eq!(st.last_message.as_ref().unwrap().chars().count(), 500);
    }
}
