import { useEffect, useState } from "react";
import { Folder, X } from "lucide-react";
import { getVersion } from "@tauri-apps/api/app";
import { dataDir, join } from "@tauri-apps/api/path";
import { useStore } from "../store";
import { daemonQuery } from "../pty";
import { inTauri } from "../backend";
import type { AgentProfileInfo, ProviderInfo } from "../api";
import { PROVIDER_ORDER, PROVIDER_VENDOR, providerTitle } from "../lib/providerLabel";
import { basename, dirname } from "../lib/recentProjects";
import { middleTruncate } from "../lib/format";
import { BUILTIN_PROFILES } from "../lib/profiles";
import { WorkspacePicker } from "../components/WorkspacePicker";

/** The protocol version this client speaks — mirrors `taime-protocol`'s
 *  `PROTOCOL_VERSION` (src/lib.rs); peers refuse the handshake on mismatch. */
const CLIENT_PROTOCOL_VERSION = 10;

/**
 * Settings: instrument-panel facts, not marketing. Nav comes from the shell's
 * settings sidebar (`store.settingsTab`); each tab renders real daemon data or
 * an honest unknown — never a fake "Installed" badge.
 */
export function SettingsScreen() {
  const settingsTab = useStore((s) => s.settingsTab);
  return (
    <div className="h-full overflow-y-auto">
      <div className="max-w-2xl p-6">
        {settingsTab === "providers" ? (
          <ProvidersTab />
        ) : settingsTab === "profiles" ? (
          <ProfilesTab />
        ) : settingsTab === "appearance" ? (
          <AppearanceTab />
        ) : settingsTab === "about" ? (
          <AboutTab />
        ) : (
          <WorkspaceTab />
        )}
      </div>
    </div>
  );
}

// ─── Shared bits ─────────────────────────────────────────────────────────────

/** Poll a daemon list query (5s). `data === null` after the first load means
 *  the daemon didn't answer (daemonQuery returned the null fallback) — callers
 *  render honest unknowns, never fabricated state. */
function useDaemonPoll<T>(kind: string): { data: T | null; loading: boolean } {
  const [data, setData] = useState<T | null>(null);
  const [loading, setLoading] = useState(true);
  useEffect(() => {
    let alive = true;
    const load = () =>
      daemonQuery<T | null>(kind, {}, null).then((d) => {
        if (!alive) return;
        setData(d);
        setLoading(false);
      });
    load();
    const timer = setInterval(load, 5000);
    return () => {
      alive = false;
      clearInterval(timer);
    };
  }, [kind]);
  return { data, loading };
}

/** Tab header: title + one terse intro line. */
function TabHead({ title, intro }: { title: string; intro?: string }) {
  return (
    <header>
      <h1 className="text-sm font-medium text-zinc-100">{title}</h1>
      {intro && (
        <p className="mt-1 text-xs leading-relaxed text-zinc-500">{intro}</p>
      )}
    </header>
  );
}

/** One label/value settings row (read-only fact line). */
function Row({
  label,
  sub,
  children,
}: {
  label: string;
  sub?: string;
  children: React.ReactNode;
}) {
  return (
    <div className="flex items-center justify-between gap-4 border-b border-ink-700 py-3 last:border-b-0">
      <div className="min-w-0">
        <div className="truncate text-xs font-medium text-zinc-200" title={label}>
          {label}
        </div>
        {sub && (
          <div className="mt-0.5 text-[11px] leading-relaxed text-zinc-600">
            {sub}
          </div>
        )}
      </div>
      <div className="shrink-0 text-xs text-zinc-400">{children}</div>
    </div>
  );
}

/** The one daemon-down line (error voice — terse, diagnostic, no apology). */
function DaemonUnreachable() {
  return (
    <p className="flex items-center gap-2 text-[11px] text-amber">
      <span className="h-1.5 w-1.5 shrink-0 animate-pulse rounded-full bg-amber" />
      daemon unreachable · retrying
    </p>
  );
}

// ─── Providers ───────────────────────────────────────────────────────────────

