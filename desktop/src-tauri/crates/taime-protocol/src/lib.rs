//! Wire protocol for the Taime session daemon.
//!
//! A single Unix-socket connection carries **length-delimited frames**
//! (`tokio_util::codec::LengthDelimitedCodec`: u32 big-endian length prefix,
//! stripped on decode). Inside each frame the first byte is a **type tag**:
//!
//! ```text
//! [len:u32 BE]                         <- added/stripped by the codec
//! [type:u8][payload...]                <- this crate defines the payload
//! ```
//!
//! Two payload shapes:
//!   * **control** (`T_CONTROL`) — a postcard-serialized [`ClientMsg`] (app→daemon)
//!     or [`ServerMsg`] (daemon→app). Low volume; serde is fine.
//!   * **data** (`T_DATA`) / **repaint** (`T_REPAINT`) — hand-rolled
//!     `[type:u8][offset:u64 BE][raw bytes]`. NO serde on the hot path (a serde
//!     `Vec<u8>` would add a redundant inner length + copy inside the already
//!     length-delimited frame).
//!
//! `postcard`, not bincode: bincode 3.0.0 is a deliberate non-compiling stub and
//! all versions trip RUSTSEC-2025-0141. postcard is serde-compatible + maintained.

use bytes::{Buf, BufMut, Bytes, BytesMut};
use serde::{Deserialize, Serialize};

pub mod paths;

// ---------------------------------------------------------------------------
// Versioning + capabilities (the versioned-handshake substrate).
// ---------------------------------------------------------------------------

/// Magic word identifying a Taime daemon socket: ASCII "taim".
pub const MAGIC: u32 = 0x7461_696d;

/// Protocol version. Bump on any incompatible change to message shapes/framing.
/// The app's daemon-upgrade policy keys off this: a daemon whose version differs
/// (or is missing a required capability) is *adopted read-only* for its existing
/// sessions (List + Kill) rather than killed.
pub const PROTOCOL_VERSION: u16 = 1;

/// Feature flags negotiated in the handshake (`capabilities` bitset). Reserving
/// the bits now keeps app-update-while-old-daemon-running safe.
pub mod cap {
    /// Daemon can hand a PTY master fd across the socket (Step 2.5, SCM_RIGHTS).
    pub const SCM_RIGHTS: u32 = 1 << 0;
    /// Daemon serves ranged scrollback history (`GetHistory`).
    pub const HISTORY: u32 = 1 << 1;
    /// Daemon emits attribution turn-boundary events.
    pub const ATTRIBUTION: u32 = 1 << 2;

    /// Capabilities this build advertises.
    pub const CURRENT: u32 = ATTRIBUTION;
}

/// Two protocol peers are compatible iff they agree on `PROTOCOL_VERSION`. The
/// caller additionally checks that any *required* capability bits are present.
pub fn versions_compatible(a: u16, b: u16) -> bool {
    a == b
}

// ---------------------------------------------------------------------------
// Frame type tags (the byte at payload\[0\]).
// ---------------------------------------------------------------------------

/// Control payload: a postcard-encoded [`ClientMsg`] / [`ServerMsg`].
pub const T_CONTROL: u8 = 0x01;
/// Live PTY output: `[u64 BE start_offset][raw bytes]`.
pub const T_DATA: u8 = 0x02;
/// Visible-screen repaint at attach: `[u64 BE seq_n][escape-sequence bytes]`.
/// `seq_n` is the byte offset the grid reflects (`[0, seq_n)`); the client writes
/// these bytes to xterm after the reset prelude, then applies T_DATA frames whose
/// `start_offset >= seq_n`.
pub const T_REPAINT: u8 = 0x03;

/// Suggested `max_frame_length` for control/data; repaint can be larger.
pub const MAX_FRAME_LEN: usize = 16 * 1024 * 1024;

// ---------------------------------------------------------------------------
// Control messages.
// ---------------------------------------------------------------------------

/// Spawn parameters for a new session. The daemon captures env at spawn; later
/// app env changes only affect *new* sessions unless `RefreshEnv` is sent.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SpawnSpec {
    pub prog: String,
    pub args: Vec<String>,
    pub cwd: Option<String>,
    pub env: Vec<(String, String)>,
    pub rows: u16,
    pub cols: u16,
    /// Opaque attribution key the app ties to dirty/diff/graph (the worktree
    /// terminal id). The daemon stores + echoes it; it does not interpret it.
    pub attribution_key: Option<String>,
}

