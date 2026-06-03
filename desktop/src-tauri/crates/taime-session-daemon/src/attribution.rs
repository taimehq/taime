//! Attribution: tie terminal output to **turns** — the flagship substrate. A
//! turn is a span of output bytes `[start_offset, end_offset)` with a cause for
//! its start and end, an optional command exit code, and (filled app-side) the
//! filesystem paths it dirtied.
//!
//! Boundary signals are **plural**, ranked by trust (the plan): app checkpoints
//! (strongest, spoof-proof — the app knows when it pressed Enter / launched an
//! agent), then OSC 133 semantic prompts, then output quiet-windows, then
//! fs-event correlation (done app-side), then process lifecycle.
//!
//! We run a **parallel `termwiz` escape parser tap** over the same byte stream
//! the emulator sees, because `wezterm-term` parses then *discards* the OSC 133
//! `;D` exit code — the tap is how we recover it (plus A/B/C transitions and the
//! reported cwd).

use taime_protocol::{Cause, TurnInfo};
use termwiz::escape::osc::{FinalTermSemanticPrompt, OperatingSystemCommand};
use termwiz::escape::parser::Parser;
use termwiz::escape::Action;

pub struct Attribution {
    session_id: String,
    parser: Parser,
    epoch: u64,
    /// Current (open) turn.
    turn_start: u64,
    started_cause: Cause,
    /// Bytes of real output seen in the current turn (gates quiet-window close).
    saw_output: bool,
    /// Last cwd reported via OSC 7 (correlates a turn to a directory).
    cwd: Option<String>,
}

impl Attribution {
    pub fn new(session_id: String) -> Self {
        Attribution {
            session_id,
            parser: Parser::new(),
            epoch: 0,
            turn_start: 0,
            started_cause: Cause::SessionStart,
            saw_output: false,
            cwd: None,
        }
    }

    /// Most recently reported working directory (OSC 7), if any. (Substrate for
    /// directory-aware attribution; not yet surfaced on the wire.)
    #[allow(dead_code)]
    pub fn cwd(&self) -> Option<&str> {
        self.cwd.as_deref()
    }

    fn close_turn(&mut self, end_offset: u64, ended_cause: Cause, exit: Option<i32>) -> TurnInfo {
        let turn = TurnInfo {
            session_id: self.session_id.clone(),
            epoch: self.epoch,
            start_offset: self.turn_start,
            end_offset,
            started_cause: self.started_cause,
            ended_cause,
            command_exit: exit,
            fs_dirty_paths: Vec::new(), // filled app-side by fs-event correlation
        };
        // Open the next turn.
        self.epoch += 1;
        self.turn_start = end_offset;
        self.started_cause = ended_cause;
        self.saw_output = false;
        turn
    }

    /// Feed a chunk and the absolute offset *after* it. Returns any turns that
    /// closed inside the chunk (OSC 133 command-finished boundaries).
    pub fn feed(&mut self, chunk: &[u8], end_offset: u64) -> Vec<TurnInfo> {
        if !chunk.is_empty() {
            self.saw_output = true;
        }
        let mut events: Vec<AttrEvent> = Vec::new();
        self.parser.parse(chunk, |action| {
            if let Action::OperatingSystemCommand(osc) = action {
                match *osc {
                    // OSC 133 ; D — command finished (carries the exit code that
                    // wezterm-term itself parses-then-discards). A/B/C transitions
                    // are implied by the start cause when we need them.
                    OperatingSystemCommand::FinalTermSemanticPrompt(
                        FinalTermSemanticPrompt::CommandStatus { status, .. },
                    ) => events.push(AttrEvent::CommandFinished(status)),
                    OperatingSystemCommand::CurrentWorkingDirectory(dir) => {
                        events.push(AttrEvent::Cwd(dir))
                    }
                    _ => {}
                }
            }
        });
        // Coalesce: we don't have per-marker byte offsets within the chunk, so
        // multiple OSC 133 ;D in one read all resolve at the chunk's end. Emit a
        // SINGLE boundary (last exit code wins) rather than several turns sharing
        // an end_offset — the latter would emit a degenerate zero-length turn.
        let mut finished = false;
        let mut exit_code: Option<i32> = None;
        for ev in events {
            match ev {
                AttrEvent::CommandFinished(status) => {
                    finished = true;
                    exit_code = Some(status);
                }
                AttrEvent::Cwd(dir) => self.cwd = Some(dir),
            }
        }
        let mut closed = Vec::new();
        // Guard against a zero-length turn (mirrors checkpoint/quiet).
        if finished && (end_offset > self.turn_start || self.saw_output) {
            closed.push(self.close_turn(end_offset, Cause::Osc133, exit_code));
        }
        closed
    }

