import { useEffect, useRef } from "react";
import { Terminal } from "@xterm/xterm";
import { FitAddon } from "@xterm/addon-fit";
import { WebglAddon } from "@xterm/addon-webgl";
import { SearchAddon } from "@xterm/addon-search";
import type { Channel } from "@tauri-apps/api/core";
import "@xterm/xterm/css/xterm.css";
import { transportFor, type PtyBackend } from "../pty";
import { wireClipboard } from "../lib/terminalClipboard";
import { registerTerminalInput } from "../lib/terminalInput";
import { makeModelSniffer } from "../lib/parseModel";
import { useStore } from "../store";
import { useTerminalFind } from "../hooks/useTerminalFind";
import { TerminalFindBar } from "./TerminalFindBar";

interface Props {
  sessionId: string;
  /** Frame id — used to attribute the parsed model back to this frame. */
  frameKey: string;
  /** Which backend owns the PTY: the in-app manager or the detached daemon. */
  backend?: PtyBackend;
  onConnectionChange?: (state: "open" | "closed") => void;
}

const THEME = {
  background: "#0a0a0a",
  foreground: "#ededed",
  cursor: "#c8c7c2",
  cursorAccent: "#0a0a0a",
  selectionBackground: "#33363b",
  black: "#0a0a0a",
  red: "#f85149",
  green: "#3fb950",
  yellow: "#d29922",
  blue: "#4493f8",
  magenta: "#bc8cff",
  cyan: "#39d3c2",
  white: "#ededed",
  brightBlack: "#6f6f6f",
};

/**
 * Terminal view for the Rust-owned PTY transport (Claude path). Same xterm UX
 * as the CAO `TerminalView`; only the transport differs — raw bytes in over a
 * binary `Channel`, `pty_write`/`pty_resize` out.
 *
 * Mount protocol (no gap, no duplicate): `pty_attach` atomically registers the
 * channel sink + replays the (boundary-bounded) scrollback as the first message
 * under the same lock the reader emits under; live output flows after. The
 * channel ref is held for the view's lifetime (GC of it would silently stop
 * output). Each processed chunk advances a byte counter that is acked back
 * (batched per frame) for backpressure. Unmount = `close_view` (the agent keeps
 * running); explicit kill is separate.
 */
