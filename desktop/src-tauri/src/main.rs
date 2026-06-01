// Hide the console window on Windows release builds.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod backend;
mod commands;
mod config;
mod fs_watch;

use backend::SupervisorHandle;
use fs_watch::FsWatchState;
use tauri::{Manager, RunEvent};

/// Shared application state managed by Tauri.
pub struct AppStateHandle {
    pub supervisor: SupervisorHandle,
}

#[cfg(unix)]
mod signals {
    use crate::backend::SupervisorHandle;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::time::Duration;

    static SIGNALLED: AtomicBool = AtomicBool::new(false);

    extern "C" fn handle(_sig: i32) {
        // Only async-signal-safe work here: flip an atomic.
        SIGNALLED.store(true, Ordering::SeqCst);
    }

    /// Install SIGTERM/SIGINT handlers + a watcher thread that performs the
    /// (non-signal-safe) graceful child shutdown and exits. This makes
    /// `kill`/Ctrl-C clean up the managed backend, not just window-close.
    pub fn install(supervisor: SupervisorHandle) {
        unsafe {
            libc::signal(libc::SIGTERM, handle as libc::sighandler_t);
            libc::signal(libc::SIGINT, handle as libc::sighandler_t);
        }
        std::thread::spawn(move || loop {
            if SIGNALLED.load(Ordering::SeqCst) {
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
            #[cfg(unix)]
            signals::install(supervisor.clone());
            app.manage(AppStateHandle { supervisor });
            app.manage(FsWatchState::new());
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            commands::get_api_url,
            commands::get_backend_routing,
            commands::get_backend_status,
            commands::watch_terminal,
            commands::unwatch_terminal,
            commands::clear_dirty
        ])
        .build(tauri::generate_context!())
        .expect("error while building Taime")
        .run(|app_handle, event| {
            // On exit, stop the managed backend child gracefully.
            if let RunEvent::ExitRequested { .. } = event {
                if let Some(state) = app_handle.try_state::<AppStateHandle>() {
                    state.supervisor.shutdown();
                }
            }
        });
}
