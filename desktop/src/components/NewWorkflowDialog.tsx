import { useEffect, useMemo, useState } from "react";
import { ChevronDown, ChevronRight, Loader2, RotateCcw, X } from "lucide-react";
import { api, type ProviderInfo } from "../api";
import { useStore } from "../store";
import { providerTitle, PROVIDER_ORDER } from "../lib/providerLabel";
import {
  composePrompt,
  type WorkflowGenScope,
} from "../lib/workflowGenPrompt";

const FOCUS_RING =
  "focus-visible:outline-none focus-visible:ring-1 focus-visible:ring-accent";

type Mode = "json" | "ai";

/** Runnable starter definition: two nodes, a keyword branch and a bounded
 *  loop-back edge — every routing concept a definition can carry, no filler. */
const TEMPLATE = `{
  "name": "build-and-verify",
  "entry": "build",
  "max_iterations": 6,
  "nodes": [
    {
      "id": "build",
      "profile": "feature-builder",
      "prompt": "Implement the requested change in this repository. When done, post a one-paragraph summary of what you changed using the share tool.",
      "output_key": "build_result"
    },
    {
      "id": "verify",
      "profile": "bug-fixer",
      "prompt": "Run the project's test suite. If everything passes, reply PASS. Otherwise reply FAIL followed by the failing output.",
      "output_key": "verdict"
    }
  ],
  "edges": [
    { "from": "build", "to": "verify", "when": "always" },
    { "from": "verify", "to": "build", "when": "keyword:FAIL" }
  ]
}
`;

/** The dialog's three generation scopes — ids match WorkflowGenScope. */
const SCOPES: { id: WorkflowGenScope; label: string; desc: string }[] = [
  {
    id: "workflow_only",
    label: "Workflow only",
    desc: "Creates one workflow definition. Does not run it, does not launch agents.",
  },
  {
    id: "workflow_and_run",
    label: "Workflow + first run",
    desc: "Creates the workflow, then starts one run — node agents will spawn and do real work.",
  },
  {
    id: "context_aware",
    label: "Context-aware",
    desc: "Reads your existing agents first (list_agents) and designs the workflow to complement them. Does not run it.",
  },
];

// ─── Client-side pre-validation (mirrors the daemon's workflow.rs checks) ────

/** "always" | "keyword:WORD" | "/regex/" — the daemon's when-grammar. */
function validateWhen(when: unknown): string | null {
  if (typeof when !== "string") return "`when` must be a string";
  const w = when.trim();
  if (w === "always") return null;
  if (w.startsWith("keyword:"))
    return w.slice("keyword:".length).trim() ? null : "keyword: condition is empty";
  if (w.length >= 2 && w.startsWith("/") && w.endsWith("/")) {
    try {
      new RegExp(w.slice(1, -1));
      return null;
    } catch (e) {
      return `bad regex in edge condition: ${e instanceof Error ? e.message : String(e)}`;
    }
  }
  return `unknown edge condition "${w}" (use always | keyword:WORD | /regex/)`;
}

/** Cheap structural checks before the daemon round-trip. The daemon stays
 *  authoritative — this only catches what a round-trip would bounce anyway. */
