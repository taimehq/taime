//! Visible-screen repaint: serialize `wezterm-term`'s authoritative grid into a
//! minimal full-screen repaint (escape sequences) for **exact-visible-screen
//! reattach**. Feeding this to xterm (after the client's reset prelude) makes the
//! display (xterm) and the record (wezterm-term) converge by construction at each
//! handoff — the plan's stated goal.
//!
//! We hand-roll SGR emission from the verified cell model rather than going
//! through termwiz's `TerminfoRenderer` (which needs a `Capabilities` + a
//! `RenderTty` wrapper): emitting standard SGR (256-color + truecolor) lets xterm
//! resolve colors with its own palette, which matches the daemon's fixed
//! `xterm-256color` `TERM`, and keeps the code to verified accessors.
//!
//! Scope (per the plan): this restores the **visible viewport** exactly. It does
//! not restore scrollback history, selection, search, or the main-screen contents
//! hidden behind an alt-screen — those are deferred (Step 3 / `GetHistory`).

use termwiz::surface::CursorVisibility;
use wezterm_term::color::ColorAttribute;
use wezterm_term::{Intensity, Terminal, Underline};

use crate::emulator;

/// Serialize the current visible screen to a self-contained repaint. The client
/// writes its reset prelude first (virtual reset of a reused xterm), then these
/// bytes, then resumes the live byte stream at `> seq_n`.
pub fn serialize(term: &Terminal) -> Vec<u8> {
    let mut out: Vec<u8> = Vec::with_capacity(4096);

    // Self-sufficient reset prelude (review L2/L3): the daemon OWNS the repaint
    // invariant — it must not depend on a client-emitted reset (the frontend
    // emits none; reattach works today only because it mounts a FRESH xterm).
    // Force a known baseline so reattach converges from ANY xterm state and
    // survives a future scrollback-preserving optimization that reuses an xterm.
    // (DECSTR already restores most of these; the explicit modes are belt-and-
    // suspenders and no-ops on a fresh terminal, so the common case is unchanged.)
    out.extend_from_slice(b"\x1b[?1049l"); // exit alt screen → main
    out.extend_from_slice(b"\x1b[!p"); // DECSTR soft reset
    out.extend_from_slice(b"\x1b[?7h"); // autowrap on (default)
    out.extend_from_slice(b"\x1b[?1l"); // DECCKM off (normal cursor keys)
    out.extend_from_slice(b"\x1b[r"); // reset scroll region to full screen
    out.extend_from_slice(b"\x1b(B"); // G0 = US-ASCII charset

    // If the grid is on the alternate screen (TUI apps like claude), re-enter it
    // so a later alt-screen *exit* by the agent correctly restores xterm's main
    // screen. The prelude reset us to the main screen; this sets the real target.
    let alt = term.is_alt_screen_active();
    if alt {
        out.extend_from_slice(b"\x1b[?1049h");
    }
    // Reset attributes, clear, home.
    out.extend_from_slice(b"\x1b[0m\x1b[2J\x1b[H");

    // Re-assert DEC private modes the agent enabled before the snapshot: the
    // client's reset prelude turned them off, and the live stream resumed at
    // > seq_n only carries FUTURE mode changes. Bracketed paste is explicitly in
    // the plan's acceptance set, so a large multi-line paste after reattach is
    // delivered as a paste, not auto-run input. (Mouse tracking is also lost on
    // reattach, but wezterm-term only exposes is_mouse_grabbed() — a boolean —
    // not the exact ?1000/?1002/?1003 tracking mode, so we leave mouse for the
    // agent to re-assert rather than restore the wrong mode.)
    if term.bracketed_paste_enabled() {
        out.extend_from_slice(b"\x1b[?2004h");
    }

    let rows = term.screen().physical_rows;
    let lines = emulator::visible_lines(term);

    let mut last_sgr: Option<String> = None;
    for (row, line) in lines.iter().enumerate().take(rows) {
        // Absolute cursor move to the start of this row (1-based).
        out.extend_from_slice(format!("\x1b[{};1H", row + 1).as_bytes());
        for cell in line.visible_cells() {
            let sgr = sgr_params(cell.attrs());
            if last_sgr.as_deref() != Some(sgr.as_str()) {
                // Reset then apply, so we never inherit a stale attribute.
                out.extend_from_slice(b"\x1b[0");
                if !sgr.is_empty() {
                    out.push(b';');
                    out.extend_from_slice(sgr.as_bytes());
                }
                out.push(b'm');
                last_sgr = Some(sgr);
            }
            out.extend_from_slice(cell.str().as_bytes());
        }
    }

    // Restore cursor position (CursorPosition is 0-based, relative to the visible
    // top-left), shape/visibility-relevant SGR reset, and visibility.
    out.extend_from_slice(b"\x1b[0m");
    let cur = term.cursor_pos();
    let cy = if cur.y < 0 { 0 } else { cur.y as usize };
    out.extend_from_slice(format!("\x1b[{};{}H", cy + 1, cur.x + 1).as_bytes());
    match cur.visibility {
        CursorVisibility::Hidden => out.extend_from_slice(b"\x1b[?25l"),
        CursorVisibility::Visible => out.extend_from_slice(b"\x1b[?25h"),
    }

    out
}

