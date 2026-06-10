import { useEffect, useRef } from "react";
import { Terminal } from "@xterm/xterm";
import { FitAddon } from "@xterm/addon-fit";
import { WebglAddon } from "@xterm/addon-webgl";
import { SearchAddon } from "@xterm/addon-search";
import type { Channel } from "@tauri-apps/api/core";
import "@xterm/xterm/css/xterm.css";
import {
  daemonAttach,
  daemonWrite,
  daemonResize,
  daemonAck,
  daemonCloseView,
  daemonCheckpoint,
} from "../pty";
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
  onConnectionChange?: (state: "open" | "closed") => void;
}

// Console well sits below the surface ladder (#070809 — darkest, slightly
// warmer black per the design system's "console is darkest" rule).
const THEME = {
  background: "#070809",
  foreground: "#c8c7c2",
  cursor: "#c8c7c2",
  cursorAccent: "#070809",
  selectionBackground: "#2f4a7a",
  black: "#070809",
  red: "#ef5b50",
  green: "#46c46e",
  yellow: "#e3a93a",
  blue: "#5b8def",
  magenta: "#9a7cf0",
  cyan: "#8db1f5",
  white: "#c8c7c2",
  brightBlack: "#6f7681",
};

/**
 * Terminal view for the session-daemon transport (the Rust PTY path, every
 * provider) — raw bytes in over a binary `Channel`, `daemon_write`/
 * `daemon_resize` out.
 *
 * Mount protocol: fit xterm to the container, then `daemon_attach` with the real
 * viewport so the daemon resizes + sends a grid repaint matching it (handoff step
 * 1), then live output streams. The channel ref is held for the view's lifetime
 * (GC of it would silently stop output). Each processed chunk advances a byte
 * counter acked back (batched per frame) for backpressure; Enter fires an
 * attribution checkpoint. Unmount = `daemon_close_view` (the agent keeps running,
 * survives even an app crash); explicit kill is separate.
 */
export function TerminalViewRustPty({
  sessionId,
  frameKey,
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
        daemonAck(sessionId, ackedBytes);
      });
    };

    const term = new Terminal({
      cursorBlink: true,
      // Initial size only — read non-reactively; live changes via the zoom
      // effect below so this mount effect isn't keyed on font size.
      fontSize: useStore.getState().terminalFontSize,
      fontFamily:
        "'Geist Mono', ui-monospace, 'JetBrains Mono', SFMono-Regular, Menlo, Monaco, monospace",
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
    applyResizeRef.current = () => daemonResize(sessionId, term.rows, term.cols);
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

    // Mouse-wheel scrollback even while the app (Claude's TUI) has mouse
    // reporting ON. With mouse tracking enabled, xterm forwards the wheel to the
    // app, so the transcript won't scroll — the #1 "I can't scroll" complaint.
    // Force a local scrollback scroll in the NORMAL buffer (the alt-screen has no
    // scrollback, so defer to the app there). Capture phase + stopPropagation so
    // we win over xterm's forward-to-app handling.
    const wheelOpts = { capture: true, passive: false } as const;
    const onWheel = (e: WheelEvent) => {
      if (term.modes.mouseTrackingMode === "none") return; // xterm scrolls natively
      if (term.buffer.active.type !== "normal") return; // alt-screen: app owns it
      const perLine = e.deltaMode === 1 ? 1 : 16; // line vs. pixel deltas
      const lines = e.deltaY / perLine;
      term.scrollLines(Math.trunc(lines) || (e.deltaY > 0 ? 1 : -1));
      e.preventDefault();
      e.stopPropagation();
    };
    el.addEventListener("wheel", onWheel, wheelOpts);

    // Sniff the running model from the agent's startup banner (best-effort).
    const sniffModel = makeModelSniffer((m) =>
      useStore.getState().setFrameModel(frameKey, m),
    );

    // Register an input writer so dropped file/screenshot paths can be typed in.
    const unregisterInput = registerTerminalInput(sessionId, (text) => {
      daemonWrite(sessionId, text);
    });

    term.onData((data) => {
      daemonWrite(sessionId, data);
      // Strongest attribution signal: the user submitted a command (Enter). The
      // daemon coalesces/guards empty turns, so spurious Enters are harmless.
      if (data.includes("\r")) daemonCheckpoint(sessionId, "submit");
    });

    resizeObserver = new ResizeObserver(() => {
      clearTimeout(resizeTimer);
      resizeTimer = setTimeout(() => {
        safeFit();
        daemonResize(sessionId, term.rows, term.cols);
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
      const ch = await daemonAttach(
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
          // A REAL process exit (the daemon's Exited control) — authoritative
          // regardless of mount state, so the lifecycle flip is unconditional.
          if (alive) term.write("\r\n\x1b[33m[process exited]\x1b[0m\r\n");
          useStore.getState().markRustPtyExited(sessionId);
          onConnectionChange?.("closed");
        },
        // Attribution turn boundaries (daemon transport only) → store.
        (turn) => useStore.getState().recordTurn(frameKey, turn),
        // Phase 4 status push → badge map (resolves terminalId via the session).
        (status) => useStore.getState().setDaemonSessionStatus(sessionId, status),
        // Phase 6 fs push → mark the agent's terminal dirty (its diff is stale).
        (paths) => useStore.getState().markDaemonFsDirty(sessionId, paths),
        // The daemon CONNECTION dropped without a process exit (daemon crash /
        // codec error — deliberate detaches are silent). The agent may still be
        // running: never flip it to exited here (detach ≠ kill, the safe
        //-context-switching invariant). Flag the lost connection so the
        // reconcile poll may demote this session against the daemon roster even
        // while framed — without the flag, the framed exemption would wedge a
        // crashed daemon's agent as "running · PTY attached" forever.
        () => {
          useStore.getState().setRustPtyConnectionLost(sessionId, true);
          if (!alive) return; // post-unmount push — nothing to render
          term.write("\r\n\x1b[33m[daemon connection lost — reattach to resume]\x1b[0m\r\n");
          onConnectionChange?.("closed");
        },
      );
      if (!alive) {
        // Unmounted while the attach was in flight: detach so we don't leave a
        // phantom attachment with no acker (which would stall the agent at the
        // backpressure watermark).
        daemonCloseView(sessionId);
        return;
      }
      channelRef.current = ch as Channel<unknown> | null;
      // A live attach supersedes any earlier connection-lost flag.
      if (ch) useStore.getState().setRustPtyConnectionLost(sessionId, false);
      onConnectionChange?.("open");
      // Nudge a redraw so a reattached TUI repaints cleanly at the current size.
      safeFit();
      daemonResize(sessionId, term.rows, term.cols);
    })();

    return () => {
      alive = false;
      cancelAnimationFrame(rafId);
      clearTimeout(resizeTimer);
      resizeObserver?.disconnect();
      el.removeEventListener("wheel", onWheel, wheelOpts);
      cleanupClipboard();
      unregisterInput();
      // Drop our ref. NOTE: the channel's callback is NOT dead yet — Tauri keeps
      // it registered until the app-side pump drops the Channel (the in-order
      // "end" message), so every attach callback above must stay alive-gated.
      channelRef.current = null;
      // Closing the view detaches — it does NOT kill the agent.
      daemonCloseView(sessionId);
      offResults.dispose();
      searchRef.current = null;
      termRef.current = null;
      fitRef.current = null;
      term.dispose();
    };
  }, [sessionId, frameKey, onConnectionChange]);

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
