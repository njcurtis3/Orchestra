//! IPC surface. Argument types are strict serde structs; everything else is validated in
//! `session`. Errors are short strings with no paths.

use std::sync::Arc;

use orchestra_core::wire::args::{
    Answer, CancelTaskArgs, CreateTaskArgs, StartArgs, StopArgs, TaskIdArgs, TranscriptArgs,
};
use tauri::State;
use tauri_plugin_dialog::DialogExt;

use crate::projects::Project;
use crate::session::{SessionSummary, TaskDiff};
use crate::session::Sessions;
use crate::session::state::InboxItem;
use crate::session::task::TaskSummary;
use crate::session::store::StorageStatus;
use crate::session::transcript::TranscriptPage;

pub type AppSessions = Arc<Sessions>;

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

/// Async so the stdin write runs on the blocking pool, not the main thread.
#[tauri::command]
pub async fn start_session(args: StartArgs, sessions: State<'_, AppSessions>) -> Result<SessionSummary, String> {
    let sessions = Arc::clone(sessions.inner());
    tauri::async_runtime::spawn_blocking(move || sessions.start(&args.project_id, &args.prompt, args.settings()))
        .await
        .map_err(|_| "session task failed".to_string())?
}

#[tauri::command]
pub fn list_sessions(sessions: State<'_, AppSessions>) -> Vec<SessionSummary> {
    sessions.list_sessions()
}

#[tauri::command]
pub fn list_inbox(sessions: State<'_, AppSessions>) -> Vec<InboxItem> {
    sessions.list_inbox()
}

/// Async for the same reason as `start_session`.
#[tauri::command]
pub async fn answer(args: Answer, sessions: State<'_, AppSessions>) -> Result<(), String> {
    let sessions = Arc::clone(sessions.inner());
    tauri::async_runtime::spawn_blocking(move || sessions.answer(args).map_err(String::from))
        .await
        .map_err(|_| "answer task failed".to_string())?
}

/// Async because it reads the session's file.
#[tauri::command]
pub async fn get_transcript(args: TranscriptArgs, sessions: State<'_, AppSessions>) -> Result<TranscriptPage, String> {
    let sessions = Arc::clone(sessions.inner());
    tauri::async_runtime::spawn_blocking(move || sessions.transcript(&args.session_id))
        .await
        .map_err(|_| "transcript task failed".to_string())?
}

/// Whether history was loaded and is being saved. Carries no path.
#[tauri::command]
pub fn storage_status(status: State<'_, StorageStatus>) -> StorageStatus {
    status.inner().clone()
}

/// Async because git runs, and so does a session's first write when a slot is free.
#[tauri::command]
pub async fn create_task(args: CreateTaskArgs, sessions: State<'_, AppSessions>) -> Result<TaskSummary, String> {
    let sessions = Arc::clone(sessions.inner());
    tauri::async_runtime::spawn_blocking(move || sessions.create_task(&args.project_id, &args.prompt, args.settings()))
        .await
    .map_err(|_| "task creation failed".to_string())?
}

#[tauri::command]
pub fn list_tasks(sessions: State<'_, AppSessions>) -> Vec<TaskSummary> {
    sessions.list_tasks()
}

/// Async so the queue it re-runs never works on the main thread.
#[tauri::command]
pub async fn cancel_task(args: CancelTaskArgs, sessions: State<'_, AppSessions>) -> Result<(), String> {
    let sessions = Arc::clone(sessions.inner());
    tauri::async_runtime::spawn_blocking(move || sessions.cancel_task(&args.task_id))
        .await
        .map_err(|_| "cancel task failed".to_string())?
}

/// The review of a finished task: files, capped patch and commits. Async because git runs.
#[tauri::command]
pub async fn get_task_diff(args: TaskIdArgs, sessions: State<'_, AppSessions>) -> Result<TaskDiff, String> {
    let sessions = Arc::clone(sessions.inner());
    tauri::async_runtime::spawn_blocking(move || sessions.get_task_diff(&args.task_id))
        .await
        .map_err(|_| "diff task failed".to_string())?
}

/// Merge a finished task into its base branch. Rust enforces the checks whatever the UI
/// confirmed. Async because git runs, and a hook may take a while.
#[tauri::command]
pub async fn land_task(args: TaskIdArgs, sessions: State<'_, AppSessions>) -> Result<TaskSummary, String> {
    let sessions = Arc::clone(sessions.inner());
    tauri::async_runtime::spawn_blocking(move || sessions.land_task(&args.task_id))
        .await
        .map_err(|_| "land task failed".to_string())?
}

