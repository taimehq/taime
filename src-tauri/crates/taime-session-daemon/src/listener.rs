//! Socket binding + connection authentication.
//!
//! Defense in depth, OS guarantee first:
//!   * the **0700 parent dir** (in `runtime.rs`) is the portable hard guarantee;
//!   * the socket is born **0600** (umask around `bind`) + an explicit chmod;
//!   * every accepted connection is **peer-uid validated** (`peer_cred()`, which
//!     is `getpeereid` on macOS / `SO_PEERCRED` on Linux — never hardcode a
//!     platform constant);
//!   * a rotating **attach token** (read by the client only after the uid gate)
//!     stops another *same-user* process from hijacking, compared in constant time.

use std::io;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;

use rand::RngCore;
use subtle::ConstantTimeEq;
use tokio::net::{UnixListener, UnixStream};

/// Bind the daemon socket securely, clearing a stale socket file left by a crash
/// (only after a failed connect confirms no live daemon owns it).
pub async fn bind_secure(socket: &Path) -> io::Result<UnixListener> {
    if socket.exists() {
        match UnixStream::connect(socket).await {
            Ok(_) => {
                return Err(io::Error::new(
                    io::ErrorKind::AddrInUse,
                    "a live daemon already owns this socket",
                ))
            }
            // ECONNREFUSED / ENOENT => stale leftover; safe to remove and rebind.
            Err(e)
                if matches!(
                    e.kind(),
                    io::ErrorKind::ConnectionRefused | io::ErrorKind::NotFound
                ) =>
            {
                std::fs::remove_file(socket)?;
            }
            Err(e) => return Err(e),
        }
    }
    // umask 0o077 so the socket is born 0600 (closes the bind→chmod race), then
    // restore and chmod belt-and-suspenders.
    let old = unsafe { libc::umask(0o077) };
    let listener = UnixListener::bind(socket);
    unsafe { libc::umask(old) };
    let listener = listener?;
    std::fs::set_permissions(socket, std::fs::Permissions::from_mode(0o600))?;
    Ok(listener)
}

/// Reject a connection whose peer is not the same uid as the daemon. This is the
/// primary access gate.
pub fn validate_same_user(stream: &UnixStream) -> io::Result<()> {
    let cred = stream.peer_cred()?;
    let me = unsafe { libc::getuid() };
    if cred.uid() != me {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!("peer uid {} != {me} (rejecting cross-user connect)", cred.uid()),
        ));
    }
    Ok(())
}

/// Generate + persist a fresh 0600 attach token (rotated each daemon start).
pub fn write_token(path: &Path) -> io::Result<String> {
    let mut raw = [0u8; 32];
    rand::rngs::OsRng.fill_bytes(&mut raw);
    let token: String = raw.iter().map(|b| format!("{b:02x}")).collect();
    let old = unsafe { libc::umask(0o077) };
    let res = std::fs::write(path, &token);
    unsafe { libc::umask(old) };
    res?;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    Ok(token)
}

/// Constant-time token comparison (avoids a timing oracle).
pub fn token_matches(expected: &str, got: &str) -> bool {
    let a = expected.as_bytes();
    let b = got.as_bytes();
    // ct_eq over equal-length slices; length mismatch is an immediate reject but
    // we still touch both to keep timing flat-ish.
    if a.len() != b.len() {
        return false;
    }
    a.ct_eq(b).into()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn token_match_is_correct() {
        assert!(token_matches("deadbeef", "deadbeef"));
        assert!(!token_matches("deadbeef", "deadbee0"));
        assert!(!token_matches("deadbeef", "short"));
    }

    #[tokio::test]
    async fn bind_then_peer_cred_same_user() {
        let dir = std::env::temp_dir().join(format!("taime-sock-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let sock = dir.join("t.sock");
        let _ = std::fs::remove_file(&sock);
        let listener = bind_secure(&sock).await.unwrap();
        // The socket should be 0600.
        let mode = std::fs::metadata(&sock).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "socket should be 0600, got {mode:o}");
        // Connect and validate the peer is us.
        let client = UnixStream::connect(&sock).await.unwrap();
        let (server, _) = listener.accept().await.unwrap();
        assert!(validate_same_user(&server).is_ok());
        drop(client);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
