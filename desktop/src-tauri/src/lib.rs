mod commands;
mod git;
mod notify;
mod projects;
mod session;
mod window;

use tauri::{Emitter, Manager, RunEvent};
use tauri_plugin_notification::NotificationExt;

use commands::AppSessions;
use session::{Event, Sessions, process, store, transcript};

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let builder = tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_notification::init());
    // Registered on the builder so it restores the main window as it is created. The
    // frontend never calls it, so no capability is granted.
    #[cfg(desktop)]
    let builder = builder.plugin(tauri_plugin_window_state::Builder::default().build());
    let app = builder
        .setup(|app| {
            // The main window exists by now and the plugin has restored it: make sure
            // the result is on a display that still exists.
            #[cfg(desktop)]
            if let Some(main) = app.get_webview_window("main") {
                window::ensure_visible(&main);
            }
            let handle = app.handle().clone();
            let notify_handle = app.handle().clone();
            // Saving stays off for the run when the file cannot be loaded; see `store::open`.
            let data_dir = app.path().app_data_dir().ok();
            let (snapshot, status, writer) = match &data_dir {
                Some(dir) => store::open(&dir.join("state.json")),
                None => (None, store::StorageStatus::unreadable(), None),
            };
            // Transcripts follow the state file: with saving off they stay in memory.
            let transcripts = match (&data_dir, &writer) {
                (Some(dir), Some(_)) => transcript::TranscriptStore::disk(dir.join("transcripts")),
                _ => transcript::TranscriptStore::memory(),
            };
            // Task worktrees live under the app data dir, nowhere near the projects.
            let worktree_base = match &data_dir {
                Some(dir) => dir.join("worktrees"),
                None => std::env::temp_dir().join("orchestra-worktrees"),
            };
            let sessions: AppSessions = Sessions::with_writer(
                Box::new(move |event| {
                    let _ = match event {
                        Event::SessionUpdated(s) => handle.emit("session-updated", s),
                        Event::InboxUpdated(items) => handle.emit("inbox-updated", items),
                        Event::TranscriptAppended(batch) => handle.emit("transcript-appended", batch),
                        Event::TaskUpdated(task) => handle.emit("task-updated", task),
                    };
                }),
                Box::new(process::spawn),
                // Rust-only use of the plugin, as with dialog: no capability is granted.
                Box::new(move |project, notice| {
                    // Nothing to announce while the user is looking at the window.
                    let focused = notify_handle.get_webview_window("main").and_then(|w| w.is_focused().ok());
                    if focused == Some(true) {
                        return;
                    }
                    let _ = notify_handle
                        .notification()
                        .builder()
                        .title("Orchestra")
                        .body(notify::body(project, notice.kind))
                        .show();
                }),
                writer,
                transcripts,
                worktree_base,
            );
            if let Some(snapshot) = snapshot {
                sessions.restore(snapshot);
            }
            // Restore spawns nothing; queued tasks start now, off the setup thread.
            sessions.pump_in_background();
            // Cleanups left pending get one more try now that no child can be holding a folder.
            sessions.retry_pending_in_background();
            app.manage(sessions);
            app.manage(status);
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            commands::pick_project,
            commands::list_projects,
            commands::start_session,
            commands::list_sessions,
            commands::list_inbox,
            commands::answer,
            commands::stop_session,
            commands::create_task,
            commands::list_tasks,
            commands::cancel_task,
            commands::get_task_diff,
            commands::land_task,
            commands::discard_task,
            commands::retry_cleanup,
            commands::get_transcript,
            commands::storage_status,
        ])
        .build(tauri::generate_context!())
        .expect("error while building tauri application");

    app.run(|handle, event| {
        if let RunEvent::Exit = event {
            if let Some(sessions) = handle.try_state::<AppSessions>() {
                sessions.shutdown();
            }
        }
    });
}
