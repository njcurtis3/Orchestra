mod commands;
mod projects;
mod session;

use tauri::{Emitter, Manager, RunEvent};

use commands::AppSessions;
use session::{Event, Sessions, process};

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let app = tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .setup(|app| {
            let handle = app.handle().clone();
            let sessions: AppSessions = Sessions::new(
                Box::new(move |event| {
                    let _ = match event {
                        Event::SessionUpdated(s) => handle.emit("session-updated", s),
                        Event::InboxUpdated(items) => handle.emit("inbox-updated", items),
                    };
                }),
                Box::new(process::spawn),
            );
            app.manage(sessions);
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
        ])
        .build(tauri::generate_context!())
        .expect("error while building tauri application");

    app.run(|handle, event| {
        if let RunEvent::Exit = event {
            if let Some(sessions) = handle.try_state::<AppSessions>() {
                sessions.kill_all();
            }
        }
    });
}
