import { useEffect, useRef } from "react";
import { Terminal } from "@xterm/xterm";
import { FitAddon } from "@xterm/addon-fit";
import { WebglAddon } from "@xterm/addon-webgl";
import { SearchAddon } from "@xterm/addon-search";
import "@xterm/xterm/css/xterm.css";
import { terminalWsUrl } from "../api";
import { wireClipboard } from "../lib/terminalClipboard";
import { registerTerminalInput } from "../lib/terminalInput";
import { makeModelSniffer } from "../lib/parseModel";
import { useStore } from "../store";
import { useTerminalFind } from "../hooks/useTerminalFind";
import { TerminalFindBar } from "./TerminalFindBar";

interface TerminalViewProps {
  terminalId: string;
  /** Frame id — used to attribute the parsed model back to this frame. */
  frameKey: string;
  /** Notifies parent of connection lifecycle for status display. */
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
 * A live, low-latency view into one CLI process. Server→client frames are raw
 * PTY bytes (binary); client→server is JSON {type:input|resize}. This exactly
 * matches the CAO terminal_ws contract — do not change the framing.
 */
export function TerminalView({
  terminalId,
  frameKey,
  onConnectionChange,
}: TerminalViewProps) {
  const containerRef = useRef<HTMLDivElement>(null);
  // Held so the font-zoom effect can mutate the live terminal without tearing
  // it down. `applyResize` re-reports rows/cols after a size change (the
  // ResizeObserver doesn't fire — the container didn't change, the glyphs did).
  const termRef = useRef<Terminal | null>(null);
  const fitRef = useRef<FitAddon | null>(null);
  const applyResizeRef = useRef<() => void>(() => {});
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
    let ws: WebSocket | null = null;
    let resizeObserver: ResizeObserver | null = null;
    let resizeTimer: ReturnType<typeof setTimeout> | undefined;
    let rafId = 0;

    const term = new Terminal({
      cursorBlink: true,
      // Initial size only — read non-reactively so this mount effect isn't
      // keyed on font size. Live changes are applied by the zoom effect below.
      fontSize: useStore.getState().terminalFontSize,
      fontFamily:
        "ui-monospace, 'JetBrains Mono', SFMono-Regular, Menlo, Monaco, monospace",
      scrollback: 10000,
      allowProposedApi: true,
      // Let the user drag-select text even when the app (tmux/TUI) has mouse
      // tracking on — Option-drag (macOS) or Shift-drag forces local selection,
      // so copy works in the CAO/tmux terminal too. Right-click selects a word.
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

    // GPU rendering for 60fps under heavy output; gracefully fall back.
    try {
      const webgl = new WebglAddon();
      webgl.onContextLoss(() => webgl.dispose());
      term.loadAddon(webgl);
    } catch {
      /* canvas/dom renderer fallback — still correct, just slower */
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
        /* element not measurable yet */
      }
    };

    const sendResize = () => {
      if (ws && ws.readyState === WebSocket.OPEN) {
        ws.send(
          JSON.stringify({ type: "resize", rows: term.rows, cols: term.cols }),
        );
      }
    };
    applyResizeRef.current = sendResize;

    // Cross-platform copy/paste (shared across transports).
    const cleanupClipboard = wireClipboard(term, el);

    // Sniff the running model from the agent's startup banner (best-effort).
    const sniffModel = makeModelSniffer((m) =>
      useStore.getState().setFrameModel(frameKey, m),
    );

    // Register an input writer so dropped file/screenshot paths can be typed in.
    const unregisterInput = registerTerminalInput(terminalId, (text) => {
      if (ws && ws.readyState === WebSocket.OPEN) {
        ws.send(JSON.stringify({ type: "input", data: text }));
      }
    });

    term.onData((data) => {
      if (ws && ws.readyState === WebSocket.OPEN) {
        ws.send(JSON.stringify({ type: "input", data }));
      }
    });

    resizeObserver = new ResizeObserver(() => {
      clearTimeout(resizeTimer);
      resizeTimer = setTimeout(() => {
        safeFit();
        sendResize();
      }, 50);
    });
    resizeObserver.observe(el);
    rafId = requestAnimationFrame(safeFit);
    term.focus();

    // Connect (URL comes from the Tauri-resolved config — async).
    terminalWsUrl(terminalId)
      .then((url) => {
        if (!alive) return;
        ws = new WebSocket(url);
        ws.binaryType = "arraybuffer";
        ws.onopen = () => {
          safeFit();
          sendResize();
          onConnectionChange?.("open");
        };
        ws.onmessage = (e) => {
          if (e.data instanceof ArrayBuffer) {
            const bytes = new Uint8Array(e.data);
            term.write(bytes);
            sniffModel(bytes);
          }
        };
        ws.onclose = () => {
          if (alive) {
            term.write("\r\n\x1b[33m[connection closed]\x1b[0m\r\n");
            onConnectionChange?.("closed");
          }
        };
        ws.onerror = () => {
          if (alive) {
            term.write("\r\n\x1b[31m[connection error]\x1b[0m\r\n");
          }
        };
      })
      .catch(() => {
        if (alive) {
          term.write("\r\n\x1b[31m[could not resolve backend URL]\x1b[0m\r\n");
        }
      });

    return () => {
      alive = false;
      cancelAnimationFrame(rafId);
      clearTimeout(resizeTimer);
      resizeObserver?.disconnect();
      cleanupClipboard();
      unregisterInput();
      ws?.close();
      offResults.dispose();
      searchRef.current = null;
      termRef.current = null;
      fitRef.current = null;
      term.dispose();
    };
  }, [terminalId, frameKey, onConnectionChange]);

  // Apply font-zoom (Cmd ±/0) to the live terminal without recreating it.
  useEffect(() => {
    const term = termRef.current;
    if (!term || term.options.fontSize === fontSize) return;
    term.options.fontSize = fontSize;
    try {
      fitRef.current?.fit();
    } catch {
      /* element not measurable yet */
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
