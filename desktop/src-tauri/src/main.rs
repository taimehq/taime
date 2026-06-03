// Hide the console window on Windows release builds.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod commands;
mod daemon;
mod fs_watch;

use daemon::DaemonClient;
use fs_watch::FsWatchState;

fn main() {
    println!("[taime] backend: taime-session-daemon (detached; CAO/tmux removed)");

    tauri::Builder::default()
        // Native folder picker for the workspace selector.
        .plugin(tauri_plugin_dialog::init())
        .setup(move |app| {
            use tauri::Manager;
            app.manage(FsWatchState::new());
            // App-side client to the detached session daemon — the one (and only)
            // backend now. Resolved next to the app exe (dev) or in Resources
            // (bundle); the daemon OUTLIVES the app (not a supervised sidecar —
            // nothing to shut down on exit).
            app.manage(DaemonClient::new(daemon::resolve_daemon_bin()));
            // Warm it up in the background so the workspace probe / list / graph
            // reads work on the first paint instead of returning empty until the
            // first launch spawns the daemon.
            let handle = app.handle().clone();
            tauri::async_runtime::spawn(async move {
                if let Some(client) = handle.try_state::<DaemonClient>() {
                    client.warm_up().await;
                }
            });
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            commands::watch_terminal,
            commands::unwatch_terminal,
            commands::clear_dirty,
            commands::daemon_spawn_agent,
            commands::daemon_provision_worktree,
            commands::daemon_send_message,
            commands::daemon_activity_graph,
            commands::daemon_query,
            commands::daemon_attach,
            commands::daemon_write,
            commands::daemon_resize,
            commands::daemon_ack,
            commands::daemon_checkpoint,
            commands::daemon_close_view,
            commands::daemon_kill,
            commands::daemon_list,
            commands::daemon_available,
            commands::set_clipboard_image_from_path
        ])
        .run(tauri::generate_context!())
        .expect("error while building Taime");
}
