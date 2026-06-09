import { Component, type ErrorInfo, type ReactNode } from "react";

interface Props {
  children: ReactNode;
}
interface State {
  error: Error | null;
}

/**
 * Last-resort safety net: any render/effect throw is caught here and shown as a
 * readable panel instead of a blank white screen. Each layout region should
 * still degrade gracefully on its own; this only catches what slips through.
 */
export class ErrorBoundary extends Component<Props, State> {
  state: State = { error: null };

  static getDerivedStateFromError(error: Error): State {
    return { error };
  }

  componentDidCatch(error: Error, info: ErrorInfo) {
    console.error("[taime] UI crash:", error, info.componentStack);
  }

  render() {
    if (!this.state.error) return this.props.children;
    return (
      <div className="flex h-full flex-col items-center justify-center gap-3 bg-ink-900 p-8 text-center">
        <h1 className="text-base font-semibold text-zinc-100">
          Taime hit a UI error
        </h1>
        <p className="max-w-md text-sm text-zinc-500">
          The interface failed to render. The backend may still be running.
        </p>
        <pre className="max-w-xl overflow-auto rounded-lg border border-red-900/60 bg-ink-800 p-3 text-left font-mono text-xs text-red-300">
          {this.state.error.message}
        </pre>
        <button
          className="no-drag rounded-md bg-ink-600 px-3 py-1.5 text-xs text-zinc-200 hover:bg-ink-500"
          onClick={() => this.setState({ error: null })}
        >
          Try again
        </button>
      </div>
    );
  }
}