/// Build the SGR parameter list (without the leading `0;` reset or trailing `m`)
/// for a cell's attributes. Empty means "all default".
fn sgr_params(a: &wezterm_term::CellAttributes) -> String {
    let mut codes: Vec<String> = Vec::new();
    match a.intensity() {
        Intensity::Bold => codes.push("1".into()),
        Intensity::Half => codes.push("2".into()),
        Intensity::Normal => {}
    }
    if a.italic() {
        codes.push("3".into());
    }
    if a.underline() != Underline::None {
        // Map every underline style to single-underline (4); the distinct curly/
        // dotted/dashed styles are cosmetic and not universally supported.
        codes.push("4".into());
    }
    if a.reverse() {
        codes.push("7".into());
    }
    if a.invisible() {
        codes.push("8".into());
    }
    if a.strikethrough() {
        codes.push("9".into());
    }
    push_color(&mut codes, a.foreground(), true);
    push_color(&mut codes, a.background(), false);
    codes.join(";")
}

/// Emit the SGR for a foreground/background color. Palette indices use the
/// 256-color form (`38;5;n` / `48;5;n`) for any index, truecolor uses `38;2;r;g;b`.
fn push_color(codes: &mut Vec<String>, color: ColorAttribute, fg: bool) {
    let (idx_lead, rgb_lead) = if fg { ("38;5;", "38;2;") } else { ("48;5;", "48;2;") };
    match color {
        ColorAttribute::Default => {}
        ColorAttribute::PaletteIndex(i) => codes.push(format!("{idx_lead}{i}")),
        ColorAttribute::TrueColorWithDefaultFallback(t)
        | ColorAttribute::TrueColorWithPaletteFallback(t, _) => {
            let (r, g, b, _a) = t.to_srgb_u8();
            codes.push(format!("{rgb_lead}{r};{g};{b}"));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn repaint_string(input: &[u8], rows: u16, cols: u16) -> String {
        let mut term = emulator::build_terminal(rows, cols);
        term.advance_bytes(input);
        String::from_utf8(serialize(&term)).unwrap()
    }

    #[test]
    fn repaint_contains_visible_text_and_resets() {
        let s = repaint_string(b"hello world", 24, 80);
        assert!(s.contains("hello world"), "repaint should carry the text: {s:?}");
        assert!(s.contains("\x1b[2J"), "repaint should clear the screen");
        // Self-sufficient reset prelude leads (review L2/L3): exit alt + DECSTR.
        assert!(s.starts_with("\x1b[?1049l\x1b[!p"), "reset prelude should lead: {s:?}");
        assert!(s.contains("\x1b[0m\x1b[2J\x1b[H"), "should still reset+clear+home");
    }

    #[test]
    fn repaint_reenters_alt_screen_when_active() {
        // Enter alt screen, draw — repaint must re-enter alt so xterm matches.
        let s = repaint_string(b"\x1b[?1049hTUI", 24, 80);
        // The prelude exits alt first (\x1b[?1049l) for a known baseline, THEN
        // re-enters it because the grid is alt-active.
        assert!(s.contains("\x1b[?1049h"), "must re-enter alt screen: {s:?}");
        let exit = s.find("\x1b[?1049l").unwrap();
        let reenter = s.find("\x1b[?1049h").unwrap();
        assert!(exit < reenter, "exit-alt baseline precedes re-enter: {s:?}");
        assert!(s.contains("TUI"));
    }

    #[test]
    fn repaint_emits_sgr_for_colored_text() {
        // Red bold text — repaint should carry an SGR with bold (1).
        let s = repaint_string(b"\x1b[1;31mRED\x1b[0m", 24, 80);
        assert!(s.contains("RED"));
        // bold code present somewhere before RED
        let red_pos = s.find("RED").unwrap();
        assert!(s[..red_pos].contains("1"), "bold SGR should precede RED: {s:?}");
    }

    #[test]
    fn repaint_hides_cursor_when_hidden() {
        let s = repaint_string(b"\x1b[?25lhi", 24, 80);
        assert!(s.contains("\x1b[?25l"), "hidden cursor should be reflected: {s:?}");
    }

    #[test]
    fn repaint_restores_bracketed_paste_mode() {
        // An agent that enabled bracketed paste must have it restored on reattach
        // (else a post-reattach multi-line paste auto-runs).
        let s = repaint_string(b"\x1b[?2004hready", 24, 80);
        assert!(s.contains("\x1b[?2004h"), "bracketed paste should be restored: {s:?}");
    }
}