/// One MCP server entry the daemon should register for an agent at spawn (the
/// per-provider injection of Phase 1). Mirrors CAO's `profile.mcpServers[name]`:
/// the daemon stamps each server's `env` with `CAO_TERMINAL_ID = attribution_key`
/// before injecting it per the provider's strategy (inline `--mcp-config` JSON,
/// codex `-c mcp_servers.*` overrides, or `~/.gemini/settings.json` merge). In
/// Phase 1 these come straight from the CAO profile ("wired to CAO's MCP for
/// now"); Phase 5 swaps in the daemon's own endpoint without changing this shape.
#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
pub struct McpServerConfig {
    pub name: String,
    pub command: String,
    pub args: Vec<String>,
    pub env: Vec<(String, String)>,
}

/// An agent profile — the CAO `agent_profile` decomposed into the data a provider
/// adapter needs to build a capable launch command (system prompt, model,
/// permission mode, tool restrictions, MCP servers, and the provider-specific
/// native-agent / codex-profile escape hatches). All optional: an all-`None`
/// profile is the "default" (unrestricted, no system prompt, no MCP) launch that
/// matches today's `daemon_spawn_claude`.
#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
pub struct AgentProfile {
    /// Profile name (e.g. "default", "developer"); display + log only.
    pub name: String,
    /// System prompt injected per provider (claude `--append-system-prompt`,
    /// codex `developer_instructions`, gemini `GEMINI.md`). Already includes any
    /// skill-catalog text the app appended.
    pub system_prompt: Option<String>,
    pub model: Option<String>,
    /// Claude permission mode ("default"/"acceptEdits"/"plan"/"bypassPermissions");
    /// when absent (or tools unrestricted) the provider uses its skip/yolo default.
    pub permission_mode: Option<String>,
    /// CAO-vocabulary allowed tools. `["*"]` or empty ⇒ unrestricted.
    pub allowed_tools: Vec<String>,
    /// Claude `--agent <name>` thin-wrapper (delegates config to Claude's native
    /// agent store); when set, most other fields are ignored.
    pub native_agent: Option<String>,
    /// Codex `--profile <name>` (codex's own profile system).
    pub codex_profile: Option<String>,
    /// MCP servers to inject at spawn (see [`McpServerConfig`]).
    pub mcp_servers: Vec<McpServerConfig>,
}

/// High-level "launch agent X" request (the Phase-1 generalization of the
/// Claude-only `SpawnSpec`). The daemon's provider **registry** turns this into
/// the concrete PTY command + MCP injection — the recipe lives daemon-side so
/// Phase-5 headless `assign` can spawn workers without the app. `attribution_key`
/// doubles as the `CAO_TERMINAL_ID` stamped into MCP server envs.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentSpawnSpec {
    /// Provider id: `claude_code` | `codex` | `gemini_cli` | `grok_cli`.
    pub provider: String,
    pub profile: AgentProfile,
    pub cwd: Option<String>,
    pub rows: u16,
    pub cols: u16,
    pub attribution_key: Option<String>,
    /// Optional first prompt to seed after the agent is ready (reserved; the app
    /// drives initial input today). Stored for Phase-5 `assign`/`handoff` seeding.
    pub seed_prompt: Option<String>,
    /// Extra env overrides applied last (after the provider's own env).
    pub env: Vec<(String, String)>,
}

