//! IPC surface. Argument types are strict serde structs; everything else is validated in
//! `session`. Errors are short strings with no paths.

use std::collections::BTreeMap;
use std::sync::Arc;

use serde::Deserialize;
use tauri::State;
use tauri_plugin_dialog::DialogExt;

use crate::projects::Project;
use crate::session::SessionSummary;
use crate::session::Sessions;
use crate::session::state::{Decision, InboxItem};

pub type AppSessions = Arc<Sessions>;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StartArgs {
    pub project_id: String,
    pub prompt: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StopArgs {
    pub session_id: String,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Answer {
    Permission { item_id: String, decision: Decision, message: Option<String> },
    Question { item_id: String, answers: BTreeMap<String, String> },
    Reply { session_id: String, text: String },
}

/// Async so the blocking folder dialog runs off the main thread.
#[tauri::command]
pub async fn pick_project(
    app: tauri::AppHandle,
    sessions: State<'_, AppSessions>,
) -> Result<Option<Project>, String> {
    let Some(picked) = app.dialog().file().blocking_pick_folder() else {
        return Ok(None);
    };
    let path = picked.into_path().map_err(|_| "could not read the chosen folder".to_string())?;
    sessions.add_project(&path).map(Some)
}

#[tauri::command]
pub fn list_projects(sessions: State<'_, AppSessions>) -> Vec<Project> {
    sessions.list_projects()
}

#[tauri::command]
pub fn start_session(args: StartArgs, sessions: State<'_, AppSessions>) -> Result<SessionSummary, String> {
    sessions.inner().start(&args.project_id, &args.prompt)
}

#[tauri::command]
pub fn list_sessions(sessions: State<'_, AppSessions>) -> Vec<SessionSummary> {
    sessions.list_sessions()
}

#[tauri::command]
pub fn list_inbox(sessions: State<'_, AppSessions>) -> Vec<InboxItem> {
    sessions.list_inbox()
}

#[tauri::command]
pub fn answer(args: Answer, sessions: State<'_, AppSessions>) -> Result<(), String> {
    dispatch_answer(&sessions, args)
}

fn dispatch_answer(sessions: &Sessions, args: Answer) -> Result<(), String> {
    match args {
        Answer::Permission { item_id, decision, message } => {
            sessions.answer_permission(&item_id, decision, message.as_deref())
        }
        Answer::Question { item_id, answers } => sessions.answer_question(&item_id, &answers),
        Answer::Reply { session_id, text } => sessions.reply(&session_id, &text),
    }
}

#[tauri::command]
pub fn stop_session(args: StopArgs, sessions: State<'_, AppSessions>) -> Result<(), String> {
    sessions.stop(&args.session_id)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn answer_parses_each_kind() {
        let a: Answer = serde_json::from_value(json!({"kind":"permission","item_id":"i","decision":"allow"})).unwrap();
        assert!(matches!(a, Answer::Permission { decision: Decision::Allow, message: None, .. }));
        let a: Answer = serde_json::from_value(json!({"kind":"permission","item_id":"i","decision":"deny","message":"no"})).unwrap();
        assert!(matches!(a, Answer::Permission { decision: Decision::Deny, message: Some(_), .. }));
        let a: Answer = serde_json::from_value(json!({"kind":"question","item_id":"i","answers":{"q":"a"}})).unwrap();
        assert!(matches!(a, Answer::Question { .. }));
        let a: Answer = serde_json::from_value(json!({"kind":"reply","session_id":"s","text":"t"})).unwrap();
        assert!(matches!(a, Answer::Reply { .. }));
    }

    #[test]
    fn answer_rejects_bad_decision_unknown_kind_and_extra_fields() {
        assert!(serde_json::from_value::<Answer>(json!({"kind":"permission","item_id":"i","decision":"maybe"})).is_err());
        assert!(serde_json::from_value::<Answer>(json!({"kind":"shell","cmd":"x"})).is_err());
        assert!(serde_json::from_value::<Answer>(json!({"kind":"reply","session_id":"s","text":"t","path":"C:/x"})).is_err());
        assert!(serde_json::from_value::<Answer>(json!({"item_id":"i","decision":"allow"})).is_err());
    }

    #[test]
    fn arg_structs_reject_extra_fields() {
        assert!(serde_json::from_value::<StartArgs>(json!({"project_id":"p1","prompt":"x","cwd":"C:/"})).is_err());
        assert!(serde_json::from_value::<StartArgs>(json!({"project_id":"p1","prompt":"x"})).is_ok());
        assert!(serde_json::from_value::<StopArgs>(json!({"session_id":"s1","force":true})).is_err());
    }
}
