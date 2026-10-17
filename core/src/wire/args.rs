//! Call arguments: the strict serde structs of the host. Every one denies unknown fields, so
//! a new argument is gated on a `welcome.features` flag (README "Versioning"). These moved
//! here from the desktop shell so the IPC and the wire share one definition.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::session::settings::{Model, PermissionMode};
use crate::session::state::Decision;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StartArgs {
    pub project_id: String,
    pub prompt: String,
    /// Absent means no flag. Anything off the allowlists fails to deserialize.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<Model>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub permission_mode: Option<PermissionMode>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CreateTaskArgs {
    pub project_id: String,
    pub prompt: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<Model>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub permission_mode: Option<PermissionMode>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CancelTaskArgs {
    pub task_id: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskIdArgs {
    pub task_id: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StopArgs {
    pub session_id: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TranscriptArgs {
    pub session_id: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Answer {
    Permission {
        item_id: String,
        decision: Decision,
        #[serde(skip_serializing_if = "Option::is_none")]
        message: Option<String>,
    },
    Question { item_id: String, answers: BTreeMap<String, String> },
    Reply { session_id: String, text: String },
}
