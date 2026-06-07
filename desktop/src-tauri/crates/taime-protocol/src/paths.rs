//! Deterministic per-user daemon socket path, shared so the app and the daemon
//! agree without any advertisement file. The socket lives in the short Darwin
//! per-user temp dir (honors `$TMPDIR`) under a 0700 `taime/` subdir, with a
//! short hashed name that stays well under the `sun_path` limit.

use std::io;
use std::path::PathBuf;

/// macOS `sun_path` is 104 bytes; Linux is 108. The whole path must fit.
pub const SUN_PATH_MAX: usize = if cfg!(target_os = "macos") { 104 } else { 108 };

fn fnv1a32(s: &str) -> u32 {
    let mut h: u32 = 0x811c_9dc5;
    for b in s.bytes() {
        h ^= b as u32;
        h = h.wrapping_mul(0x0100_0193);
    }
    h
}

#[cfg(unix)]
fn current_uid() -> u32 {
    unsafe { libc::getuid() }
}
#[cfg(not(unix))]
fn current_uid() -> u32 {
    0
}

/// The per-user `taime/` runtime directory under the system temp dir. Not
/// created here — the daemon creates + chmods it on bind; the app only derives.
pub fn runtime_dir() -> PathBuf {
    std::env::temp_dir().join("taime")
}

/// The deterministic per-user daemon socket path. Errors if it would overflow
/// `sun_path` (fail loudly rather than truncate).
pub fn default_socket_path() -> io::Result<PathBuf> {
    let h = fnv1a32(&format!("taime-{}", current_uid()));
    let socket = runtime_dir().join(format!("taime-{h:08x}.sock"));
    let len = socket.as_os_str().len();
    if len >= SUN_PATH_MAX {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("socket path {len}B exceeds sun_path cap {SUN_PATH_MAX}: {socket:?}"),
        ));
    }
    Ok(socket)
}

/// Sibling token path for a socket (`*.token`).
pub fn token_path(socket: &std::path::Path) -> PathBuf {
    socket.with_extension("token")
}

/// Sibling lock path for a socket (`*.lock`).
pub fn lock_path(socket: &std::path::Path) -> PathBuf {
    socket.with_extension("lock")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_socket_is_deterministic_and_fits() {
        let a = default_socket_path().unwrap();
        let b = default_socket_path().unwrap();
        assert_eq!(a, b, "socket path must be deterministic for app/daemon agreement");
        assert!(a.as_os_str().len() < SUN_PATH_MAX);
        assert_eq!(token_path(&a).extension().unwrap(), "token");
        assert_eq!(lock_path(&a).extension().unwrap(), "lock");
    }
}
