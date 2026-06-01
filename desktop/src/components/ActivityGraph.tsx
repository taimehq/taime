import { useCallback, useEffect, useMemo, useState } from "react";
import {
  X,
  GitBranch,
  RefreshCw,
  ArrowRight,
  AlertTriangle,
  FileText,
} from "lucide-react";
import { api, type ActivityGraph as Graph } from "../api";
import { useStore } from "../store";

const PROVIDER_NAME: Record<string, string> = {
  claude_code: "Claude Code",
  codex: "Codex CLI",
  gemini_cli: "Gemini CLI",
  grok_cli: "Grok Build CLI",
};

const EDGE_LABEL: Record<string, string> = {
  handoff: "handed off to",
  assign: "assigned to",
  send_message: "messaged",
};

/** Order agents so each team owner is immediately followed by its members. */
function orderTeams<T extends { terminal_id: string; member_of: string | null }>(
  agents: T[],
): T[] {
  const owners = agents.filter((a) => !a.member_of);
  const membersByOwner = new Map<string, T[]>();
  for (const a of agents) {
    if (a.member_of) {
      const list = membersByOwner.get(a.member_of) ?? [];
      list.push(a);
      membersByOwner.set(a.member_of, list);
    }
  }
  const ordered: T[] = [];
  for (const o of owners) {
    ordered.push(o, ...(membersByOwner.get(o.terminal_id) ?? []));
  }
  // Append any members whose owner isn't in the set (orphans).
  for (const a of agents) {
    if (a.member_of && !ordered.includes(a)) ordered.push(a);
  }
  return ordered;
}

function fmtTime(iso: string | null): string {
  if (!iso) return "";
  try {
    return new Date(iso).toLocaleTimeString([], { hour: "2-digit", minute: "2-digit", second: "2-digit" });
  } catch {
    return "";
  }
}

/**
 * The activity graph: not a chat log but a queryable record of who changed what,
 * when, and why. Each agent is a column of turns (with the files each turn
 * touched and its tree snapshots); orchestration edges show delegation between
 * agents; contended files are flagged. Drill into a turn's files to review.
 */
