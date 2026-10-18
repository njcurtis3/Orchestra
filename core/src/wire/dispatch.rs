//! The one table from a decoded call to the core operation behind it. The link calls this
//! after its scope check; nothing here knows about sockets, keys or scopes. A refusal comes
//! back as a `WireError` whose message is the core's own: short, with no path.

use std::sync::Arc;

use serde::Serialize;
use serde_json::{Value, json};

use super::message::{Call, ClientMessage, HostEvent, WireStorageStatus};
use super::{ErrorCode, Method, PROTOCOL_VERSION, WireError, decode_client};
use crate::session::store::StorageStatus;
use crate::session::{AnswerError, Event, Sessions};

impl From<Event> for HostEvent {
    fn from(e: Event) -> Self {
        match e {
            Event::SessionUpdated(s) => HostEvent::SessionUpdated(s),
            Event::InboxUpdated(items) => HostEvent::InboxUpdated(items),
            Event::TranscriptAppended(batch) => HostEvent::TranscriptAppended(batch),
            Event::TaskUpdated(t) => HostEvent::TaskUpdated(t),
        }
    }
}

impl From<AnswerError> for WireError {
    fn from(e: AnswerError) -> Self {
        let code = match e {
            AnswerError::AlreadyAnswered => ErrorCode::AlreadyAnswered,
            AnswerError::Failed(_) => ErrorCode::Failed,
        };
        WireError::new(code, e.message())
    }
}

fn failed(message: String) -> WireError {
    WireError::new(ErrorCode::Failed, message)
}

fn value<T: Serialize>(v: T) -> Result<Value, WireError> {
    serde_json::to_value(v).map_err(|_| failed("could not encode the result".into()))
}

fn unit(r: Result<(), String>) -> Result<Value, WireError> {
    r.map(|()| Value::Null).map_err(failed)
}

/// Run a call and return the JSON that goes in `result.ok`. Blocking: it may run git or
/// write to a child's stdin, so a caller keeps it off its read loop.
pub fn dispatch(sessions: &Arc<Sessions>, storage: &StorageStatus, call: &Call) -> Result<Value, WireError> {
    match call {
        Call::ListProjects => value(sessions.list_projects()),
        Call::ListSessions => value(sessions.list_sessions()),
        Call::ListInbox => value(sessions.list_inbox()),
        Call::ListTasks => value(sessions.list_tasks()),
        Call::GetTranscript(a) => value(sessions.transcript(&a.session_id).map_err(failed)?),
        Call::GetTaskDiff(a) => value(sessions.get_task_diff(&a.task_id).map_err(failed)?),
        Call::StorageStatus => value(WireStorageStatus::from(storage)),
        Call::StartSession(a) => value(sessions.start(&a.project_id, &a.prompt, a.settings()).map_err(failed)?),
        Call::StopSession(a) => unit(sessions.stop(&a.session_id)),
        Call::Answer(a) => {
            sessions.answer(a.clone())?;
            Ok(Value::Null)
        }
        Call::CreateTask(a) => value(sessions.create_task(&a.project_id, &a.prompt, a.settings()).map_err(failed)?),
        Call::CancelTask(a) => unit(sessions.cancel_task(&a.task_id)),
        Call::LandTask(a) => value(sessions.land_task(&a.task_id).map_err(failed)?),
        Call::DiscardTask(a) => value(sessions.discard_task(&a.task_id).map_err(failed)?),
        Call::RetryCleanup(a) => value(sessions.retry_cleanup(&a.task_id).map_err(failed)?),
    }
}