function ProvidersTab() {
  const { data, loading } = useDaemonPoll<ProviderInfo[]>("providers");
  const byName = new Map((data ?? []).map((p) => [p.name, p]));
  return (
    <section className="flex flex-col gap-4">
      <TabHead
        title="Providers"
        intro="The CLI runtimes agents launch on — one provider per agent, in its own worktree. Binaries resolve from the daemon's PATH at launch."
      />
      {!loading && data === null && <DaemonUnreachable />}
      <div>
        {PROVIDER_ORDER.map((id) => {
          const info = byName.get(id);
          return (
            <div
              key={id}
              className="flex items-center gap-4 border-b border-ink-700 py-3 last:border-b-0"
            >
              <div className="min-w-0 flex-1">
                <div
                  className="truncate text-xs font-medium text-zinc-200"
                  title={providerTitle(id)}
                >
                  {providerTitle(id)}
                </div>
                <div
                  className="truncate font-mono text-[10px] text-zinc-600"
                  title={`${id} · ${PROVIDER_VENDOR[id] ?? ""}`}
                >
                  {id} · {PROVIDER_VENDOR[id]}
                </div>
              </div>
              <ProviderState info={info} loading={loading} />
            </div>
          );
        })}
      </div>
    </section>
  );
}

/** Detected-on-PATH state from the daemon's providers query; while the daemon
 *  is unreachable the state is unknown — neutral copy, no green checks. */
function ProviderState({
  info,
  loading,
}: {
  info: ProviderInfo | undefined;
  loading: boolean;
}) {
  if (loading) {
    return <span className="shrink-0 text-[11px] text-zinc-600">checking…</span>;
  }
  if (!info) {
    return (
      <span
        className="shrink-0 text-[11px] text-zinc-500"
        title="PATH state unknown until the daemon answers"
      >
        configured via PATH
      </span>
    );
  }
  return (
    <span
      className={`flex shrink-0 items-center gap-2 text-[11px] ${
        info.installed ? "text-emerald-400" : "text-amber"
      }`}
    >
      <span
        className={`h-1.5 w-1.5 rounded-full ${
          info.installed ? "bg-emerald-400" : "bg-amber"
        }`}
      />
      <span className="font-mono">{info.binary}</span>
      {info.installed ? "on PATH" : "not on PATH"}
    </span>
  );
}

// ─── Profiles ────────────────────────────────────────────────────────────────
// The daemon-mirror fallback list lives in lib/profiles.ts (BUILTIN_PROFILES) —
// the single profile-presentation source shared with the launcher/palette.

function ProfilesTab() {
  const { data, loading } = useDaemonPoll<AgentProfileInfo[]>("profiles");
  // Built-ins always exist daemon-side; show live rows when the daemon answers
  // (a ~/.taime file can replace a built-in — it then reports source "file").
  const builtins = data?.filter((p) => p.source === "builtin") ?? BUILTIN_PROFILES;
  const customs = data?.filter((p) => p.source !== "builtin") ?? null;
  return (
    <section className="flex flex-col gap-4">
      <TabHead
        title="Agent profiles"
        intro="A profile fills the agent's system prompt, model, and tools at launch. Built-ins ship with the daemon; custom profiles load from ~/.taime/agents/*.toml — a file with a built-in's name replaces it."
      />
      <div>
        <SubHead>Built-in</SubHead>
        {builtins.map((p) => (
          <ProfileRow key={p.name} profile={p} />
        ))}
      </div>
      <div>
        <SubHead>Custom · ~/.taime/agents</SubHead>
        {loading ? (
          <p className="py-2 text-[11px] text-zinc-600">loading…</p>
        ) : customs === null ? (
          <div className="py-2">
            <DaemonUnreachable />
          </div>
        ) : customs.length === 0 ? (
          <p className="py-2 text-[11px] text-zinc-600">
            None. Drop a .toml in ~/.taime/agents — it appears here and in the
            launcher.
          </p>
        ) : (
          customs.map((p) => <ProfileRow key={p.name} profile={p} />)
        )}
      </div>
    </section>
  );
}

