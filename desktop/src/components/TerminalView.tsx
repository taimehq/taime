import { useEffect, useRef } from "react";
import { Terminal } from "@xterm/xterm";
import { FitAddon } from "@xterm/addon-fit";
import { WebglAddon } from "@xterm/addon-webgl";
import "@xterm/xterm/css/xterm.css";
import { terminalWsUrl } from "../api";

interface TerminalViewProps {
  terminalId: string;
  /** Notifies parent of connection lifecycle for status display. */
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
 * A live, low-latency view into one CLI process. Server→client frames are raw
 * PTY bytes (binary); client→server is JSON {type:input|resize}. This exactly
 * matches the CAO terminal_ws contract — do not change the framing.
 */
export function TerminalView({
  terminalId,
  onConnectionChange,
}: TerminalViewProps) {
  const containerRef = useRef<HTMLDivElement>(null);

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
      fontSize: 13,
      fontFamily:
        "ui-monospace, 'JetBrains Mono', SFMono-Regular, Menlo, Monaco, monospace",
      scrollback: 10000,
      allowProposedApi: true,
      theme: THEME,
    });

    const fitAddon = new FitAddon();
    term.loadAddon(fitAddon);
    term.open(el);

    // GPU rendering for 60fps under heavy output; gracefully fall back.
    try {
      const webgl = new WebglAddon();
      webgl.onContextLoss(() => webgl.dispose());
      term.loadAddon(webgl);
    } catch {
      /* canvas/dom renderer fallback — still correct, just slower */
    }

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

    // Copy selection to clipboard.
    term.onSelectionChange(() => {
      const sel = term.getSelection();
      if (sel) navigator.clipboard.writeText(sel).catch(() => {});
    });
    term.attachCustomKeyEventHandler((e) => {
      if (e.ctrlKey && e.shiftKey && e.key === "C") {
        const sel = term.getSelection();
        if (sel) navigator.clipboard.writeText(sel).catch(() => {});
        return false;
      }
      return true;
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
            term.write(new Uint8Array(e.data));
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
      ws?.close();
      term.dispose();
    };
  }, [terminalId, onConnectionChange]);

  return (
    <div className="relative h-full w-full overflow-hidden bg-ink-900">
      <div ref={containerRef} className="absolute inset-1.5" />
    </div>
  );
}
