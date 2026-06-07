import { useEffect, useState } from "react";
import { X, Users, Bot } from "lucide-react";
import { api, type ProviderInfo, type AgentProfileInfo } from "../api";
import { useStore } from "../store";
import { prettySessionText } from "../lib/sessionName";

/** Canonical target providers, in display order, with friendly labels. */
const TARGETS: Record<string, { name: string; vendor: string }> = {
  claude_code: { name: "Claude Code", vendor: "Anthropic" },
  codex: { name: "Codex CLI", vendor: "OpenAI" },
  gemini_cli: { name: "Gemini CLI", vendor: "Google" },
  grok_cli: { name: "Grok Build CLI", vendor: "xAI" },
};
const ORDER = ["claude_code", "codex", "gemini_cli", "grok_cli"];

/** Heuristic: does this profile orchestrate other agents? */
function isSupervisor(p: AgentProfileInfo): boolean {
  const hay = `${p.name} ${p.description}`.toLowerCase();
  return (
    hay.includes("supervisor") ||
    hay.includes("orchestrat") ||
    hay.includes("architect") ||
    hay.includes("delegat") ||
    hay.includes("coordinat")
  );
}

export function LaunchAgentDialog({ onClose }: { onClose: () => void }) {
  const launchAgent = useStore((s) => s.launchAgent);
  const sessions = useStore((s) => s.sessions);
  // prettySession: drop the "cao-" prefix for display.
  const workspaceDir = useStore((s) => s.workspaceDir);

  const [providers, setProviders] = useState<ProviderInfo[] | null>(null);
  const [selected, setSelected] = useState<string | null>(null);
  const [profiles, setProfiles] = useState<AgentProfileInfo[]>([]);
  const [profile, setProfile] = useState<string>("default");
  const [sessionName, setSessionName] = useState<string>(""); // "" = new session
  const [busy, setBusy] = useState(false);

  useEffect(() => {
    api
      .listProviders()
      .then((list) => {
        const installed = list.filter((p) => p.installed);
        const ordered = [...installed].sort((a, b) => {
          const ai = ORDER.indexOf(a.name);
          const bi = ORDER.indexOf(b.name);
          return (ai === -1 ? 99 : ai) - (bi === -1 ? 99 : bi);
        });
        setProviders(ordered);
        const firstTarget = ordered.find((p) => ORDER.includes(p.name));
        setSelected(firstTarget?.name ?? ordered[0]?.name ?? null);
      })
      .catch(() => setProviders([]));

    api
      .listProfiles()
      .then((list) => {
        // Supervisors first, then everything else, both alphabetical.
        const sorted = [...list].sort((a, b) => {
          const as = isSupervisor(a) ? 0 : 1;
          const bs = isSupervisor(b) ? 0 : 1;
          if (as !== bs) return as - bs;
          return a.name.localeCompare(b.name);
        });
        setProfiles(sorted);
      })
      .catch(() => setProfiles([]));
  }, []);

  // The daemon's profile store already includes the built-in default/orchestrator
  // roles, but guarantee they exist (and aren't duplicated) so the select is
  // never empty mid-load.
  const byName = new Map(profiles.map((p) => [p.name, p]));
  if (!byName.has("default"))
    byName.set("default", {
      name: "default",
      description: "Plain agent — no orchestration tools.",
      source: "builtin",
    });
  if (!byName.has("orchestrator"))
    byName.set("orchestrator", {
      name: "orchestrator",
      description: "Can assign / handoff to other agents.",
      source: "builtin",
    });
  const displayProfiles = [...byName.values()];
  const supervisors = displayProfiles.filter(isSupervisor);
  const workers = displayProfiles.filter((p) => !isSupervisor(p));

  const activeProfile = displayProfiles.find((p) => p.name === profile);
  const profileIsSupervisor = activeProfile
    ? isSupervisor(activeProfile)
    : false;

  const submit = async () => {
    if (!selected) return;
    setBusy(true);
    onClose(); // optimistic: close immediately, frame appears as pending
    // The dropdown value is the session's unique root (id); resolve its display
    // name and pass the root so provisioning actually joins that session.
    const sess = sessions.find((s) => s.id === sessionName);
    await launchAgent(selected, profile, {
      sessionName: sess?.name || undefined,
      workingDirectory: sess?.id || undefined,
    });
  };

  return (
    <div
      className="fixed inset-0 z-50 flex items-center justify-center bg-black/50"
      onClick={onClose}
    >
      <div
        className="no-drag w-[30rem] rounded-xl border border-ink-500 bg-ink-800 p-5 shadow-2xl"
        onClick={(e) => e.stopPropagation()}
      >
        <div className="mb-4 flex items-center justify-between">
          <h2 className="text-sm font-semibold text-zinc-100">Launch agent</h2>
          <button
            onClick={onClose}
            className="rounded p-1 text-zinc-500 hover:text-zinc-200"
          >
            <X size={16} />
          </button>
        </div>

        {/* Provider */}
        <label className="mb-1.5 block text-[11px] font-medium uppercase tracking-wide text-zinc-500">
          Runs on
        </label>
        <div className="mb-4 flex flex-col gap-1.5">
          {providers === null && (
            <p className="text-xs text-zinc-500">Loading providers…</p>
          )}
          {providers?.length === 0 && (
            <p className="text-xs text-amber">
              No installed CLIs detected by the backend.
            </p>
          )}
          {providers?.map((p) => {
            const friendly = TARGETS[p.name];
            const active = selected === p.name;
            return (
              <button
                key={p.name}
                onClick={() => setSelected(p.name)}
                className={`flex items-center justify-between rounded-lg border px-3 py-2 text-left transition-colors ${
                  active
                    ? "border-teal-600 bg-teal-600/10"
                    : "border-ink-500 bg-ink-700/40 hover:border-ink-400"
                }`}
              >
                <span className="text-sm text-zinc-100">
                  {friendly?.name ?? p.name}
                </span>
                <span className="font-mono text-[11px] text-zinc-500">
                  {friendly?.vendor ? `${friendly.vendor} · ` : ""}
                  {p.binary}
                </span>
              </button>
            );
          })}
        </div>

        {/* Profile / role */}
        <label className="mb-1.5 block text-[11px] font-medium uppercase tracking-wide text-zinc-500">
          Role / profile
        </label>
        <select
          value={profile}
          onChange={(e) => setProfile(e.target.value)}
          className="mb-1.5 w-full rounded-lg border border-ink-500 bg-ink-700 px-3 py-2 text-sm text-zinc-200"
        >
          {supervisors.length > 0 && (
            <optgroup label="Supervisors (orchestrate other agents)">
              {supervisors.map((p) => (
                <option key={p.name} value={p.name}>
                  {p.name}
                </option>
              ))}
            </optgroup>
          )}
          {workers.length > 0 && (
            <optgroup label="Workers / specialists">
              {workers.map((p) => (
                <option key={p.name} value={p.name}>
                  {p.name}
                </option>
              ))}
            </optgroup>
          )}
        </select>

        {/* Profile detail + orchestration hint */}
        <div className="mb-4 min-h-[2.5rem] rounded-lg border border-ink-600 bg-ink-700/30 px-3 py-2">
          {profile === "default" ? (
            <p className="text-[11px] leading-relaxed text-zinc-500">
              A standalone agent. It won&apos;t spawn or coordinate other agents.
              Pick <span className="text-zinc-300">orchestrator</span> to let it
              assign work to a team.
            </p>
          ) : profile === "orchestrator" ? (
            <div className="flex items-start gap-2">
              <Users size={13} className="mt-0.5 shrink-0 text-teal-400" />
              <p className="text-[11px] leading-relaxed text-zinc-400">
                Gets the daemon&apos;s MCP tools (<span className="text-zinc-300">list_agents</span>,{" "}
                <span className="text-zinc-300">send_message</span>,{" "}
                <span className="text-zinc-300">handoff</span>,{" "}
                <span className="text-zinc-300">assign</span>) so it can spawn and
                coordinate other agents.
              </p>
            </div>
          ) : (
            <div className="flex items-start gap-2">
              {profileIsSupervisor ? (
                <Users size={13} className="mt-0.5 shrink-0 text-teal-400" />
              ) : (
                <Bot size={13} className="mt-0.5 shrink-0 text-zinc-500" />
              )}
              <div>
                <p className="text-[11px] font-medium text-zinc-300">
                  {profileIsSupervisor
                    ? "Supervisor — can assign / handoff to other agents"
                    : "Worker / specialist"}
                </p>
                <p className="text-[11px] leading-relaxed text-zinc-500">
                  {activeProfile?.description ?? ""}
                </p>
              </div>
            </div>
          )}
        </div>

        {/* Session */}
        {sessions.length > 0 && (
          <>
            <label className="mb-1.5 block text-[11px] font-medium uppercase tracking-wide text-zinc-500">
              Session
            </label>
            <select
              value={sessionName}
              onChange={(e) => setSessionName(e.target.value)}
              className="mb-4 w-full rounded-lg border border-ink-500 bg-ink-700 px-3 py-2 text-sm text-zinc-200"
            >
              <option value="">New session</option>
              {sessions.map((s) => (
                <option key={s.id} value={s.id}>
                  Add to {prettySessionText(s.name)}
                </option>
              ))}
            </select>
          </>
        )}

        <p className="mb-4 text-[11px] text-zinc-500">
          Working directory:{" "}
          <span className="font-mono text-zinc-400">
            {workspaceDir ?? "backend default (agent's home)"}
          </span>
        </p>

        <div className="flex justify-end gap-2">
          <button
            onClick={onClose}
            className="rounded-lg px-3 py-1.5 text-sm text-zinc-400 hover:text-zinc-200"
          >
            Cancel
          </button>
          <button
            onClick={submit}
            disabled={!selected || busy}
            className="rounded-lg bg-primary px-3 py-1.5 text-sm font-medium text-ink-900 hover:bg-primary-hover disabled:opacity-50"
          >
            Launch
          </button>
        </div>
      </div>
    </div>
  );
}
