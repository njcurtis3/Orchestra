mod commands;
mod keyring;
mod link;
mod link_config;
mod qr;
mod window;

use tauri::{Emitter, Manager, RunEvent};
use tauri_plugin_notification::NotificationExt;

use orchestra_core::{host, notify, projects, session};

use commands::AppSessions;
use link::{LinkRef, LinkState};
use session::{Event, process};

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
            let (sessions, status): (AppSessions, _) = host::open_host(
                data_dir.as_deref(),
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
            );
            // The phone link: its files sit beside the app's, and it is off unless the saved
            // settings say otherwise. Starting it waits on sockets, so it runs off this thread.
            let link_handle = app.handle().clone();
            let link = LinkState::new(
                std::sync::Arc::clone(&sessions),
                status.clone(),
                data_dir.as_ref().map(|dir| dir.join("link")),
                link::host_name_from_env(),
                std::sync::Arc::new(keyring::provider_for),
                std::sync::Arc::new(move |name, payload| {
                    let _ = link_handle.emit(name, payload);
                }),
            );
            link.auto_start();
            app.manage(sessions);
            app.manage(status);
            app.manage(link);
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
            link::link_status,
            link::link_interfaces,
            link::link_set_config,
            link::link_open_pairing,
            link::link_close_pairing,
            link::link_pending,
            link::link_confirm_pairing,
            link::link_reject_pairing,
            link::link_list_devices,
            link::link_set_scopes,
            link::link_revoke,
        ])
        .build(tauri::generate_context!())
        .expect("error while building tauri application");

    app.run(|handle, event| {
        if let RunEvent::Exit = event {
            // Stopping the link joins its threads, which can take long: wait 2 seconds at most.
            if let Some(link) = handle.try_state::<LinkRef>() {
                link.shutdown(std::time::Duration::from_secs(2));
            }
            if let Some(sessions) = handle.try_state::<AppSessions>() {
                sessions.shutdown();
            }
        }
    });
}

#[cfg(test)]
mod tests {
    /// The webview's CSP is part of the link's safety: the pairing QR is an `<img>` data URL
    /// and nothing inline may run. Changing it is a decision, so the exact text is pinned.
    #[test]
    fn csp_pinned() {
        let conf: serde_json::Value = serde_json::from_str(include_str!("../tauri.conf.json")).unwrap();
        assert_eq!(
            conf["app"]["security"]["csp"].as_str(),
            Some(
                "default-src 'self'; script-src 'self'; style-src 'self'; img-src 'self' data:; font-src 'self'; connect-src ipc: http://ipc.localhost"
            )
        );
    }
}