    /// App-driven checkpoint (strongest signal). Closes the current turn.
    pub fn checkpoint(&mut self, end_offset: u64) -> Option<TurnInfo> {
        if end_offset <= self.turn_start && !self.saw_output {
            // Nothing happened since the last boundary — don't emit an empty turn,
            // just re-attribute the start cause.
            self.started_cause = Cause::AppCheckpoint;
            return None;
        }
        Some(self.close_turn(end_offset, Cause::AppCheckpoint, None))
    }

    /// Output went quiet after a burst. Closes the current turn iff it has output.
    pub fn quiet(&mut self, end_offset: u64) -> Option<TurnInfo> {
        if !self.saw_output || end_offset <= self.turn_start {
            return None;
        }
        Some(self.close_turn(end_offset, Cause::QuietWindow, None))
    }
}

enum AttrEvent {
    CommandFinished(i32),
    Cwd(String),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn osc133_command_status_closes_a_turn_with_exit_code() {
        let mut attr = Attribution::new("s1".into());
        // Some output, then OSC 133 ; D ; 0 (command finished, exit 0).
        let turns = attr.feed(b"build output\x1b]133;D;0\x07", 20);
        assert_eq!(turns.len(), 1, "a command-finished boundary should close a turn");
        let t = &turns[0];
        assert_eq!(t.ended_cause, Cause::Osc133);
        assert_eq!(t.command_exit, Some(0));
        assert_eq!(t.start_offset, 0);
        assert_eq!(t.end_offset, 20);
    }

    #[test]
    fn coalesced_double_command_status_emits_one_turn() {
        // Two OSC 133 ;D markers in a single read chunk must coalesce into ONE
        // turn (last exit wins), not emit a degenerate zero-length second turn.
        let mut attr = Attribution::new("s1".into());
        let turns = attr.feed(b"a\x1b]133;D;0\x07b\x1b]133;D;1\x07", 30);
        assert_eq!(turns.len(), 1, "two ;D in one chunk -> one turn");
        assert_eq!(turns[0].command_exit, Some(1), "last exit code wins");
    }

    #[test]
    fn osc7_reports_cwd() {
        let mut attr = Attribution::new("s1".into());
        attr.feed(b"\x1b]7;file:///Users/me/proj\x07", 10);
        assert_eq!(attr.cwd(), Some("file:///Users/me/proj"));
    }

    #[test]
    fn app_checkpoint_closes_turn_when_output_present() {
        let mut attr = Attribution::new("s1".into());
        attr.feed(b"some output", 11);
        let t = attr.checkpoint(11).expect("checkpoint should close a turn");
        assert_eq!(t.ended_cause, Cause::AppCheckpoint);
        assert_eq!(t.start_offset, 0);
        assert_eq!(t.end_offset, 11);
    }

    #[test]
    fn checkpoint_without_output_does_not_emit_empty_turn() {
        let mut attr = Attribution::new("s1".into());
        assert!(attr.checkpoint(0).is_none());
    }

    #[test]
    fn quiet_window_closes_only_when_output_present() {
        let mut attr = Attribution::new("s1".into());
        assert!(attr.quiet(0).is_none(), "no output -> no quiet boundary");
        attr.feed(b"hi", 2);
        let t = attr.quiet(2).expect("output then quiet -> close");
        assert_eq!(t.ended_cause, Cause::QuietWindow);
    }
}