function SubHead({ children }: { children: React.ReactNode }) {
  return (
    <div className="border-b border-ink-700 pb-1.5 text-[10px] font-semibold uppercase tracking-wider text-zinc-500">
      {children}
    </div>
  );
}

function ProfileRow({ profile }: { profile: AgentProfileInfo }) {
  return (
    <div className="flex items-start gap-3 border-b border-ink-700 py-2.5 last:border-b-0">
      <div className="min-w-0 flex-1">
        <div
          className="truncate font-mono text-xs text-zinc-200"
          title={profile.name}
        >
          {profile.name}
        </div>
        <div className="mt-0.5 text-[11px] leading-relaxed text-zinc-500">
          {profile.description}
        </div>
      </div>
      <span className="mt-0.5 shrink-0 rounded bg-ink-600 px-1.5 py-0.5 text-[9px] font-semibold uppercase tracking-wide text-zinc-500">
        {profile.source === "builtin" ? "built-in" : "file"}
      </span>
    </div>
  );
}

// ─── Workspace ───────────────────────────────────────────────────────────────

function WorkspaceTab() {
  const workspaceDir = useStore((s) => s.workspaceDir);
  const recentProjects = useStore((s) => s.recentProjects);
  const switchWorkspace = useStore((s) => s.switchWorkspace);
  const removeRecentProject = useStore((s) => s.removeRecentProject);
  const clearRecentProjects = useStore((s) => s.clearRecentProjects);

  // The REAL worktree storage root: the daemon provisions under
  // data_dir()/taime/worktrees (worktree.rs) — same dir the path API reports.
  const [storageDir, setStorageDir] = useState<string | null>(null);
  useEffect(() => {
    if (!inTauri()) return;
    let alive = true;
    dataDir()
      .then((d) => join(d, "taime", "worktrees"))
      .then((p) => {
        if (alive) setStorageDir(p);
      })
      .catch(() => {});
    return () => {
      alive = false;
    };
  }, []);

  const others = recentProjects.filter((p) => p !== workspaceDir);

  return (
    <section className="flex flex-col gap-4">
      <TabHead
        title="Workspace"
        intro="The active project root. Agents fork their worktrees from it; tasks are scoped to it."
      />

      <div className="max-w-sm">
        <WorkspacePicker />
      </div>

      <div className="border-t border-ink-700 pt-3">
        <div className="text-xs font-medium text-zinc-200">Worktree storage</div>
        <div className="mt-1">
          {storageDir ? (
            <span
              className="font-mono text-[11px] text-zinc-400"
              title={storageDir}
            >
              {middleTruncate(storageDir, 64)}
            </span>
          ) : (
            <span className="text-[11px] text-zinc-600">
              {inTauri() ? "resolving…" : "unavailable outside the app"}
            </span>
          )}
        </div>
        <p className="mt-1 text-[11px] leading-relaxed text-zinc-600">
          One worktree per agent, grouped by project. Merge or revert from the
          task review — don't edit these directly.
        </p>
      </div>

      <div className="border-t border-ink-700 pt-3">
        <div className="flex items-center justify-between">
          <div className="text-xs font-medium text-zinc-200">
            Recent workspaces
          </div>
          {others.length > 0 && (
            <button
              onClick={clearRecentProjects}
              className="rounded px-1 text-[10px] text-zinc-600 hover:text-zinc-400"
            >
              Clear all
            </button>
          )}
        </div>
        {others.length === 0 ? (
          <p className="mt-1.5 text-[11px] text-zinc-600">None yet.</p>
        ) : (
          <ul className="mt-1.5 flex flex-col gap-0.5">
            {others.map((p) => (
              <li key={p} className="group/recent flex items-center">
                <button
                  onClick={() => switchWorkspace(p)}
                  title={`Open ${p}`}
                  className="flex min-w-0 flex-1 items-center gap-2 rounded-md px-2 py-1.5 text-left hover:bg-ink-600/60"
                >
                  <Folder size={12} className="shrink-0 text-zinc-600" />
                  <span className="shrink-0 text-xs text-zinc-200">
                    {basename(p)}
                  </span>
                  <span className="min-w-0 flex-1 truncate font-mono text-[10px] text-zinc-600">
                    {dirname(p)}
                  </span>
                </button>
                <button
                  onClick={() => removeRecentProject(p)}
                  aria-label={`Remove ${p} from recents`}
                  title="Remove from recents"
                  className="ml-0.5 shrink-0 rounded p-1 text-zinc-700 opacity-0 hover:text-zinc-300 focus-visible:opacity-100 group-hover/recent:opacity-100"
                >
                  <X size={12} />
                </button>
              </li>
            ))}
          </ul>
        )}
      </div>
    </section>
  );
}

