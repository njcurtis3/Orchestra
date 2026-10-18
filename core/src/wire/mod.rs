//! Protocol v1, the Rust copy. The TypeScript definitions in `protocol/` are the source of
//! truth; `tests/wire_fixtures.rs` runs the shared `protocol/fixtures/*.json` through this
//! module so the two cannot drift. Nothing here opens a socket or enforces a scope: `link`
//! does both, and calls `dispatch` once a call has passed its scope check.
//!
//! Named `wire` because `session::protocol` is the claude stream-json parser.

pub mod args;
pub mod codec;
pub mod dispatch;
pub mod message;

use std::fmt;

pub use codec::{
    decode_client, decode_handshake_reply, decode_handshake_request, decode_host, encode_client,
    encode_handshake_reply, encode_handshake_request, encode_host,
};
pub use dispatch::{dispatch, dispatch_json};
pub use message::{
    Call, ClientMessage, ErrorBody, ErrorMessage, HandshakeReply, HostEvent, HostMessage,
    Snapshot, Welcome, WireStorageStatus,
};

pub const PROTOCOL_NAME: &str = "orchestra";
pub const PROTOCOL_VERSION: u64 = 1;
pub const SUPPORTED_VERSIONS: [u64; 1] = [1];
/// One text field a client sends, in UTF-8 bytes. Also caps every client-sent string.
pub const MAX_TEXT_BYTES: usize = crate::session::MAX_TEXT_BYTES;
/// One decoded JSON message, in UTF-8 bytes.
pub const MAX_MESSAGE_BYTES: usize = 4_194_304;
/// `call.id`: client-generated, non-empty.
pub const MAX_ID_BYTES: usize = 128;

/// Tauri event names verbatim, plus the reserved `notice` (payload typed by H2).
pub const EVENT_NAMES: [&str; 5] =
    ["session-updated", "inbox-updated", "transcript-appended", "task-updated", "notice"];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scope {
    Read,
    Answer,
    Control,
    Land,
}

impl Scope {
    pub const ALL: [Scope; 4] = [Scope::Read, Scope::Answer, Scope::Control, Scope::Land];

    pub fn as_str(self) -> &'static str {
        match self {
            Scope::Read => "read",
            Scope::Answer => "answer",
            Scope::Control => "control",
            Scope::Land => "land",
        }
    }
}

/// The Tauri command names verbatim. `pick_project` is host-local and is not a wire method.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Method {
    ListProjects,
    ListSessions,
    ListInbox,
    GetTranscript,
    ListTasks,
    GetTaskDiff,
    StorageStatus,
    StartSession,
    StopSession,
    Answer,
    CreateTask,
    CancelTask,
    LandTask,
    DiscardTask,
    RetryCleanup,
}

impl Method {
    pub const ALL: [Method; 15] = [
        Method::ListProjects,
        Method::ListSessions,
        Method::ListInbox,
        Method::GetTranscript,
        Method::ListTasks,
        Method::GetTaskDiff,
        Method::StorageStatus,
        Method::StartSession,
        Method::StopSession,
        Method::Answer,
        Method::CreateTask,
        Method::CancelTask,
        Method::LandTask,
        Method::DiscardTask,
        Method::RetryCleanup,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Method::ListProjects => "list_projects",
            Method::ListSessions => "list_sessions",
            Method::ListInbox => "list_inbox",
            Method::GetTranscript => "get_transcript",
            Method::ListTasks => "list_tasks",
            Method::GetTaskDiff => "get_task_diff",
            Method::StorageStatus => "storage_status",
            Method::StartSession => "start_session",
            Method::StopSession => "stop_session",
            Method::Answer => "answer",
            Method::CreateTask => "create_task",
            Method::CancelTask => "cancel_task",
            Method::LandTask => "land_task",
            Method::DiscardTask => "discard_task",
            Method::RetryCleanup => "retry_cleanup",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|m| m.as_str() == s)
    }

    /// The scope table of the README as data. Not enforced here.
    pub fn scope(self) -> Scope {
        match self {
            Method::ListProjects
            | Method::ListSessions
            | Method::ListInbox
            | Method::GetTranscript
            | Method::ListTasks
            | Method::GetTaskDiff
            | Method::StorageStatus => Scope::Read,
            Method::Answer => Scope::Answer,
            Method::StartSession
            | Method::StopSession
            | Method::CreateTask
            | Method::CancelTask
            | Method::RetryCleanup => Scope::Control,
            Method::LandTask | Method::DiscardTask => Scope::Land,
        }
    }
}

/// The closed set of error codes a host sends. A client keeps a code it does not know as a
/// plain string (`ErrorBody::code`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrorCode {
    UnsupportedVersion,
    UnknownType,
    BadRequest,
    Forbidden,
    Revoked,
    AlreadyAnswered,
    Failed,
}

impl ErrorCode {
    pub const ALL: [ErrorCode; 7] = [
        ErrorCode::UnsupportedVersion,
        ErrorCode::UnknownType,
        ErrorCode::BadRequest,
        ErrorCode::Forbidden,
        ErrorCode::Revoked,
        ErrorCode::AlreadyAnswered,
        ErrorCode::Failed,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            ErrorCode::UnsupportedVersion => "unsupported_version",
            ErrorCode::UnknownType => "unknown_type",
            ErrorCode::BadRequest => "bad_request",
            ErrorCode::Forbidden => "forbidden",
            ErrorCode::Revoked => "revoked",
            ErrorCode::AlreadyAnswered => "already_answered",
            ErrorCode::Failed => "failed",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|c| c.as_str() == s)
    }
}

/// A message that could not be decoded or encoded. The message is short and carries no
/// payload, so it is safe to log.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WireError {
    pub code: ErrorCode,
    pub message: String,
}

impl WireError {
    pub fn new(code: ErrorCode, message: impl Into<String>) -> Self {
        WireError { code, message: message.into() }
    }

    pub fn bad(message: impl Into<String>) -> Self {
        Self::new(ErrorCode::BadRequest, message)
    }
}

impl fmt::Display for WireError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.code.as_str(), self.message)
    }
}

impl std::error::Error for WireError {}
