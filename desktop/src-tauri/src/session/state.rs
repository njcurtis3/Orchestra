//! Pure state machine for one session: protocol messages in, `SessionStatus` and inbox
//! items out. No I/O, no clock (callers pass `now`), so it replays fixtures directly.

use std::collections::{BTreeMap, HashMap};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::protocol::{self, Message};

const MAX_MESSAGE_CHARS: usize = 500;
const MAX_SUMMARY_CHARS: usize = 300;
const MAX_PREVIEW_CHARS: usize = 2000;
const DENY_DEFAULT: &str = "User denied";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionStatus {
    Starting,
    Working,
    WaitingPermission,
    WaitingQuestion,
    Idle,
    Ended,
    Failed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
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

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct QuestionOption {
    pub label: String,
    pub description: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Question {
    pub question: String,
    pub header: Option<String>,
    pub multi_select: bool,
    pub options: Vec<QuestionOption>,
}

#[derive(Debug, Clone, Serialize)]
pub struct InboxItem {
    pub id: String,
    pub session_id: String,
    pub kind: InboxKind,
    pub created_at: u64,
    pub resolved: bool,
    pub summary: String,
    pub tool_name: Option<String>,
    pub tool_input_preview: Option<String>,
    pub questions: Option<Vec<Question>>,
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
}

pub fn truncate(s: &str, max_chars: usize) -> String {
    if s.chars().count() <= max_chars {
        return s.to_string();
    }
    s.chars().take(max_chars).collect()
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
    ) -> String {
        self.next_item += 1;
        let id = format!("{}-i{}", self.session_id, self.next_item);
        self.inbox.push(InboxItem {
            id: id.clone(),
            session_id: self.session_id.clone(),
            kind,
            created_at: now,
            resolved: false,
            summary,
            tool_name,
            tool_input_preview,
            questions,
        });
        id
    }

    fn resolve(&mut self, item_id: &str) {
        if let Some(i) = self.inbox.iter_mut().find(|i| i.id == item_id) {
            i.resolved = true;
        }
    }

    fn resolve_kind(&mut self, kind: InboxKind) {
        for i in self.inbox.iter_mut().filter(|i| i.kind == kind) {
            i.resolved = true;
        }
    }

    /// Waiting while any request is open, otherwise working.
    fn recompute(&mut self) {
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
                if session_id.is_some() {
                    self.claude_session_id = session_id;
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
                let preview = serde_json::to_string(&input).ok().map(|s| truncate(&s, MAX_PREVIEW_CHARS));
                let (kind, summary, questions) = if tool_name == "AskUserQuestion" {
                    let qs = parse_questions(&input);
                    let first = qs.first().map(|q| q.question.clone()).unwrap_or_default();
                    (InboxKind::Question, truncate(&first, MAX_SUMMARY_CHARS), Some(qs))
                } else {
                    (InboxKind::Permission, truncate(&format!("Permission to use {tool_name}"), MAX_SUMMARY_CHARS), None)
                };
                let id = self.push_item(kind, now, summary, Some(tool_name), preview, questions);
                self.pending.insert(id.clone(), Pending { request_id, input });
                self.pending_order.push(id);
                self.recompute();
            }
            Message::Result { is_error, text } => {
                if !text.trim().is_empty() {
                    self.last_message = Some(truncate(&text, MAX_MESSAGE_CHARS));
                }
                let summary = truncate(&text, MAX_SUMMARY_CHARS);
                if is_error {
                    let s = if summary.is_empty() { "The session reported an error".to_string() } else { summary };
                    self.push_item(InboxKind::Error, now, s, None, None, None);
                } else {
                    let s = if summary.is_empty() { "Finished".to_string() } else { summary };
                    self.push_item(InboxKind::Finished, now, s, None, None, None);
                }
                self.status = SessionStatus::Idle;
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

    /// Returns the stdin line to send. State changes only on success.
    pub fn answer_permission(
        &mut self,
        item_id: &str,
        decision: Decision,
        message: Option<&str>,
        now: u64,
    ) -> Result<String, String> {
        let p = self.take_pending(item_id, InboxKind::Permission)?;
        let line = match decision {
            Decision::Allow => protocol::allow_response(&p.request_id, &p.input),
            Decision::Deny => protocol::deny_response(&p.request_id, message.filter(|m| !m.trim().is_empty()).unwrap_or(DENY_DEFAULT)),
        };
        self.close_pending(item_id);
        self.recompute();
        self.last_activity_at = now;
        Ok(line)
    }

    pub fn answer_question(
        &mut self,
        item_id: &str,
        answers: &BTreeMap<String, String>,
        now: u64,
    ) -> Result<String, String> {
        let p = self.take_pending(item_id, InboxKind::Question)?;
        let line = protocol::question_response(&p.request_id, &p.input, answers);
        self.close_pending(item_id);
        self.recompute();
        self.last_activity_at = now;
        Ok(line)
    }

    /// A free-text reply is the next user turn; only valid once a turn has ended.
    pub fn reply(&mut self, text: &str, now: u64) -> Result<String, String> {
        if self.status != SessionStatus::Idle {
            return Err("session is not waiting for a reply".into());
        }
        self.resolve_kind(InboxKind::Finished);
        self.resolve_kind(InboxKind::Error);
        self.status = SessionStatus::Working;
        self.last_activity_at = now;
        Ok(protocol::user_message(text))
    }

    fn finish(&mut self, status: SessionStatus, now: u64) {
        let open: Vec<String> = self.pending_order.drain(..).collect();
        for id in open {
            self.pending.remove(&id);
            self.resolve(&id);
        }
        self.status = status;
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
            self.push_item(InboxKind::Error, now, "The session process exited unexpectedly".into(), None, None, None);
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
    fn long_text_is_truncated() {
        let mut st = SessionState::new("s1", 0);
        st.apply(Message::Assistant { text: "é".repeat(900), tool_name: None }, 1);
        assert_eq!(st.last_message.as_ref().unwrap().chars().count(), 500);
    }
}
