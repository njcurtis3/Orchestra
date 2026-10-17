//! Protocol v1 messages. Host input (`ClientMessage`) is built by a strict decoder; host
//! output (`HostMessage`) is read by a tolerant one. See `codec.rs` for the rules.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::Method;
use super::args::{
    Answer, CancelTaskArgs, CreateTaskArgs, StartArgs, StopArgs, TaskIdArgs, TranscriptArgs,
};
use crate::projects::Project;
use crate::session::state::InboxItem;
use crate::session::task::TaskSummary;
use crate::session::transcript::TranscriptBatch;
use crate::session::SessionSummary;

// ---- Client to host ----

#[derive(Debug, Clone, PartialEq)]
pub enum ClientMessage {
    Hello { client_name: String, app_version: String },
    Call { id: String, call: Call },
    Ping,
}

/// A call with its arguments parsed. The no-argument methods carry `{}` on the wire.
#[derive(Debug, Clone, PartialEq)]
pub enum Call {
    ListProjects,
    ListSessions,
    ListInbox,
    GetTranscript(TranscriptArgs),
    ListTasks,
    GetTaskDiff(TaskIdArgs),
    StorageStatus,
    StartSession(StartArgs),
    StopSession(StopArgs),
    Answer(Answer),
    CreateTask(CreateTaskArgs),
    CancelTask(CancelTaskArgs),
    LandTask(TaskIdArgs),
    DiscardTask(TaskIdArgs),
    RetryCleanup(TaskIdArgs),
}

impl Call {
    pub fn method(&self) -> Method {
        match self {
            Call::ListProjects => Method::ListProjects,
            Call::ListSessions => Method::ListSessions,
            Call::ListInbox => Method::ListInbox,
            Call::GetTranscript(_) => Method::GetTranscript,
            Call::ListTasks => Method::ListTasks,
            Call::GetTaskDiff(_) => Method::GetTaskDiff,
            Call::StorageStatus => Method::StorageStatus,
            Call::StartSession(_) => Method::StartSession,
            Call::StopSession(_) => Method::StopSession,
            Call::Answer(_) => Method::Answer,
            Call::CreateTask(_) => Method::CreateTask,
            Call::CancelTask(_) => Method::CancelTask,
            Call::LandTask(_) => Method::LandTask,
            Call::DiscardTask(_) => Method::DiscardTask,
            Call::RetryCleanup(_) => Method::RetryCleanup,
        }
    }

    /// The `args` object as it goes on the wire.
    pub fn args_value(&self) -> Value {
        let v = match self {
            Call::ListProjects
            | Call::ListSessions
            | Call::ListInbox
            | Call::ListTasks
            | Call::StorageStatus => return Value::Object(Default::default()),
            Call::GetTranscript(a) => serde_json::to_value(a),
            Call::StopSession(a) => serde_json::to_value(a),
            Call::GetTaskDiff(a) | Call::LandTask(a) | Call::DiscardTask(a) | Call::RetryCleanup(a) => {
                serde_json::to_value(a)
            }
            Call::StartSession(a) => serde_json::to_value(a),
            Call::CreateTask(a) => serde_json::to_value(a),
            Call::CancelTask(a) => serde_json::to_value(a),
            Call::Answer(a) => serde_json::to_value(a),
        };
        // Plain structs of strings and options cannot fail to serialise.
        v.unwrap_or(Value::Null)
    }
}

// ---- Host to client ----

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Welcome {
    pub host_id: String,
    pub host_name: String,
    /// Kept as strings: a client renders a scope it does not know as plain text.
    pub scopes: Vec<String>,
    pub features: Vec<String>,
}

/// `StorageStatus` in core holds a `&'static str`, which cannot be read back, so the wire
/// has its own owned copy of the same three fields.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WireStorageStatus {
    pub ok: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub found_version: Option<u64>,
}

impl From<&crate::session::store::StorageStatus> for WireStorageStatus {
    fn from(s: &crate::session::store::StorageStatus) -> Self {
        WireStorageStatus { ok: s.ok, reason: s.reason.map(str::to_string), found_version: s.found_version }
    }
}

/// The connect-time state. Not `store::Snapshot`, which is the on-disk file.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Snapshot {
    pub seq: u64,
    pub projects: Vec<Project>,
    pub sessions: Vec<SessionSummary>,
    pub inbox: Vec<InboxItem>,
    pub tasks: Vec<TaskSummary>,
    pub storage: WireStorageStatus,
}

#[derive(Debug, Clone, PartialEq)]
pub enum HostEvent {
    SessionUpdated(SessionSummary),
    InboxUpdated(Vec<InboxItem>),
    TranscriptAppended(TranscriptBatch),
    TaskUpdated(TaskSummary),
    /// A name this build does not know, `notice` included until H2 types it.
    Unknown { name: String, payload: Value },
}

impl HostEvent {
    pub fn name(&self) -> &str {
        match self {
            HostEvent::SessionUpdated(_) => "session-updated",
            HostEvent::InboxUpdated(_) => "inbox-updated",
            HostEvent::TranscriptAppended(_) => "transcript-appended",
            HostEvent::TaskUpdated(_) => "task-updated",
            HostEvent::Unknown { name, .. } => name,
        }
    }
}

/// `result.error`. The code is a string: a client keeps a code it does not know.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ErrorBody {
    pub code: String,
    pub message: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ErrorMessage {
    pub code: String,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub supported: Option<Vec<u64>>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum HostMessage {
    Welcome(Welcome),
    Snapshot(Box<Snapshot>),
    Event { seq: u64, event: HostEvent },
    /// `Ok` holds any JSON, `null` for unit commands.
    Result { id: String, outcome: Result<Value, ErrorBody> },
    Error(ErrorMessage),
    Pong,
}

/// What a client reads back to its plaintext handshake frame.
#[derive(Debug, Clone, PartialEq)]
pub enum HandshakeReply {
    Accept { supported: Vec<u64> },
    Error(ErrorMessage),
}