// ─── Appearance ──────────────────────────────────────────────────────────────

/** Live `prefers-reduced-motion` state (index.css collapses animations on it). */
function useReducedMotion(): boolean {
  const [reduced, setReduced] = useState(
    () => window.matchMedia("(prefers-reduced-motion: reduce)").matches,
  );
  useEffect(() => {
    const mq = window.matchMedia("(prefers-reduced-motion: reduce)");
    const onChange = () => setReduced(mq.matches);
    mq.addEventListener("change", onChange);
    return () => mq.removeEventListener("change", onChange);
  }, []);
  return reduced;
}

function AppearanceTab() {
  const terminalFontSize = useStore((s) => s.terminalFontSize);
  const reduced = useReducedMotion();
  return (
    <section className="flex flex-col gap-4">
      <TabHead title="Appearance" intro="Read-only in v1." />
      <div>
        <Row label="Theme" sub="Dark only in v1.">
          dark
        </Row>
        <Row
          label="Interface font"
          sub="Sans for chrome; mono for terminals, ids, and paths."
        >
          Geist · Geist Mono
        </Row>
        <Row
          label="Terminal font size"
          sub="⌘+ / ⌘− / ⌘0 adjust it; applies to every terminal frame."
        >
          <span className="tnum font-mono text-xs text-zinc-200">
            {terminalFontSize}px
          </span>
        </Row>
        <Row
          label="Reduced motion"
          sub="Follows the macOS Reduce Motion setting — animations collapse when on."
        >
          {reduced ? "on" : "off"}
        </Row>
      </div>
    </section>
  );
}

// ─── About ───────────────────────────────────────────────────────────────────

function AboutTab() {
  // `connected` is the real reachability signal: the agent-roster poll flips it
  // on daemon response/failure (the same fact the title-bar pill reads).
  const connected = useStore((s) => s.connected);
  const [version, setVersion] = useState<string | null>(null);
  useEffect(() => {
    if (!inTauri()) {
      setVersion("dev");
      return;
    }
    let alive = true;
    getVersion()
      .then((v) => {
        if (alive) setVersion(v);
      })
      .catch(() => {
        if (alive) setVersion("unknown");
      });
    return () => {
      alive = false;
    };
  }, []);

  return (
    <section className="flex flex-col gap-4">
      <TabHead title="About" />
      <div>
        <Row label="Version" sub="Taime desktop.">
          <span className="tnum font-mono text-xs text-zinc-200">
            {version ?? "…"}
          </span>
        </Row>
        <Row
          label="Daemon protocol"
          sub="The version this client speaks; mismatched daemons refuse the handshake."
        >
          <span className="font-mono text-xs text-zinc-200">
            v{CLIENT_PROTOCOL_VERSION}
          </span>
        </Row>
        <Row
          label="Session daemon"
          sub="taime-session-daemon — owns every agent PTY; agents survive app restarts."
        >
          <span
            className="flex items-center gap-2"
            title="taime-session-daemon"
          >
            <span
              className={`h-2 w-2 rounded-full ${
                connected ? "bg-emerald-400" : "animate-pulse bg-amber"
              }`}
            />
            <span
              className={`text-[11px] ${connected ? "text-zinc-300" : "text-amber"}`}
            >
              {connected ? "reachable" : "daemon unreachable · retrying"}
            </span>
          </span>
        </Row>
      </div>
    </section>
  );
}
