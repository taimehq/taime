import { useEffect, useRef } from "react";
import { Terminal } from "@xterm/xterm";
import { FitAddon } from "@xterm/addon-fit";
import { WebglAddon } from "@xterm/addon-webgl";
import "@xterm/xterm/css/xterm.css";
import {
  ptyWrite,
  ptyResize,
  ptyReattachView,
  ptyCloseView,
  onPtyData,
  onPtyExit,
} from "../pty";
import { wireClipboard } from "../lib/terminalClipboard";
import { registerTerminalInput } from "../lib/terminalInput";

interface Props {
  sessionId: string;
  onConnectionChange?: (state: "open" | "closed") => void;
}

const THEME = {
  background: "#0b0e13",
  foreground: "#dbe1ea",
  cursor: "#43c6b8",
  cursorAccent: "#0b0e13",
  selectionBackground: "#27405c",
  black: "#0b0e13",
  red: "#ff7b72",
  green: "#43c6b8",
  yellow: "#e0a458",
  blue: "#6ea8fe",
  magenta: "#bc8cff",
  cyan: "#39d3c2",
  white: "#dbe1ea",
  brightBlack: "#56606f",
};

/**
 * Terminal view for the Rust-owned PTY transport (Claude path). Same xterm UX
 * as the CAO `TerminalView`; only the transport differs — Tauri events in,
 * `pty_write`/`pty_resize` out.
 *
 * Mount protocol (no gap, no duplicate): subscribe FIRST while the session is
 * detached, THEN reattach — which atomically attaches + returns the scrollback
 * to replay; live output flows after. Unmount = `close_view` (the agent keeps
 * running); explicit kill is separate.
 */
export function TerminalViewRustPty({ sessionId, onConnectionChange }: Props) {
  const containerRef = useRef<HTMLDivElement>(null);

  useEffect(() => {
    const el = containerRef.current;
    if (!el) return;

    let alive = true;
    let unlistenData: (() => void) | undefined;
    let unlistenExit: (() => void) | undefined;
    let resizeObserver: ResizeObserver | null = null;
    let resizeTimer: ReturnType<typeof setTimeout> | undefined;
    let rafId = 0;

    const term = new Terminal({
      cursorBlink: true,
      fontSize: 13,
      fontFamily:
        "ui-monospace, 'JetBrains Mono', SFMono-Regular, Menlo, Monaco, monospace",
      scrollback: 10000,
      allowProposedApi: true,
      macOptionClickForcesSelection: true,
      rightClickSelectsWord: true,
      theme: THEME,
    });
    const fitAddon = new FitAddon();
    term.loadAddon(fitAddon);
    term.open(el);
    try {
      const webgl = new WebglAddon();
      webgl.onContextLoss(() => webgl.dispose());
      term.loadAddon(webgl);
    } catch {
      /* canvas/dom fallback */
    }

    const safeFit = () => {
      try {
        fitAddon.fit();
      } catch {
        /* not measurable yet */
      }
    };

    // Cross-platform copy/paste (shared across transports).
    const cleanupClipboard = wireClipboard(term, el);

    // Register an input writer so dropped file/screenshot paths can be typed in.
    const unregisterInput = registerTerminalInput(sessionId, (text) => {
      ptyWrite(sessionId, text);
    });

    term.onData((data) => {
      ptyWrite(sessionId, data);
    });

    resizeObserver = new ResizeObserver(() => {
      clearTimeout(resizeTimer);
      resizeTimer = setTimeout(() => {
        safeFit();
        ptyResize(sessionId, term.rows, term.cols);
      }, 50);
    });
    resizeObserver.observe(el);
    rafId = requestAnimationFrame(safeFit);
    term.focus();

    (async () => {
      // Subscribe first (session is detached → no events yet, so no dup).
      unlistenData = await onPtyData(sessionId, (bytes) => {
        if (alive) term.write(bytes);
      });
      unlistenExit = await onPtyExit(sessionId, () => {
        if (alive) term.write("\r\n\x1b[33m[process exited]\x1b[0m\r\n");
        onConnectionChange?.("closed");
      });
      // Reattach: atomically attaches + returns scrollback to replay.
      const replay = await ptyReattachView(sessionId);
      if (!alive) return;
      if (replay.length) term.write(replay);
      onConnectionChange?.("open");
      // Nudge a redraw so a reattached TUI repaints cleanly at the current size.
      safeFit();
      ptyResize(sessionId, term.rows, term.cols);
    })();

    return () => {
      alive = false;
      cancelAnimationFrame(rafId);
      clearTimeout(resizeTimer);
      resizeObserver?.disconnect();
      cleanupClipboard();
      unregisterInput();
      unlistenData?.();
      unlistenExit?.();
      // Closing the view detaches — it does NOT kill the agent.
      ptyCloseView(sessionId);
      term.dispose();
    };
  }, [sessionId, onConnectionChange]);

  return (
    <div className="relative h-full w-full overflow-hidden bg-ink-900">
      <div ref={containerRef} className="absolute inset-1.5" />
    </div>
  );
}
