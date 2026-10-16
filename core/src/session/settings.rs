//! Per-session claude settings: model and permission mode. Each is a closed allowlist that
//! maps to a fixed static string, so no IPC string ever reaches the claude command line.
//! `None` means no flag, which is the behaviour a session had before these existed.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Model {
    Opus,
    Sonnet,
    Fable,
}

impl Model {
    pub fn as_str(self) -> &'static str {
        match self {
            Model::Opus => "opus",
            Model::Sonnet => "sonnet",
            Model::Fable => "fable",
        }
    }

    /// A stored value that is not on the allowlist is `None`.
    pub fn parse(s: &str) -> Option<Self> {
        [Model::Opus, Model::Sonnet, Model::Fable].into_iter().find(|m| m.as_str() == s)
    }
}

/// `default`, `auto`, `dontAsk` and `bypassPermissions` are deliberately absent: the first
/// two are not the inbox gate, and the others remove it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum PermissionMode {
    #[serde(rename = "manual")]
    Manual,
    #[serde(rename = "acceptEdits")]
    AcceptEdits,
    #[serde(rename = "plan")]
    Plan,
}

impl PermissionMode {
    pub fn as_str(self) -> &'static str {
        match self {
            PermissionMode::Manual => "manual",
            PermissionMode::AcceptEdits => "acceptEdits",
            PermissionMode::Plan => "plan",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        [PermissionMode::Manual, PermissionMode::AcceptEdits, PermissionMode::Plan]
            .into_iter()
            .find(|m| m.as_str() == s)
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SessionSettings {
    pub model: Option<Model>,
    pub permission_mode: Option<PermissionMode>,
}

impl SessionSettings {
    /// From the two stored strings; an unknown one loads as no setting.
    pub fn from_stored(model: Option<&str>, permission_mode: Option<&str>) -> Self {
        Self { model: model.and_then(Model::parse), permission_mode: permission_mode.and_then(PermissionMode::parse) }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn serde_names_match_the_flag_strings() {
        for m in [Model::Opus, Model::Sonnet, Model::Fable] {
            assert_eq!(serde_json::to_string(&m).unwrap(), format!("\"{}\"", m.as_str()));
            assert_eq!(Model::parse(m.as_str()), Some(m));
        }
        for m in [PermissionMode::Manual, PermissionMode::AcceptEdits, PermissionMode::Plan] {
            assert_eq!(serde_json::to_string(&m).unwrap(), format!("\"{}\"", m.as_str()));
            assert_eq!(PermissionMode::parse(m.as_str()), Some(m));
        }
    }

    #[test]
    fn parse_rejects_everything_else() {
        for bad in ["haiku", "Opus", "--model", "", "opus "] {
            assert_eq!(Model::parse(bad), None, "{bad}");
        }
        for bad in ["bypassPermissions", "dontAsk", "auto", "default", "--x", "Plan", ""] {
            assert_eq!(PermissionMode::parse(bad), None, "{bad}");
        }
    }
}
