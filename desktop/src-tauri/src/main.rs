// Hide the console window on Windows release builds.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod backend;
mod commands;
mod config;
mod fs_watch;
mod pty;

use backend::SupervisorHandle;
use base64::Engine;
use fs_watch::FsWatchState;
use pty::{EventSink, PtyEvent, PtyManager};
use std::sync::Arc;
use tauri::{Emitter, Manager, RunEvent};

/// Shared application state managed by Tauri.
pub struct AppStateHandle {
    pub supervisor: SupervisorHandle,
}

#[cfg(unix)]
mod signals {
    use crate::backend::SupervisorHandle;
    use crate::pty::PtyManager;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::time::Duration;

    static SIGNALLED: AtomicBool = AtomicBool::new(false);

    extern "C" fn handle(_sig: i32) {
        // Only async-signal-safe work here: flip an atomic.
        SIGNALLED.store(true, Ordering::SeqCst);
    }

    /// Install SIGTERM/SIGINT handlers + a watcher thread that performs the
    /// (non-signal-safe) graceful child shutdown and exits. This makes
    /// `kill`/Ctrl-C clean up the managed backend AND any Rust-owned PTYs, not
    /// just window-close.
    pub fn install(supervisor: SupervisorHandle, pty: PtyManager) {
        unsafe {
            libc::signal(libc::SIGTERM, handle as libc::sighandler_t);
            libc::signal(libc::SIGINT, handle as libc::sighandler_t);
        }
        std::thread::spawn(move || loop {
            if SIGNALLED.load(Ordering::SeqCst) {
                pty.shutdown_all();
                supervisor.shutdown();
                std::process::exit(0);
            }
            std::thread::sleep(Duration::from_millis(100));
        });
    }
}

fn main() {
    let resolved = config::resolve();
    println!(
        "[taime] backend: {} (mode: {}, source: {})",
        resolved.api_url,
        if resolved.external_backend {
            "external"
        } else {
            "managed"
        },
        resolved.source
    );

    tauri::Builder::default()
        // Native folder picker for the workspace selector.
        .plugin(tauri_plugin_dialog::init())
        .setup(move |app| {
            let supervisor = backend::start(&app.handle(), resolved.clone());

            // Rust-owned PTY manager. Its output sink emits Tauri events keyed
            // by session id; the frontend listens on `pty://{id}/data|exit`.
            let app_handle = app.handle().clone();
            let sink: EventSink = Arc::new(move |id: &str, ev: PtyEvent| match ev {
                PtyEvent::Data(d) => {
                    let b64 = base64::engine::general_purpose::STANDARD.encode(&d);
                    let _ = app_handle.emit(&format!("pty://{id}/data"), b64);
                }
                PtyEvent::Exit(code) => {
                    let _ = app_handle.emit(&format!("pty://{id}/exit"), code);
                }
            });
            let pty = PtyManager::new(sink);

            #[cfg(unix)]
            signals::install(supervisor.clone(), pty.clone());
            app.manage(AppStateHandle { supervisor });
            app.manage(FsWatchState::new());
            app.manage(pty);
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            commands::get_api_url,
            commands::get_backend_routing,
            commands::get_backend_status,
            commands::watch_terminal,
            commands::unwatch_terminal,
            commands::clear_dirty,
            commands::pty_spawn_claude,
            commands::pty_write,
            commands::pty_resize,
            commands::pty_close_view,
            commands::pty_reattach_view,
            commands::pty_kill,
            commands::pty_list,
            commands::set_clipboard_image_from_path
        ])
        .build(tauri::generate_context!())
        .expect("error while building Taime")
        .run(|app_handle, event| {
            // On exit, kill Rust-owned PTYs and stop the managed backend child.
            if let RunEvent::ExitRequested { .. } = event {
                if let Some(pty) = app_handle.try_state::<PtyManager>() {
                    pty.shutdown_all();
                }
                if let Some(state) = app_handle.try_state::<AppStateHandle>() {
                    state.supervisor.shutdown();
                }
            }
        });
}
