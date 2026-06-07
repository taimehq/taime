//! Small shared helpers for the per-provider status/approval heuristics.
//!
//! The [`GridView`](super::GridView) is already de-ANSI'd by the emulator, so the
//! heuristics mostly run on clean text. [`strip_ansi`] stays available for the
//! Phase-5 *log-tail* idle check, which reads the raw `pipe-pane`-equivalent
//! stream rather than the emulator grid.

// `strip_ansi` feeds the Phase-5 log-tail idle check (raw stream, not the grid).
#![allow(dead_code)]

/// Remove CSI/OSC/escape sequences from a raw terminal string. Conservative: it
/// drops `ESC [ … <final>`, `ESC ] … (BEL|ST)`, and bare two-char escapes — the
/// shapes CAO's `ANSI_CODE_PATTERN` family targets.
pub fn strip_ansi(input: &str) -> String {
    let bytes = input.as_bytes();
    let mut out = String::with_capacity(input.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == 0x1b {
            // ESC
            if i + 1 < bytes.len() && bytes[i + 1] == b'[' {
                // CSI: ESC [ params... final-byte (0x40..=0x7e)
                i += 2;
                while i < bytes.len() && !(0x40..=0x7e).contains(&bytes[i]) {
                    i += 1;
                }
                i += 1; // consume final byte
                continue;
            } else if i + 1 < bytes.len() && bytes[i + 1] == b']' {
                // OSC: ESC ] ... (BEL | ESC \)
                i += 2;
                while i < bytes.len() {
                    if bytes[i] == 0x07 {
                        i += 1;
                        break;
                    }
                    if bytes[i] == 0x1b && i + 1 < bytes.len() && bytes[i + 1] == b'\\' {
                        i += 2;
                        break;
                    }
                    i += 1;
                }
                continue;
            } else {
                // Bare 2-char escape: skip ESC + next.
                i += 2;
                continue;
            }
        }
        // Safe because we only advance by whole UTF-8 chars below.
        let ch_len = utf8_len(bytes[i]);
        if let Ok(s) = std::str::from_utf8(&bytes[i..(i + ch_len).min(bytes.len())]) {
            out.push_str(s);
        }
        i += ch_len;
    }
    out
}

fn utf8_len(b: u8) -> usize {
    match b {
        0x00..=0x7f => 1,
        0xc0..=0xdf => 2,
        0xe0..=0xef => 3,
        _ => 4,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_csi_color_codes() {
        assert_eq!(strip_ansi("\x1b[31mred\x1b[0m"), "red");
    }

    #[test]
    fn strips_osc_with_bel() {
        assert_eq!(strip_ansi("a\x1b]133;D;0\x07b"), "ab");
    }

    #[test]
    fn keeps_unicode() {
        assert_eq!(strip_ansi("❯ ✦ ⏺"), "❯ ✦ ⏺");
    }
}