#[tauri::command]
pub async fn discard_task(args: TaskIdArgs, sessions: State<'_, AppSessions>) -> Result<TaskSummary, String> {
    let sessions = Arc::clone(sessions.inner());
    tauri::async_runtime::spawn_blocking(move || sessions.discard_task(&args.task_id))
        .await
        .map_err(|_| "discard task failed".to_string())?
}

#[tauri::command]
pub async fn retry_cleanup(args: TaskIdArgs, sessions: State<'_, AppSessions>) -> Result<TaskSummary, String> {
    let sessions = Arc::clone(sessions.inner());
    tauri::async_runtime::spawn_blocking(move || sessions.retry_cleanup(&args.task_id))
        .await
        .map_err(|_| "cleanup task failed".to_string())?
}

/// Async for the same reason as `start_session`: the kill runs on the blocking pool.
#[tauri::command]
pub async fn stop_session(args: StopArgs, sessions: State<'_, AppSessions>) -> Result<(), String> {
    let sessions = Arc::clone(sessions.inner());
    tauri::async_runtime::spawn_blocking(move || sessions.stop(&args.session_id))
        .await
        .map_err(|_| "stop task failed".to_string())?
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::settings::{Model, PermissionMode};
    use crate::session::state::Decision;
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
        assert!(serde_json::from_value::<TranscriptArgs>(json!({"session_id":"s1","path":"C:/x"})).is_err());
        assert!(serde_json::from_value::<TranscriptArgs>(json!({"session_id":"s1"})).is_ok());
    }

    #[test]
    fn task_args_reject_extra_fields_and_unlisted_values() {
        let ok = json!({"project_id":"p1","prompt":"x","model":"opus","permission_mode":"plan"});
        assert!(serde_json::from_value::<CreateTaskArgs>(ok).is_ok());
        for extra in ["cwd", "worktree", "branch", "path"] {
            assert!(serde_json::from_value::<CreateTaskArgs>(json!({"project_id":"p1","prompt":"x", extra: "C:/"})).is_err());
        }
        assert!(serde_json::from_value::<CreateTaskArgs>(json!({"project_id":"p1","prompt":"x","model":"haiku"})).is_err());
        assert!(serde_json::from_value::<CancelTaskArgs>(json!({"task_id":"t1","force":true})).is_err());
        assert!(serde_json::from_value::<CancelTaskArgs>(json!({"task_id":"t1"})).is_ok());
    }

    #[test]
    fn review_args_take_a_task_id_and_nothing_else() {
        assert!(serde_json::from_value::<TaskIdArgs>(json!({"task_id":"t1"})).is_ok());
        for extra in ["path", "branch", "force", "cwd"] {
            assert!(serde_json::from_value::<TaskIdArgs>(json!({"task_id":"t1", extra: "x"})).is_err());
        }
        assert!(serde_json::from_value::<TaskIdArgs>(json!({})).is_err());
    }

    #[test]
    fn start_args_accept_listed_values_and_absence() {
        let a: StartArgs = serde_json::from_value(
            json!({"project_id":"p1","prompt":"x","model":"fable","permission_mode":"acceptEdits"}),
        )
        .unwrap();
        assert_eq!((a.model, a.permission_mode), (Some(Model::Fable), Some(PermissionMode::AcceptEdits)));
        let a: StartArgs = serde_json::from_value(json!({"project_id":"p1","prompt":"x","model":null})).unwrap();
        assert_eq!((a.model, a.permission_mode), (None, None));
    }

    #[test]
    fn start_args_reject_unlisted_values() {
        let with = |k: &str, v: &str| serde_json::from_value::<StartArgs>(json!({"project_id":"p1","prompt":"x", k: v}));
        for bad in ["haiku", "Opus", "--model", "opus --x", ""] {
            assert!(with("model", bad).is_err(), "model {bad}");
        }
        for bad in ["bypassPermissions", "dontAsk", "auto", "default", "--dangerously-skip-permissions", "Plan", ""] {
            assert!(with("permission_mode", bad).is_err(), "mode {bad}");
        }
        assert!(serde_json::from_value::<StartArgs>(json!({"project_id":"p1","prompt":"x","model":3})).is_err());
    }
}