export function TerminalViewRustPty({
  sessionId,
  frameKey,
  backend = "inapp",
  onConnectionChange,
}: Props) {
  const containerRef = useRef<HTMLDivElement>(null);
  // Held so the font-zoom effect can mutate the live terminal without tearing
  // it down. `applyResize` re-reports rows/cols after a size change.
  const termRef = useRef<Terminal | null>(null);
  const fitRef = useRef<FitAddon | null>(null);
  const applyResizeRef = useRef<() => void>(() => {});
  // Held so the channel's onmessage isn't GC'd while the view is mounted.
  const channelRef = useRef<Channel<unknown> | null>(null);
  const fontSize = useStore((s) => s.terminalFontSize);
  const {
    searchRef,
    queryRef,
    open: findOpen,
    setOpen: setFindOpen,
    results,
    setResults,
  } = useTerminalFind(frameKey);

  useEffect(() => {
    const el = containerRef.current;
    if (!el) return;

    // Bind to the in-app PTY manager or the detached daemon, transparently.
    const tx = transportFor(backend);

    let alive = true;
    let resizeObserver: ResizeObserver | null = null;
    let resizeTimer: ReturnType<typeof setTimeout> | undefined;
    let rafId = 0;
    // Backpressure (Step 0b): track bytes xterm has *processed* (write-callback)
    // and ack the high-water offset back to Rust, batched once per frame.
    let processedBytes = 0;
    let ackedBytes = 0;
    let ackScheduled = false;
    const scheduleAck = () => {
      if (ackScheduled || !alive) return;
      ackScheduled = true;
      requestAnimationFrame(() => {
        ackScheduled = false;
        if (!alive || processedBytes === ackedBytes) return;
        ackedBytes = processedBytes;
        tx.ack(sessionId, ackedBytes);
      });
    };

    const term = new Terminal({
      cursorBlink: true,
      // Initial size only — read non-reactively; live changes via the zoom
      // effect below so this mount effect isn't keyed on font size.
      fontSize: useStore.getState().terminalFontSize,
      fontFamily:
        "ui-monospace, 'JetBrains Mono', SFMono-Regular, Menlo, Monaco, monospace",
      scrollback: 10000,
      allowProposedApi: true,
      macOptionClickForcesSelection: true,
      // Right-click is owned by wireClipboard (copy selection, else paste) — do
      // NOT let xterm grab a word on right-click, or it would clobber the
      // selection and turn every right-click into a copy instead of a paste.
      rightClickSelectsWord: false,
      theme: THEME,
    });
    const fitAddon = new FitAddon();
    term.loadAddon(fitAddon);
    term.open(el);
    termRef.current = term;
    fitRef.current = fitAddon;
    applyResizeRef.current = () => tx.resize(sessionId, term.rows, term.cols);
    try {
      const webgl = new WebglAddon();
      webgl.onContextLoss(() => webgl.dispose());
      term.loadAddon(webgl);
    } catch {
      /* canvas/dom fallback */
    }

    // Find-in-terminal (Cmd+F). Decorations drive the match-count readout.
    const search = new SearchAddon();
    term.loadAddon(search);
    searchRef.current = search;
    const offResults = search.onDidChangeResults((r) =>
      setResults({ index: r.resultIndex, count: r.resultCount }),
    );

    const safeFit = () => {
      try {
        fitAddon.fit();
      } catch {
        /* not measurable yet */
      }
    };

    // Cross-platform copy/paste (shared across transports).
    const cleanupClipboard = wireClipboard(term, el);

    // Sniff the running model from the agent's startup banner (best-effort).
    const sniffModel = makeModelSniffer((m) =>
      useStore.getState().setFrameModel(frameKey, m),
    );

    // Register an input writer so dropped file/screenshot paths can be typed in.
    const unregisterInput = registerTerminalInput(sessionId, (text) => {
      tx.write(sessionId, text);
    });

    term.onData((data) => {
      tx.write(sessionId, data);
      // Strongest attribution signal: the user submitted a command (Enter). The
      // daemon coalesces/guards empty turns, so spurious Enters are harmless.
      if (data.includes("\r")) tx.checkpoint?.(sessionId, "submit");
    });

    resizeObserver = new ResizeObserver(() => {
      clearTimeout(resizeTimer);
      resizeTimer = setTimeout(() => {
        safeFit();
        tx.resize(sessionId, term.rows, term.cols);
      }, 50);
    });
    resizeObserver.observe(el);
    rafId = requestAnimationFrame(safeFit);
    term.focus();

    (async () => {
      // Resize FIRST (handoff step 1): let one layout frame settle, fit xterm to
      // the container, then attach with the REAL viewport size so the daemon's
      // grid repaint matches it (not the spawn-time 24x80).
      await new Promise<void>((resolve) => requestAnimationFrame(() => resolve()));
      if (!alive) return;
      safeFit();
      // Attach: registers the channel sink + replays scrollback (in-app) or sends
      // the grid repaint at the attach size (daemon), then streams live output.
      const ch = await tx.attach(
        sessionId,
        term.rows,
        term.cols,
        (bytes) => {
          if (!alive) return;
          // Ack on the write-callback (chunk parsed/processed by xterm), which is
          // the true in-flight measure — not "delivered to the channel".
          term.write(bytes, () => {
            processedBytes += bytes.length;
            scheduleAck();
          });
          sniffModel(bytes);
        },
        () => {
          if (alive) term.write("\r\n\x1b[33m[process exited]\x1b[0m\r\n");
          // Reflect lifecycle: the agent's process is gone (running → exited).
          useStore.getState().markRustPtyExited(sessionId);
          onConnectionChange?.("closed");
        },
        // Attribution turn boundaries (daemon transport only) → store.
        (turn) => useStore.getState().recordTurn(frameKey, turn),
      );
      if (!alive) {
        // Unmounted while the attach was in flight: detach so we don't leave a
        // phantom attachment with no acker (which would stall the agent at the
        // backpressure watermark).
        tx.closeView(sessionId);
        return;
      }
      channelRef.current = ch as Channel<unknown> | null;
      onConnectionChange?.("open");
      // Nudge a redraw so a reattached TUI repaints cleanly at the current size.
      safeFit();
      tx.resize(sessionId, term.rows, term.cols);
    })();

    return () => {
      alive = false;
      cancelAnimationFrame(rafId);
      clearTimeout(resizeTimer);
      resizeObserver?.disconnect();
      cleanupClipboard();
      unregisterInput();
      // Drop the channel ref (its onmessage stops); detach keeps the agent alive.
      channelRef.current = null;
      // Closing the view detaches — it does NOT kill the agent.
      tx.closeView(sessionId);
      offResults.dispose();
      searchRef.current = null;
      termRef.current = null;
      fitRef.current = null;
      term.dispose();
    };
  }, [sessionId, frameKey, backend, onConnectionChange]);

  // Apply font-zoom (Cmd ±/0) to the live terminal without recreating it.
  useEffect(() => {
    const term = termRef.current;
    if (!term || term.options.fontSize === fontSize) return;
    term.options.fontSize = fontSize;
    try {
      fitRef.current?.fit();
    } catch {
      /* not measurable yet */
    }
    term.refresh(0, term.rows - 1);
    applyResizeRef.current();
  }, [fontSize]);

  return (
    <div className="relative h-full w-full overflow-hidden bg-ink-900">
      <div ref={containerRef} className="absolute inset-1.5" />
      {findOpen && (
        <TerminalFindBar
          searchRef={searchRef}
          queryRef={queryRef}
          results={results}
          onClose={() => setFindOpen(false)}
        />
      )}
    </div>
  );
}