/// App → daemon. The first message on every connection MUST be `Hello`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ClientMsg {
    Hello {
        magic: u32,
        protocol_version: u16,
        capabilities: u32,
        /// Defense-in-depth token read from the 0600 sibling file (after the
        /// peer-uid gate). Empty string when the daemon advertised no token.
        attach_token: String,
    },
    /// Create a session from a low-level, fully-resolved [`SpawnSpec`] (the
    /// Claude-only Step-2 path; still used for back-compat). `req_id` correlates
    /// the `Spawned` reply.
    Spawn { req_id: u64, spec: SpawnSpec },
    /// Create a session from a high-level [`AgentSpawnSpec`]: the daemon's
    /// provider registry builds the command + injects MCP. `req_id` correlates
    /// the `Spawned` reply. This is the Phase-1 all-CLI spawn path.
    SpawnAgent { req_id: u64, spec: AgentSpawnSpec },
    /// Enumerate sessions. `req_id` correlates the `Sessions` reply.
    List { req_id: u64 },
    /// Bind THIS connection to stream `session_id`'s output. Carries the client's
    /// current viewport so the daemon **resizes the PTY + emulator BEFORE
    /// snapshotting the grid** (handoff step 1 = Resize) — otherwise the repaint
    /// would reflect the spawn-time size, not the real viewport. Triggers the
    /// handoff (`AttachOk` → `T_REPAINT` → `T_DATA` frames). One session per conn.
    Attach { session_id: String, rows: u16, cols: u16 },
    /// Stop streaming on this connection without killing the agent (close_view).
    Detach,
    /// Write bytes to the attached session's PTY (stdin). Low volume → control.
    Input { bytes: Vec<u8> },
    /// Resize the attached session's PTY (delivers SIGWINCH). First-class message.
    Resize { rows: u16, cols: u16 },
    /// Backpressure: highest byte offset the client has *processed* (xterm
    /// write-callback fired), batched ~once/16 ms, monotonic.
    AckBytes { offset: u64 },
    /// Explicitly terminate a session's process tree.
    Kill { session_id: String },
    /// Ranged scrollback backfill (reserved; unimplemented in Step 2).
    GetHistory { session_id: String, before_seq: u64, max_lines: u32 },
    /// App-driven attribution boundary (strongest signal): the app knows when it
    /// pressed Enter / launched an agent. `cause` is a short label.
    Checkpoint { cause: String },
    /// Re-send env to an existing session (does not restart it).
    RefreshEnv { env: Vec<(String, String)> },
    /// Liveness ping so the daemon can tell "app closed" from "app crashed".
    Heartbeat,
    /// Reserved (Step 2.5): the next message carries exactly one fd in SCM_RIGHTS
    /// ancillary data. Present so the protocol does not preclude fd handoff.
    FdFollows { session_id: String, kind: FdKind },
}

/// What an SCM_RIGHTS-passed fd represents (Step 2.5, reserved).
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum FdKind {
    /// The PTY master fd for a live session (zero-downtime daemon upgrade).
    PtyMaster,
}

/// Daemon → app.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ServerMsg {
    HelloOk {
        magic: u32,
        protocol_version: u16,
        capabilities: u32,
        daemon_version: String,
    },
    /// `Hello` rejected (version/capability/token mismatch). The app may still
    /// fall back to the backward-compatible subset (List + Kill) on a legacy
    /// daemon — see the upgrade policy.
    HelloRejected { reason: String },
    Spawned { req_id: u64, session_id: String },
    Sessions { req_id: u64, sessions: Vec<SessionSummary> },
    /// Handoff step 2: the grid is snapshotted at `seq_n`. A `T_REPAINT` frame
    /// (carrying the same `seq_n`) follows immediately, then `T_DATA` frames with
    /// `start_offset >= seq_n`.
    AttachOk { rows: u16, cols: u16, seq_n: u64, alt_screen: bool },
    /// The attached session's process exited.
    Exited { code: Option<i32> },
    /// Attribution: a turn boundary was detected.
    TurnBoundary { turn: TurnInfo },
    /// Ranged history response (reserved; unimplemented in Step 2).
    HistoryLines { session_id: String, lines: Vec<String>, base_seq: u64 },
    Heartbeat,
    Error { message: String },
}

/// One enumerated session (for the detached-agents panel + upgrade UI).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionSummary {
    pub id: String,
    pub cwd: String,
    pub program: String,
    pub alive: bool,
    pub attached: bool,
    pub rows: u16,
    pub cols: u16,
    pub created_at_unix: u64,
    pub attribution_key: Option<String>,
    /// Protocol version the daemon serving this session speaks (upgrade UI).
    pub protocol_version: u16,
}

/// An agent's inferred lifecycle state — CAO's `TerminalStatus`, native to the
/// daemon. Phase 1 defines it (the provider adapters compute it from the grid);
/// Phase 4 adds it to [`SessionSummary`] + a push event and drives the UI badge.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum AgentStatus {
    /// Ready for input; idle prompt visible, no active work.
    Idle,
    /// Actively working (spinner / streaming output).
    Processing,
    /// Blocked on an approval/permission prompt the user must answer.
    WaitingUserAnswer,
    /// Finished a turn with a response visible at the idle prompt.
    Completed,
    /// Error state or unreadable/empty grid.
    Error,
}

/// Why a turn started/ended — the plural boundary signals, ranked by trust.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum Cause {
    /// App told us (pressed Enter / launched agent). Strongest, spoof-proof.
    AppCheckpoint,
    /// OSC 133 semantic prompt (A/B/C/D).
    Osc133,
    /// Output idle after a burst (quiet window).
    QuietWindow,
    /// Correlated with filesystem dirty events.
    FsActivity,
    /// Process group / lifecycle change (coarse).
    ProcessLifecycle,
    /// First turn / session start.
    SessionStart,
}