function validateDefinition(text: string): string[] {
  let parsed: unknown;
  try {
    parsed = JSON.parse(text);
  } catch (e) {
    return [`not valid JSON: ${e instanceof Error ? e.message : String(e)}`];
  }
  if (typeof parsed !== "object" || parsed === null || Array.isArray(parsed))
    return ["definition must be a JSON object"];
  const d = parsed as Record<string, unknown>;
  const errors: string[] = [];

  if (typeof d.name !== "string" || !d.name.trim())
    errors.push("`name` is required");
  if (typeof d.entry !== "string" || !d.entry.trim())
    errors.push("`entry` is required");

  if (!Array.isArray(d.nodes) || d.nodes.length === 0) {
    errors.push("`nodes` must be a non-empty array");
    return errors;
  }
  const ids = new Set<string>();
  d.nodes.forEach((n, i) => {
    if (typeof n !== "object" || n === null || Array.isArray(n)) {
      errors.push(`nodes[${i}] must be an object`);
      return;
    }
    const node = n as Record<string, unknown>;
    const label = typeof node.id === "string" && node.id ? `"${node.id}"` : `[${i}]`;
    if (typeof node.id !== "string" || !node.id.trim())
      errors.push(`nodes[${i}] needs an id`);
    else if (ids.has(node.id)) errors.push(`duplicate node id "${node.id}"`);
    else ids.add(node.id);
    if (typeof node.prompt !== "string" || !node.prompt.trim())
      errors.push(`node ${label} needs a non-empty prompt`);
  });

  if (typeof d.entry === "string" && d.entry.trim() && !ids.has(d.entry))
    errors.push(`entry node "${d.entry}" not found in nodes`);

  if (d.edges !== undefined && !Array.isArray(d.edges))
    errors.push("`edges` must be an array");
  (Array.isArray(d.edges) ? d.edges : []).forEach((e, i) => {
    if (typeof e !== "object" || e === null || Array.isArray(e)) {
      errors.push(`edges[${i}] must be an object`);
      return;
    }
    const edge = e as Record<string, unknown>;
    if (typeof edge.from !== "string" || !ids.has(edge.from))
      errors.push(`edges[${i}].from is not a node id`);
    if (typeof edge.to !== "string" || !ids.has(edge.to))
      errors.push(`edges[${i}].to is not a node id`);
    // Absent `when` defaults to "always" daemon-side — only validate if given.
    if (edge.when !== undefined) {
      const werr = validateWhen(edge.when);
      if (werr) errors.push(`edges[${i}]: ${werr}`);
    }
  });

  if (
    d.max_iterations !== undefined &&
    (typeof d.max_iterations !== "number" ||
      !Number.isInteger(d.max_iterations) ||
      d.max_iterations < 1)
  )
    errors.push("`max_iterations` must be a positive integer");

  return errors;
}

// ─── Shared chrome ───────────────────────────────────────────────────────────

/** Collapsible panel: chevron header + bordered body. */
function Disclosure({
  label,
  open,
  onToggle,
  children,
}: {
  label: string;
  open: boolean;
  onToggle: () => void;
  children: React.ReactNode;
}) {
  return (
    <div className="rounded-lg border border-ink-600">
      <button
        onClick={onToggle}
        aria-expanded={open}
        className={`flex w-full items-center gap-1.5 rounded-lg px-3 py-2 text-[11px] font-medium text-zinc-400 hover:text-zinc-200 ${FOCUS_RING}`}
      >
        {open ? (
          <ChevronDown size={12} className="shrink-0 text-zinc-600" />
        ) : (
          <ChevronRight size={12} className="shrink-0 text-zinc-600" />
        )}
        {label}
      </button>
      {open && <div className="border-t border-ink-700 px-3 py-2.5">{children}</div>}
    </div>
  );
}

/** One schema-reference row: mono term, plain definition. */
function SchemaRow({ term, def }: { term: string; def: string }) {
  return (
    <p className="text-[11px] leading-relaxed text-zinc-500">
      <span className="font-mono text-zinc-300">{term}</span> — {def}
    </p>
  );
}

/**
 * New-workflow authoring: Write JSON (template + pre-validation + daemon
 * create) or Generate with AI (scoped prompt composed for an orchestrator
 * agent that calls create_workflow itself). Store-owned visibility
 * (newWorkflowOpen) so the screen header and the sidebar "+" share it.
 */
