/**
 * THE single agent-status module: every status conditional imports from here.
 *
 * Two vocabularies meet in this file:
 *  - wire: the daemon's inferred CAO statuses (IDLE/PROCESSING/WAITING_USER_ANSWER/
 *    COMPLETED/ERROR), the lifecycle EXITED, and the frontend-synthetic PENDING
 *    (a frame launched before the daemon returned a session id).
 *  - UI: the normalized vocabulary screens render (running/blocked/done/idle/
 *    error/launching/exited), each with one label + dot class + badge class.
 */

export type UiAgentStatus =
  | "running"
  | "blocked"
  | "done"
  | "idle"
  | "error"
  | "launching"
  | "exited"
  | "unknown";

const WIRE_TO_UI: Record<string, UiAgentStatus> = {
  PROCESSING: "running",
  WAITING_USER_ANSWER: "blocked",
  COMPLETED: "done",
  IDLE: "idle",
  ERROR: "error",
  /** Frontend-synthetic: optimistic frame, no daemon session yet. */
  PENDING: "launching",
  /** Lifecycle: the agent's process is gone. */
  EXITED: "exited",
};

/** Map a raw wire status (any case, possibly absent) to the UI vocabulary. */
export function uiStatus(wire: string | null | undefined): UiAgentStatus {
  return WIRE_TO_UI[(wire ?? "").toUpperCase()] ?? "unknown";
}

export interface StatusUi {
  /** Human label, instrument-panel voice (lowercase, terse). */
  label: string;
  /** Tailwind classes for the status dot (color + any pulse). */
  dot: string;
  /** Tailwind classes for a status chip/badge (bg + text). */
  badge: string;
}

export const STATUS_UI: Record<UiAgentStatus, StatusUi> = {
  running: {
    label: "working",
    dot: "bg-teal-400 animate-pulse",
    badge: "bg-teal-600/15 text-teal-300",
  },
  blocked: {
    label: "needs you",
    dot: "bg-amber animate-pulse",
    badge: "bg-amber/20 text-amber",
  },
  done: {
    label: "done",
    dot: "bg-emerald-400",
    badge: "bg-emerald-500/15 text-emerald-400",
  },
  idle: {
    label: "idle",
    dot: "bg-zinc-500",
    badge: "bg-ink-600 text-zinc-400",
  },
  error: {
    label: "error",
    dot: "bg-red-500",
    badge: "bg-red-500/15 text-red-400",
  },
  launching: {
    label: "launching",
    dot: "bg-amber animate-pulse",
    badge: "bg-amber/15 text-amber",
  },
  exited: {
    label: "exited",
    dot: "bg-zinc-700",
    badge: "bg-ink-600 text-zinc-500",
  },
  unknown: {
    label: "—",
    dot: "bg-zinc-600",
    badge: "bg-ink-600 text-zinc-500",
  },
};

/** The normalized, human label for a raw wire status (e.g. "working"). */
export function statusLabel(wire: string | null | undefined): string {
  return STATUS_UI[uiStatus(wire)].label;
}

/** Tailwind classes for the status dot of a raw wire status. */
export function statusDotClass(wire: string | null | undefined): string {
  return STATUS_UI[uiStatus(wire)].dot;
}

/** Tailwind classes for a status chip/badge of a raw wire status. */
export function statusBadgeClass(wire: string | null | undefined): string {
  return STATUS_UI[uiStatus(wire)].badge;
}