/// `dispatch` from a method and its raw `args`, validated by the strict decoder first.
pub fn dispatch_json(
    sessions: &Arc<Sessions>,
    storage: &StorageStatus,
    method: Method,
    args: Value,
) -> Result<Value, WireError> {
    let text = json!({"v": PROTOCOL_VERSION, "type": "call", "id": "dispatch", "method": method.as_str(), "args": args})
        .to_string();
    match decode_client(&text)? {
        ClientMessage::Call { call, .. } => dispatch(sessions, storage, &call),
        _ => Err(WireError::bad("not a call")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::process::{Output, Proc};
    use crate::session::transcript::TranscriptStore;
    use crate::session::{Sink, Spawner};
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;

    static N: AtomicUsize = AtomicUsize::new(0);

    struct Fake {
        lines: Mutex<Vec<String>>,
    }

    impl Proc for Fake {
        fn send_line(&self, line: &str) -> Result<(), String> {
            self.lines.lock().unwrap().push(line.to_string());
            Ok(())
        }
        fn kill(&self) {}
        fn wait_exit(&self, _timeout: Duration) -> bool {
            true
        }
    }

    struct Rig {
        sessions: Arc<Sessions>,
        fake: Arc<Fake>,
        sink: Arc<Mutex<Vec<Sink>>>,
        project: String,
    }

    const INIT: &str = r#"{"type":"system","subtype":"init","session_id":"c-1"}"#;
    const PERM: &str = r#"{"type":"control_request","request_id":"r-1","request":{"subtype":"can_use_tool","tool_name":"Bash","input":{"command":"touch x"}}}"#;

    fn rig() -> Rig {
        let fake = Arc::new(Fake { lines: Mutex::new(vec![]) });
        let sink: Arc<Mutex<Vec<Sink>>> = Arc::new(Mutex::new(Vec::new()));
        let (f2, s2) = (fake.clone(), sink.clone());
        let spawner: Spawner = Box::new(move |_, _, sk| {
            s2.lock().unwrap().push(sk);
            Ok(f2.clone() as Arc<dyn Proc>)
        });
        let base = std::env::temp_dir().join(format!(
            "orchestra-dispatch-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::SeqCst)
        ));
        let sessions = Sessions::with_writer(
            Box::new(|_| {}),
            spawner,
            Box::new(|_, _| {}),
            None,
            TranscriptStore::memory(),
            base,
        );
        let project = sessions.add_project(&std::env::temp_dir()).unwrap().id;
        Rig { sessions, fake, sink, project }
    }

    impl Rig {
        fn call(&self, method: Method, args: Value) -> Result<Value, WireError> {
            dispatch_json(&self.sessions, &StorageStatus::ok(), method, args)
        }

        fn feed(&self, line: &str) {
            let guard = self.sink.lock().unwrap();
            (guard.last().unwrap())(Output::Line(line.to_string()));
        }

        fn started(&self) -> String {
            let v = self.call(Method::StartSession, json!({"project_id": self.project, "prompt": "go"})).unwrap();
            v["id"].as_str().unwrap().to_string()
        }
    }

    fn code(r: Result<Value, WireError>) -> ErrorCode {
        r.unwrap_err().code
    }

    #[test]
    fn dispatch_list_projects_sessions_inbox_tasks() {
        let r = rig();
        assert_eq!(r.call(Method::ListProjects, json!({})).unwrap().as_array().unwrap().len(), 1);
        assert_eq!(r.call(Method::ListSessions, json!({})).unwrap(), json!([]));
        assert_eq!(r.call(Method::ListInbox, json!({})).unwrap(), json!([]));
        assert_eq!(r.call(Method::ListTasks, json!({})).unwrap(), json!([]));
    }

    #[test]
    fn dispatch_start_session_then_list_and_transcript() {
        let r = rig();
        let id = r.started();
        assert_eq!(r.call(Method::ListSessions, json!({})).unwrap().as_array().unwrap().len(), 1);
        let page = r.call(Method::GetTranscript, json!({"session_id": id})).unwrap();
        assert!(page["entries"].is_array());
        assert_eq!(code(r.call(Method::GetTranscript, json!({"session_id": "nope"}))), ErrorCode::Failed);
    }

    #[test]
    fn dispatch_start_session_refusals_are_failed() {
        let r = rig();
        assert_eq!(code(r.call(Method::StartSession, json!({"project_id": "nope", "prompt": "go"}))), ErrorCode::Failed);
        assert_eq!(code(r.call(Method::StartSession, json!({"project_id": r.project, "prompt": " "}))), ErrorCode::Failed);
    }

    #[test]
    fn dispatch_stop_session() {
        let r = rig();
        let id = r.started();
        assert_eq!(r.call(Method::StopSession, json!({"session_id": id})).unwrap(), Value::Null);
        assert_eq!(code(r.call(Method::StopSession, json!({"session_id": "nope"}))), ErrorCode::Failed);
    }

    #[test]
    fn dispatch_storage_status_passes_the_given_status_through() {
        let r = rig();
        let ok = r.call(Method::StorageStatus, json!({})).unwrap();
        assert_eq!(ok, json!({"ok": true}));
        let bad = StorageStatus::unreadable();
        let v = dispatch_json(&r.sessions, &bad, Method::StorageStatus, json!({})).unwrap();
        assert_eq!(v, json!({"ok": false, "reason": "unreadable"}));
    }

    #[test]
    fn dispatch_answer_applies_once_then_is_already_answered() {
        let r = rig();
        r.started();
        r.feed(INIT);
        r.feed(PERM);
        let item = r.sessions.list_inbox().into_iter().find(|i| !i.resolved).unwrap();
        let args = json!({"kind": "permission", "item_id": item.id, "decision": "allow"});
        assert_eq!(r.call(Method::Answer, args.clone()).unwrap(), Value::Null);
        assert_eq!(r.fake.lines.lock().unwrap().len(), 2);
        assert_eq!(code(r.call(Method::Answer, args)), ErrorCode::AlreadyAnswered);
        assert_eq!(r.fake.lines.lock().unwrap().len(), 2);
    }

    #[test]
    fn dispatch_answer_unknown_item_is_already_answered_and_other_errors_are_failed() {
        let r = rig();
        let unknown = json!({"kind": "permission", "item_id": "zzz", "decision": "allow"});
        assert_eq!(code(r.call(Method::Answer, unknown)), ErrorCode::AlreadyAnswered);
        let reply = json!({"kind": "reply", "session_id": "nope", "text": "hi"});
        assert_eq!(code(r.call(Method::Answer, reply)), ErrorCode::Failed);
        let long = "x".repeat(crate::session::MAX_TEXT_BYTES + 1);
        let args = json!({"kind": "permission", "item_id": "zzz", "decision": "deny", "message": long});
        assert_eq!(code(dispatch_json(&r.sessions, &StorageStatus::ok(), Method::Answer, args)), ErrorCode::BadRequest);
    }

    #[test]
    fn dispatch_task_methods_refuse_unknown_ids_and_projects() {
        let r = rig();
        assert_eq!(code(r.call(Method::GetTaskDiff, json!({"task_id": "t9"}))), ErrorCode::Failed);
        assert_eq!(code(r.call(Method::CancelTask, json!({"task_id": "t9"}))), ErrorCode::Failed);
        assert_eq!(code(r.call(Method::LandTask, json!({"task_id": "t9"}))), ErrorCode::Failed);
        assert_eq!(code(r.call(Method::DiscardTask, json!({"task_id": "t9"}))), ErrorCode::Failed);
        assert_eq!(code(r.call(Method::RetryCleanup, json!({"task_id": "t9"}))), ErrorCode::Failed);
        assert_eq!(
            code(r.call(Method::CreateTask, json!({"project_id": "nope", "prompt": "go"}))),
            ErrorCode::Failed
        );
    }

    #[test]
    fn dispatch_json_validates_args_strictly() {
        let r = rig();
        assert_eq!(code(r.call(Method::ListSessions, json!({"x": 1}))), ErrorCode::BadRequest);
        assert_eq!(code(r.call(Method::StopSession, json!({"session_id": "s", "force": true}))), ErrorCode::BadRequest);
    }

    #[test]
    fn dispatch_covers_every_method() {
        // A new Method must get an arm in `dispatch` (the match is exhaustive) and a test above.
        assert_eq!(Method::ALL.len(), 15);
    }

    #[test]
    fn host_event_from_core_event_keeps_the_wire_name() {
        let e = Event::InboxUpdated(vec![]);
        assert_eq!(HostEvent::from(e).name(), "inbox-updated");
    }
}
