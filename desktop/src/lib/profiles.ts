import {
  Bot,
  Box,
  Bug,
  Code2,
  Network,
  Shield,
  type LucideIcon,
} from "lucide-react";
import type { AgentProfileInfo } from "../api";

/** Display meta for an agent profile: icon + label + one-line description.
 *  THE single profile-presentation map (launcher grid, settings, palette) —
 *  import this; never re-declare a local copy. */
export interface ProfileMeta {
  icon: LucideIcon;
  label: string;
  desc: string;
}

export const PROFILE_META: Record<string, ProfileMeta> = {
  orchestrator: {
    icon: Network,
    label: "Orchestrator",
    desc: "Coordinates specialist agents",
  },
  "feature-builder": {
    icon: Code2,
    label: "Feature builder",
    desc: "Implements features",
  },
  "bug-fixer": {
    icon: Bug,
    label: "Bug fixer",
    desc: "Diagnoses failures, writes fixes",
  },
  "security-reviewer": {
    icon: Shield,
    label: "Security reviewer",
    desc: "Audits for vulnerabilities",
  },
  "product-builder": {
    icon: Box,
    label: "Product builder",
    desc: "Scaffolds full features",
  },
  default: {
    icon: Bot,
    label: "Default",
    desc: "General purpose",
  },
};

/** Canonical display order: known profiles in this order, customs after. */
export const PROFILE_ORDER = Object.keys(PROFILE_META);

/** Meta for ANY profile name. Custom profiles (`~/.taime/agents/*.toml`) fall
 *  back to the daemon-reported description and the generic agent icon. */
export function profileMeta(
  name: string,
  daemonDescription?: string,
): ProfileMeta {
  return (
    PROFILE_META[name] ?? {
      icon: Bot,
      label: name,
      desc: daemonDescription?.trim() || "Custom profile",
    }
  );
}

/** Display fallback while the daemon is unreachable — an exact mirror of the
 *  daemon's `builtins()` (taime-session-daemon profiles.rs), which are always
 *  present daemon-side. Live daemon data takes precedence whenever reachable;
 *  drift only shows while the daemon is down. */
export const BUILTIN_PROFILES: AgentProfileInfo[] = [
  { name: "default", description: "Plain agent — no orchestration tools.", source: "builtin" },
  {
    name: "orchestrator",
    description: "Team lead — plans and delegates to specialist workers.",
    source: "builtin",
  },
  {
    name: "product-builder",
    description: "Builds a new product/project from scratch.",
    source: "builtin",
  },
  {
    name: "feature-builder",
    description: "Adds a feature to an existing codebase.",
    source: "builtin",
  },
  {
    name: "bug-fixer",
    description: "Reproduces and fixes a bug with a regression test.",
    source: "builtin",
  },
  {
    name: "security-reviewer",
    description: "Reviews code for security vulnerabilities.",
    source: "builtin",
  },
];
