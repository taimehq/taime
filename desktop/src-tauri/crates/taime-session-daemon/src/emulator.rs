//! The authoritative headless emulator: one `wezterm_term::Terminal` per
//! session, fed the same PTY byte stream the client renders. It is the source of
//! truth for the grid (used for the exact-visible-screen repaint and for
//! per-turn attribution snapshots). xterm in the webview remains the *display*.

use std::sync::Arc;

use wezterm_term::color::ColorPalette;
use wezterm_term::{Line, Terminal, TerminalConfiguration, TerminalSize};

/// Lines of structured scrollback retained on the primary screen.
const SCROLLBACK: usize = 10_000;

/// Minimal headless config. `color_palette()` is the only required method; we
/// override `scrollback_size()` to bound history/memory.
#[derive(Debug)]
struct HeadlessConfig;

impl TerminalConfiguration for HeadlessConfig {
    fn color_palette(&self) -> ColorPalette {
        ColorPalette::default()
    }
    fn scrollback_size(&self) -> usize {
        SCROLLBACK
    }
}

/// The emulator's writer sink. `wezterm-term` writes auto-replies (DA/DSR query
/// responses) here. We do NOT forward them to the PTY: the real terminal (the
/// agent's actual controlling environment) is xterm, which answers queries
/// itself. Feeding wezterm's replies back to the PTY master would inject phantom
/// bytes into the agent's stdin. So the sink is a no-op — wezterm is a passive
/// recorder, not the interactive terminal.
struct NullWriter;
impl std::io::Write for NullWriter {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// Build a headless terminal of the given size.
pub fn build_terminal(rows: u16, cols: u16) -> Terminal {
    let size = TerminalSize {
        rows: rows as usize,
        cols: cols as usize,
        pixel_width: 0,
        pixel_height: 0,
        dpi: 0,
    };
    let config: Arc<dyn TerminalConfiguration + Send + Sync> = Arc::new(HeadlessConfig);
    Terminal::new(size, config, "taime", env!("CARGO_PKG_VERSION"), Box::new(NullWriter))
}

/// Resize the emulator grid (call alongside `pty.resize()` so the authoritative
/// grid tracks the client's viewport).
pub fn resize(term: &mut Terminal, rows: u16, cols: u16) {
    term.resize(TerminalSize {
        rows: rows as usize,
        cols: cols as usize,
        pixel_width: 0,
        pixel_height: 0,
        dpi: 0,
    });
}

/// The visible screen's lines (owned clones, no scrollback). `Screen`'s own
/// `visible_lines()` is `#[cfg(test)]`-gated, so we go through the public
/// `phys_range` → `lines_in_phys_range` path: visible rows are `0..physical_rows`.
pub fn visible_lines(term: &Terminal) -> Vec<Line> {
    let screen = term.screen();
    let rows = screen.physical_rows as i64;
    let phys = screen.phys_range(&(0..rows));
    screen.lines_in_phys_range(phys)
}

/// Snapshot the visible screen as plain text (one String per row, trailing
/// blanks trimmed) — the per-turn attribution artifact. Cheap relative to a
/// full cell-attribute clone; we only call it at turn boundaries. (Substrate for
/// turn-level grid snapshots; not yet wired into the wire `TurnInfo`.)
#[allow(dead_code)]
pub fn snapshot_visible_text(term: &Terminal) -> Vec<String> {
    visible_lines(term)
        .iter()
        .map(|line| line.as_str().trim_end().to_string())
        .collect()
}