/// An attribution "turn": a span of output bytes tied to a cause + fs changes.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TurnInfo {
    pub session_id: String,
    pub epoch: u64,
    pub start_offset: u64,
    pub end_offset: u64,
    pub started_cause: Cause,
    pub ended_cause: Cause,
    /// Exit code if this turn ended on an OSC 133 ;D with a status.
    pub command_exit: Option<i32>,
    /// Filesystem paths reported dirty during this turn (correlated by the app).
    pub fs_dirty_paths: Vec<String>,
}

// ---------------------------------------------------------------------------
// Frame encode/decode. `encode_*` returns the payload (type byte + body); the
// length-delimited codec adds the u32 length on the wire. `parse_frame` takes a
// codec-yielded payload (length already stripped) and classifies it.
// ---------------------------------------------------------------------------

/// A decoded frame payload, classified by its type byte.
#[derive(Debug)]
pub enum Frame {
    /// Control payload bytes (postcard); decode with [`decode_client`] /
    /// [`decode_server`] depending on which side you are.
    Control(Bytes),
    /// `(start_offset, raw_bytes)`.
    Data(u64, Bytes),
    /// `(seq_n, repaint_bytes)`.
    Repaint(u64, Bytes),
}

/// Error decoding a frame or control message.
#[derive(Debug)]
pub enum ProtoError {
    Empty,
    UnknownType(u8),
    Truncated,
    Postcard(postcard::Error),
}

impl std::fmt::Display for ProtoError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ProtoError::Empty => write!(f, "empty frame"),
            ProtoError::UnknownType(t) => write!(f, "unknown frame type {t:#x}"),
            ProtoError::Truncated => write!(f, "truncated frame"),
            ProtoError::Postcard(e) => write!(f, "postcard: {e}"),
        }
    }
}
impl std::error::Error for ProtoError {}
impl From<postcard::Error> for ProtoError {
    fn from(e: postcard::Error) -> Self {
        ProtoError::Postcard(e)
    }
}

/// Classify a codec-yielded payload (length prefix already stripped) by its type
/// byte. Cheap: data/repaint slices share the input buffer (no copy).
pub fn parse_frame(mut payload: BytesMut) -> Result<Frame, ProtoError> {
    if payload.is_empty() {
        return Err(ProtoError::Empty);
    }
    let ty = payload[0];
    payload.advance(1);
    match ty {
        T_CONTROL => Ok(Frame::Control(payload.freeze())),
        T_DATA => {
            if payload.len() < 8 {
                return Err(ProtoError::Truncated);
            }
            let offset = payload.get_u64();
            Ok(Frame::Data(offset, payload.freeze()))
        }
        T_REPAINT => {
            if payload.len() < 8 {
                return Err(ProtoError::Truncated);
            }
            let seq = payload.get_u64();
            Ok(Frame::Repaint(seq, payload.freeze()))
        }
        other => Err(ProtoError::UnknownType(other)),
    }
}

fn encode_control<T: Serialize>(msg: &T) -> Result<Bytes, ProtoError> {
    let body = postcard::to_allocvec(msg)?;
    let mut buf = BytesMut::with_capacity(1 + body.len());
    buf.put_u8(T_CONTROL);
    buf.put_slice(&body);
    Ok(buf.freeze())
}

/// Encode an app→daemon control message into a frame payload.
pub fn encode_client(msg: &ClientMsg) -> Result<Bytes, ProtoError> {
    encode_control(msg)
}

/// Encode a daemon→app control message into a frame payload.
pub fn encode_server(msg: &ServerMsg) -> Result<Bytes, ProtoError> {
    encode_control(msg)
}

/// Decode a [`Frame::Control`] payload as a [`ClientMsg`] (daemon side).
pub fn decode_client(body: &[u8]) -> Result<ClientMsg, ProtoError> {
    Ok(postcard::from_bytes(body)?)
}

/// Decode a [`Frame::Control`] payload as a [`ServerMsg`] (app side).
pub fn decode_server(body: &[u8]) -> Result<ServerMsg, ProtoError> {
    Ok(postcard::from_bytes(body)?)
}

/// Encode a live PTY data frame: `[T_DATA][u64 BE offset][raw]`.
pub fn encode_data(offset: u64, chunk: &[u8]) -> Bytes {
    let mut buf = BytesMut::with_capacity(1 + 8 + chunk.len());
    buf.put_u8(T_DATA);
    buf.put_u64(offset);
    buf.put_slice(chunk);
    buf.freeze()
}

