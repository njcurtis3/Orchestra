mod commands;
mod projects;
mod session;
mod window;

use tauri::{Emitter, Manager, RunEvent};

use commands::AppSessions;
use session::{Event, Sessions, process, store, transcript};

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let builder = tauri::Builder::default().plugin(tauri_plugin_dialog::init());
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
            let sessions: AppSessions = Sessions::with_writer(
                Box::new(move |event| {
                    let _ = match event {
                        Event::SessionUpdated(s) => handle.emit("session-updated", s),
                        Event::InboxUpdated(items) => handle.emit("inbox-updated", items),
                        Event::TranscriptAppended(batch) => handle.emit("transcript-appended", batch),
                    };
                }),
                Box::new(process::spawn),
                writer,
                transcripts,
            );
            if let Some(snapshot) = snapshot {
                sessions.restore(snapshot);
            }
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