export function ActivityGraph() {
  const open = useStore((s) => s.graphOpen);
  const setGraphOpen = useStore((s) => s.setGraphOpen);
  const openDiff = useStore((s) => s.openDiff);
  const frames = useStore((s) => s.frames);
  const activeFrameKey = useStore((s) => s.activeFrameKey);

  const session = useMemo(() => {
    const active = frames.find((f) => f.key === activeFrameKey);
    return active?.sessionName ?? frames.find((f) => f.sessionName)?.sessionName ?? null;
  }, [frames, activeFrameKey]);

  const [graph, setGraph] = useState<Graph | null>(null);
  const [loading, setLoading] = useState(false);

  const nameFor = useCallback(
    (tid: string | null) => {
      if (!tid) return "?";
      const fr = frames.find((f) => f.terminalId === tid);
      const g = graph?.agents.find((a) => a.terminal_id === tid);
      const prov = fr?.provider ?? g?.provider ?? "";
      return PROVIDER_NAME[prov] ?? prov ?? tid.slice(0, 6);
    },
    [frames, graph],
  );

  const load = useCallback(async () => {
    if (!session) {
      setGraph(null);
      return;
    }
    setLoading(true);
    try {
      setGraph(await api.getGraph(session));
    } catch {
      setGraph(null);
    } finally {
      setLoading(false);
    }
  }, [session]);

  useEffect(() => {
    if (open) load();
  }, [open, load]);

  if (!open) return null;

  return (
    <div className="fixed inset-0 z-50 flex flex-col bg-ink-900/95 backdrop-blur-sm">
      <div className="flex items-center justify-between border-b border-ink-600 bg-ink-800 px-4 py-2.5">
        <div className="flex items-center gap-2.5 text-sm">
          <span className="font-semibold text-zinc-100">Activity graph</span>
          {session && (
            <span className="font-mono text-[11px] text-zinc-500">{session}</span>
          )}
        </div>
        <div className="flex items-center gap-2">
          <button
            onClick={load}
            className="flex items-center gap-1.5 rounded-lg border border-ink-600 px-2.5 py-1.5 text-xs text-zinc-300 hover:bg-ink-600"
          >
            <RefreshCw size={13} className={loading ? "animate-spin" : ""} />
            Refresh
          </button>
          <button
            onClick={() => setGraphOpen(false)}
            className="rounded-lg p-1.5 text-zinc-400 hover:bg-ink-600 hover:text-zinc-200"
            aria-label="Close graph"
          >
            <X size={18} />
          </button>
        </div>
      </div>

      <div className="min-h-0 flex-1 overflow-auto p-4">
        {!session && (
          <p className="text-sm text-zinc-500">
            Launch an agent to start building the activity graph.
          </p>
        )}

        {graph && (
          <>
            {/* Orchestration edges */}
            {graph.edges.length > 0 && (
              <section className="mb-5">
                <h3 className="mb-2 text-[11px] uppercase tracking-wide text-zinc-600">
                  Delegation
                </h3>
                <ul className="space-y-1">
                  {graph.edges.map((e, i) => (
                    <li
                      key={i}
                      className="flex items-center gap-2 text-[12px] text-zinc-400"
                    >
                      <span className="text-zinc-200">{nameFor(e.source)}</span>
                      <ArrowRight size={12} className="text-sky-400" />
                      <span className="text-zinc-500">{EDGE_LABEL[e.kind] ?? e.kind}</span>
                      <span className="text-zinc-200">{nameFor(e.target)}</span>
                      <span className="ml-auto font-mono text-[10px] text-zinc-600">
                        {fmtTime(e.ts)}
                      </span>
                    </li>
                  ))}
                </ul>
              </section>
            )}

            {/* Contention */}
            {graph.contention.length > 0 && (
              <section className="mb-5 rounded-lg border border-rose-500/30 bg-rose-500/5 p-3">
                <h3 className="mb-2 flex items-center gap-1.5 text-[11px] uppercase tracking-wide text-rose-300">
                  <AlertTriangle size={13} /> Contended files
                </h3>
                <ul className="space-y-1">
                  {graph.contention.map((c) => (
                    <li key={c.path} className="flex items-center gap-2 text-[12px]">
                      <span className="font-mono text-rose-200">{c.path}</span>
                      <span className="text-zinc-500">
                        — {c.terminals.map(nameFor).join(", ")}
                      </span>
                    </li>
                  ))}
                </ul>
              </section>
            )}

            {/* Agent columns */}
            <section>
              <h3 className="mb-2 text-[11px] uppercase tracking-wide text-zinc-600">
                Agents &amp; turns
              </h3>
              <div className="flex gap-4 overflow-x-auto pb-2">
                {orderTeams(graph.agents).map((a) => (
                  <div
                    key={a.terminal_id}
                    className={`w-64 shrink-0 rounded-lg border bg-ink-800/60 ${
                      a.member_of
                        ? "ml-2 border-ink-600 border-l-2 border-l-violet-500/50"
                        : "border-ink-600"
                    }`}
                  >
                    <div className="border-b border-ink-600 px-3 py-2">
                      <div className="flex items-center gap-1.5 text-sm font-medium text-zinc-100">
                        {nameFor(a.terminal_id)}
                        {a.member_of && (
                          <span className="rounded bg-violet-500/20 px-1.5 text-[9px] font-semibold uppercase tracking-wide text-violet-300">
                            member
                          </span>
                        )}
                      </div>
                      {a.member_of ? (
                        <div className="mt-0.5 truncate text-[10px] text-zinc-500">
                          ↳ delegated by {nameFor(a.member_of)}
                          {a.branch ? ` · shares ${a.branch}` : ""}
                        </div>
                      ) : (
                        <div className="mt-0.5 flex items-center gap-1.5 font-mono text-[10px] text-zinc-500">
                          {a.mode === "worktree" && a.branch ? (
                            <span className="flex items-center gap-1 text-sky-300">
                              <GitBranch size={10} />
                              {a.branch}
                            </span>
                          ) : (
                            <span>{a.mode ?? "—"}</span>
                          )}
                        </div>
                      )}
                    </div>
                    <div className="space-y-2 p-2">
                      {a.turns.length === 0 && (
                        <p className="px-1 py-2 text-[11px] text-zinc-600">No turns yet.</p>
                      )}
                      {a.turns.map((t) => (
                        <button
                          key={t.id}
                          onClick={() => {
                            setGraphOpen(false);
                            openDiff(a.terminal_id);
                          }}
                          className="block w-full rounded-md border border-ink-600 bg-ink-900/70 p-2 text-left hover:border-sky-500/50"
                        >
                          <div className="flex items-center justify-between">
                            <span className="text-[11px] font-semibold text-zinc-300">
                              Turn {t.turn_index + 1}
                              {!t.ended_at && (
                                <span className="ml-1.5 text-amber">• active</span>
                              )}
                            </span>
                            <span className="font-mono text-[10px] text-zinc-600">
                              {fmtTime(t.started_at)}
                            </span>
                          </div>
                          {t.files_touched.length > 0 && (
                            <div className="mt-1 flex items-center gap-1 text-[10px] text-zinc-500">
                              <FileText size={10} />
                              {t.files_touched.length} file
                              {t.files_touched.length === 1 ? "" : "s"}
                              <span className="truncate text-zinc-600">
                                — {t.files_touched.slice(0, 3).join(", ")}
                              </span>
                            </div>
                          )}
                        </button>
                      ))}
                    </div>
                  </div>
                ))}
              </div>
            </section>
          </>
        )}
      </div>
    </div>
  );
}