/// Encode a repaint frame: `[T_REPAINT][u64 BE seq_n][escape bytes]`.
pub fn encode_repaint(seq_n: u64, bytes: &[u8]) -> Bytes {
    let mut buf = BytesMut::with_capacity(1 + 8 + bytes.len());
    buf.put_u8(T_REPAINT);
    buf.put_u64(seq_n);
    buf.put_slice(bytes);
    buf.freeze()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn data_frame_roundtrips() {
        let payload = encode_data(42, b"hello");
        // Simulate the codec stripping the length prefix: parse the payload bytes.
        let frame = parse_frame(BytesMut::from(&payload[..])).unwrap();
        match frame {
            Frame::Data(off, bytes) => {
                assert_eq!(off, 42);
                assert_eq!(&bytes[..], b"hello");
            }
            other => panic!("expected Data, got {other:?}"),
        }
    }

    #[test]
    fn repaint_frame_roundtrips() {
        let payload = encode_repaint(7, b"\x1b[2J");
        match parse_frame(BytesMut::from(&payload[..])).unwrap() {
            Frame::Repaint(seq, bytes) => {
                assert_eq!(seq, 7);
                assert_eq!(&bytes[..], b"\x1b[2J");
            }
            other => panic!("expected Repaint, got {other:?}"),
        }
    }

    #[test]
    fn client_control_roundtrips() {
        let msg = ClientMsg::Attach { session_id: "pty-1".into(), rows: 40, cols: 120 };
        let payload = encode_client(&msg).unwrap();
        match parse_frame(BytesMut::from(&payload[..])).unwrap() {
            Frame::Control(body) => match decode_client(&body).unwrap() {
                ClientMsg::Attach { session_id, rows, cols } => {
                    assert_eq!((session_id.as_str(), rows, cols), ("pty-1", 40, 120))
                }
                other => panic!("wrong msg {other:?}"),
            },
            other => panic!("expected Control, got {other:?}"),
        }
    }

    #[test]
    fn server_control_roundtrips() {
        let msg = ServerMsg::AttachOk { rows: 24, cols: 80, seq_n: 1000, alt_screen: true };
        let payload = encode_server(&msg).unwrap();
        match parse_frame(BytesMut::from(&payload[..])).unwrap() {
            Frame::Control(body) => match decode_server(&body).unwrap() {
                ServerMsg::AttachOk { rows, cols, seq_n, alt_screen } => {
                    assert_eq!((rows, cols, seq_n, alt_screen), (24, 80, 1000, true));
                }
                other => panic!("wrong msg {other:?}"),
            },
            other => panic!("expected Control, got {other:?}"),
        }
    }

    #[test]
    fn spawn_agent_control_roundtrips() {
        let spec = AgentSpawnSpec {
            provider: "codex".into(),
            profile: AgentProfile {
                name: "default".into(),
                model: Some("gpt-5".into()),
                mcp_servers: vec![McpServerConfig {
                    name: "cao".into(),
                    command: "cao-mcp-server".into(),
                    args: vec!["--stdio".into()],
                    env: vec![("X".into(), "1".into())],
                }],
                ..Default::default()
            },
            cwd: Some("/tmp/wt".into()),
            rows: 40,
            cols: 120,
            attribution_key: Some("term-abc".into()),
            seed_prompt: None,
            env: vec![],
        };
        let msg = ClientMsg::SpawnAgent { req_id: 9, spec };
        let payload = encode_client(&msg).unwrap();
        match parse_frame(BytesMut::from(&payload[..])).unwrap() {
            Frame::Control(body) => match decode_client(&body).unwrap() {
                ClientMsg::SpawnAgent { req_id, spec } => {
                    assert_eq!(req_id, 9);
                    assert_eq!(spec.provider, "codex");
                    assert_eq!(spec.profile.model.as_deref(), Some("gpt-5"));
                    assert_eq!(spec.profile.mcp_servers.len(), 1);
                    assert_eq!(spec.attribution_key.as_deref(), Some("term-abc"));
                }
                other => panic!("wrong msg {other:?}"),
            },
            other => panic!("expected Control, got {other:?}"),
        }
    }

    #[test]
    fn empty_and_unknown_frames_error() {
        assert!(matches!(parse_frame(BytesMut::new()), Err(ProtoError::Empty)));
        let mut bad = BytesMut::new();
        bad.put_u8(0xff);
        assert!(matches!(parse_frame(bad), Err(ProtoError::UnknownType(0xff))));
    }
}