export function NewWorkflowDialog({ onClose }: { onClose: () => void }) {
  const connected = useStore((s) => s.connected);
  const [mode, setMode] = useState<Mode>("json");

  // ── Write JSON state ──
  const [text, setText] = useState(TEMPLATE);
  const [clientErrors, setClientErrors] = useState<string[]>([]);
  const [daemonError, setDaemonError] = useState<string | null>(null);
  const [saving, setSaving] = useState(false);
  const [schemaOpen, setSchemaOpen] = useState(false);

  // ── Generate with AI state ──
  const [description, setDescription] = useState("");
  const [scope, setScope] = useState<WorkflowGenScope>("workflow_only");
  const [providers, setProviders] = useState<ProviderInfo[] | null>(null);
  const [provider, setProvider] = useState<string | null>(null);
  const [previewOpen, setPreviewOpen] = useState(false);
  const [launching, setLaunching] = useState(false);

  // Re-fetch on reconnect so a daemon restart doesn't strand a stale list.
  useEffect(() => {
    api
      .listProviders()
      .then((list) => {
        const installed = list.filter((p) => p.installed);
        const ordered = [...installed].sort((a, b) => {
          const ai = PROVIDER_ORDER.indexOf(a.name);
          const bi = PROVIDER_ORDER.indexOf(b.name);
          return (ai === -1 ? 99 : ai) - (bi === -1 ? 99 : bi);
        });
        setProviders(ordered);
        setProvider((prev) => prev ?? ordered[0]?.name ?? null);
      })
      .catch(() => setProviders([]));
  }, [connected]);

  // The exact assignment the generator agent receives — previewed live.
  const composed = useMemo(
    () => composePrompt(scope, description.trim()),
    [scope, description],
  );

  const save = async () => {
    if (saving) return;
    const errs = validateDefinition(text);
    setClientErrors(errs);
    setDaemonError(null);
    if (errs.length > 0) return; // inline errors — no daemon round-trip
    setSaving(true);
    try {
      const r = await api.createWorkflow(text);
      if (r.ok && r.name) {
        onClose();
        const s = useStore.getState();
        s.setSection("workflows");
        s.setSelectedWorkflow(r.name);
        s.showSnackbar({ type: "success", message: `Workflow ${r.name} created` });
      } else {
        // The daemon's validation message, verbatim.
        setDaemonError(r.error ?? "create failed — no error reported");
      }
    } finally {
      setSaving(false);
    }
  };

  const canLaunch =
    !!provider && description.trim().length > 0 && !launching && connected;

  const launch = async () => {
    if (!canLaunch || !provider) return;
    setLaunching(true);
    const s = useStore.getState();
    onClose(); // optimistic: the agent frame appears as it spawns
    s.setSection("agents"); // launch focuses the new frame (activeFrameKey)
    await s.launchAgent(provider, "orchestrator", {
      taskId: null,
      assignment: composed,
    });
    // launchAgent surfaced its own error snackbar on failure — don't mask it.
    const after = useStore.getState();
    if (after.snackbar?.type !== "error")
      after.showSnackbar({
        type: "info",
        message: "Generator launched — the workflow appears in Workflows once created.",
      });
  };

  const handleKey = (e: React.KeyboardEvent) => {
    if (e.key === "Escape") {
      e.preventDefault();
      e.stopPropagation();
      onClose();
      return;
    }
    if (e.key !== "Enter") return;
    const tag = (e.target as HTMLElement).tagName;
    if (tag === "BUTTON" || tag === "SELECT") return; // native activation
    if (tag === "TEXTAREA" && !(e.metaKey || e.ctrlKey)) return; // newline
    e.preventDefault();
    if (mode === "json") void save();
    else void launch();
  };

  // ── Write JSON tab ───────────────────────────────────────────────────────
  const jsonTab = (
    <>
      <div className="mb-1.5 flex items-center">
        <label className="block text-[11px] font-medium uppercase tracking-wide text-zinc-500">
          Definition
        </label>
        <span className="flex-1" />
        <button
          onClick={() => {
            setText(TEMPLATE);
            setClientErrors([]);
            setDaemonError(null);
          }}
          disabled={saving}
          title="Replace the editor with the starter template"
          className={`flex items-center gap-1 rounded px-1.5 py-0.5 text-[11px] text-zinc-500 hover:text-zinc-300 disabled:opacity-50 ${FOCUS_RING}`}
        >
          <RotateCcw size={11} />
          Insert template
        </button>
      </div>
      <textarea
        value={text}
        onChange={(e) => {
          setText(e.target.value);
          // Stale messages don't outlive the edit they critiqued.
          if (clientErrors.length > 0) setClientErrors([]);
          if (daemonError) setDaemonError(null);
        }}
        rows={16}
        spellCheck={false}
        autoFocus
        disabled={saving}
        className={`w-full resize-y rounded-lg border bg-ink-900/60 px-3 py-2 font-mono text-[12px] leading-relaxed text-zinc-200 placeholder:text-zinc-600 disabled:opacity-60 ${
          clientErrors.length > 0 || daemonError
            ? "border-red-500/50"
            : "border-ink-500"
        } ${FOCUS_RING}`}
      />

      {/* Inline errors: client pre-validation first, then the daemon verbatim. */}
      {clientErrors.length > 0 && (
        <div className="mt-1.5 flex flex-col gap-0.5">
          {clientErrors.map((err, i) => (
            <p key={i} className="break-words text-[11px] text-red-400">
              {err}
            </p>
          ))}
        </div>
      )}
      {daemonError && (
        <p className="mt-1.5 break-words text-[11px] text-red-400">{daemonError}</p>
      )}

      <div className="mt-3">
        <Disclosure
          label="Schema reference"
          open={schemaOpen}
          onToggle={() => setSchemaOpen((o) => !o)}
        >
          <div className="flex flex-col gap-1">
            <SchemaRow term="name" def="unique workflow name" />
            <SchemaRow term="entry" def="node id the run starts at" />
            <SchemaRow
              term="max_iterations"
              def="loop bound across back-edges; optional, default 20"
            />
            <SchemaRow
              term="nodes[]"
              def="{ id, profile, prompt, output_key?, provider? }"
            />
            <SchemaRow
              term="· profile"
              def="orchestrator · feature-builder · bug-fixer · security-reviewer · product-builder · default · any ~/.taime/agents profile"
            />
            <SchemaRow
              term="· prompt"
              def="must be self-contained — the worker sees nothing else"
            />
            <SchemaRow
              term="· output_key"
              def="key the worker posts its result to via share; default: the node id"
            />
            <SchemaRow
              term="· provider"
              def="claude_code · codex · gemini_cli · grok_cli; absent = run default"
            />
            <SchemaRow term="edges[]" def="{ from, to, when }" />
            <SchemaRow
              term="· when"
              def={`"always" | "keyword:WORD" | "/regex/" — first matching edge wins; no match = node is terminal`}
            />
            <SchemaRow
              term="· loops"
              def="back-edges to earlier nodes, bounded by max_iterations"
            />
          </div>
        </Disclosure>
      </div>
    </>
  );

  // ── Generate with AI tab ─────────────────────────────────────────────────
  const aiTab = (
    <>
      <label className="mb-1.5 block text-[11px] font-medium uppercase tracking-wide text-zinc-500">
        Description
      </label>
      <textarea
        value={description}
        onChange={(e) => setDescription(e.target.value)}
        placeholder="Describe the workflow — steps, branches, when to loop, what done means."
        rows={4}
        autoFocus
        disabled={launching}
        className={`mb-4 w-full resize-y rounded-lg border border-ink-500 bg-ink-700 px-3 py-2 text-sm leading-relaxed text-zinc-200 placeholder:text-zinc-600 disabled:opacity-60 ${FOCUS_RING}`}
      />

      {/* Scope — what the generator is allowed to do beyond authoring. */}
      <label className="mb-1.5 block text-[11px] font-medium uppercase tracking-wide text-zinc-500">
        Scope
      </label>
      <div className="mb-4 flex flex-col gap-1.5" role="radiogroup" aria-label="Scope">
        {SCOPES.map((sc) => {
          const active = scope === sc.id;
          return (
            <button
              key={sc.id}
              onClick={() => setScope(sc.id)}
              role="radio"
              aria-checked={active}
              disabled={launching}
              className={`flex w-full items-start gap-2.5 rounded-lg border px-3 py-2 text-left transition-colors disabled:opacity-60 ${
                active
                  ? "border-accent bg-accent/10"
                  : "border-ink-500 bg-ink-700/40 hover:border-ink-400"
              } ${FOCUS_RING}`}
            >
              <span
                className={`mt-1 h-2 w-2 shrink-0 rounded-full ${
                  active ? "bg-accent" : "bg-ink-500"
                }`}
              />
              <span className="flex min-w-0 flex-col">
                <span className="text-xs font-medium text-zinc-100">{sc.label}</span>
                <span className="text-[11px] leading-relaxed text-zinc-500">
                  {sc.desc}
                </span>
              </span>
            </button>
          );
        })}
      </div>

      {/* Provider — powers the generator agent, not the workflow's nodes. */}
      <label className="mb-1.5 block text-[11px] font-medium uppercase tracking-wide text-zinc-500">
        Generator runs on
      </label>
      {providers === null ? (
        <p className="mb-1.5 text-xs text-zinc-600">
          {connected ? "Loading providers…" : "daemon unreachable · retrying"}
        </p>
      ) : providers.length === 0 ? (
        <p className="mb-1.5 text-xs text-amber">
          {connected
            ? "No installed CLIs detected by the backend."
            : "daemon unreachable · retrying"}
        </p>
      ) : (
        <select
          value={provider ?? ""}
          onChange={(e) => setProvider(e.target.value)}
          disabled={launching}
          className={`mb-1.5 w-full rounded-lg border border-ink-500 bg-ink-700 px-3 py-2 text-sm text-zinc-200 disabled:opacity-60 ${FOCUS_RING}`}
        >
          {providers.map((p) => (
            <option key={p.name} value={p.name}>
              {providerTitle(p.name)}
            </option>
          ))}
        </select>
      )}
      <p className="mb-4 text-[11px] text-zinc-600">
        The CLI the generator agent runs on — node providers are set in the
        definition it writes.
      </p>

      <Disclosure
        label="Instructions sent to the agent"
        open={previewOpen}
        onToggle={() => setPreviewOpen((o) => !o)}
      >
        <pre className="max-h-48 overflow-y-auto whitespace-pre-wrap break-words font-mono text-[11px] leading-relaxed text-zinc-400">
          {composed}
        </pre>
      </Disclosure>
      <p className="mt-2 text-[11px] text-zinc-600">
        Launches an orchestrator agent with these instructions as its first
        prompt; it authors the workflow via create_workflow.
      </p>
    </>
  );

  return (
    <div
      className="fixed inset-0 z-50 flex items-center justify-center bg-black/50"
      onClick={onClose}
    >
      <div
        onKeyDown={handleKey}
        className="no-drag flex max-h-[85vh] w-[40rem] flex-col rounded-xl border border-ink-500 bg-ink-800 shadow-2xl"
        onClick={(e) => e.stopPropagation()}
      >
        <div className="flex shrink-0 items-center gap-3 border-b border-ink-600 px-5 py-3.5">
          <h2 className="text-sm font-semibold text-zinc-100">New workflow</h2>
          <span className="flex-1" />
          <button
            onClick={onClose}
            aria-label="Close"
            className={`rounded p-1 text-zinc-500 hover:text-zinc-200 ${FOCUS_RING}`}
          >
            <X size={16} />
          </button>
        </div>

        <div className="min-h-0 flex-1 overflow-y-auto p-5">
          {/* Mode tabs */}
          <div className="mb-4 flex rounded-lg border border-ink-500 bg-ink-700 p-0.5">
            {(
              [
                ["json", "Write JSON"],
                ["ai", "Generate with AI"],
              ] as [Mode, string][]
            ).map(([m, label]) => (
              <button
                key={m}
                onClick={() => setMode(m)}
                aria-pressed={mode === m}
                className={`flex-1 rounded-md px-2 py-1 text-xs transition-colors ${
                  mode === m
                    ? "bg-ink-500 text-zinc-100"
                    : "text-zinc-500 hover:text-zinc-300"
                } ${FOCUS_RING}`}
              >
                {label}
              </button>
            ))}
          </div>

          {mode === "json" ? jsonTab : aiTab}
        </div>

        <div className="flex shrink-0 items-center justify-end gap-2 border-t border-ink-600 px-5 py-3">
          {!connected && (
            <span className="mr-auto text-[11px] text-zinc-600">
              daemon unreachable · retrying
            </span>
          )}
          <button
            onClick={onClose}
            className={`rounded-lg px-3 py-1.5 text-sm text-zinc-400 hover:text-zinc-200 ${FOCUS_RING}`}
          >
            Cancel
          </button>
          {mode === "json" ? (
            <button
              onClick={() => void save()}
              disabled={saving || !connected || text.trim().length === 0}
              title={connected ? "Validate and create this workflow" : "Daemon unreachable"}
              className={`flex items-center gap-1.5 rounded-lg bg-primary px-3 py-1.5 text-sm font-medium text-white hover:bg-primary-hover disabled:opacity-50 ${FOCUS_RING}`}
            >
              {saving && <Loader2 size={13} className="animate-spin" />}
              {saving ? "Saving…" : "Save workflow"}
            </button>
          ) : (
            <button
              onClick={() => void launch()}
              disabled={!canLaunch}
              title={connected ? "Launch the generator agent" : "Daemon unreachable"}
              className={`flex items-center gap-1.5 rounded-lg bg-primary px-3 py-1.5 text-sm font-medium text-white hover:bg-primary-hover disabled:opacity-50 ${FOCUS_RING}`}
            >
              {launching && <Loader2 size={13} className="animate-spin" />}
              {launching ? "Launching…" : "Launch generator"}
            </button>
          )}
        </div>
      </div>
    </div>
  );
}
