# Design Philosophies for Dev Tools & Native Desktop Apps, 2025–2026 — Research Report for Taime

## 1. Linear: the craft/quality benchmark

- The Linear Method frames itself as restoring "the lost art of building true quality software"; core moves: opinionated software ("one really good way of doing things"), scope projects down, write issues not user stories, launch and keep launching (https://linear.app/method, https://www.figma.com/blog/the-linear-method-opinionated-software/).
- Karri Saarinen's 10 rules (Figma blog, Mar 2026): commit to quality at leadership level; small craft-oriented teams; no design→dev handoff; spec is "the baseline, not the finish line"; quality ≠ perfection; design with a specific opinion; **reduce scope to increase quality**; values over process; don't use data as a crutch (https://www.figma.com/blog/karri-saarinens-10-rules-for-crafting-products-that-stand-out/).
- 2025 "calmer interface" refresh — the most directly applicable artifact: principle "Don't compete for attention you haven't earned" (sidebar "a few notches dimmer," compact tabs, main content wins); "Structure should be felt not seen" (softened/rounded borders); palette moved from cool blue-grays to **warmer, less saturated grays**; fewer/smaller icons, no colored icon backgrounds; "most of what makes software feel good is what you aren't likely to see" (https://linear.app/now/behind-the-latest-design-refresh).
- Theme engine rebuilt on **LCH color space** (perceptually uniform lightness); whole themes derived from just 3 variables: base color, accent color, contrast (https://linear.app/now/how-we-redesigned-the-linear-ui).

## 2. Vercel Geist

- Principles: simplicity, minimalism, speed, Swiss-typography roots; "precision, clarity, functionality" (https://vercel.com/geist/introduction, https://vercel.com/font).
- Visual signature: restrained type, sharp monospaced numerals, generous whitespace, **single-accent neutrality**, high-contrast accessible color scales; optimized for dashboards/logs/code surfaces — applied to consumer marketing it "reads as cold" (https://www.designsystems.one/design-systems/vercel-geist). Geist Mono was designed specifically for code editors, terminals, and diagrams (https://basement.studio/post/the-birth-of-geist-a-typeface-crafted-for-the-web). Caveat for Taime: Geist is now the *de facto* dev-tool look with "a wave of imitators" — using it verbatim risks generic; differentiate via warmth (Taime's #c8c7c2 off-white) and density choices.

## 3. Raycast

- Three design principles: **fast, simple, delightful**, "in every corner... from big features to tiny details" (https://www.raycast.com/blog/a-fresh-look-and-feel). Founder thesis: "the best interface is no interface" — hotkey → type → enter; UI waits for input and surfaces only needed results (https://www.raycast.com/).
- Raycast 2.0 technical deep dive distilled into 8 tenets (https://github.com/yetone/native-feel-skill, from https://www.raycast.com/blog/a-technical-deep-dive-into-the-new-raycast): adopt the platform (OS blur/materials/scrolling, don't reimplement); **performance through perception** (optimize what users feel); "identity is muscle memory" — the hotkey, command ranking, and verb set ARE the product. Concrete targets: cold start <500ms, no white flash on launch, pre-warmed fonts. Highly relevant to a Tauri app: share code above the WebView, lean on native below it.

## 4. Warp

- 8 product principles incl. "Keep the power, but fix the UI" (keyboard-driven + modern elements), great out-of-the-box defaults instead of plugin hunting, build for speed (Rust, GPU, 60fps on multi-MB logs) (https://www.warp.dev/blog/how-we-design-warp-our-product-philosophy).
- **Block model**: every command+output is a discrete selectable/shareable unit with exit code, duration, timestamp; the BlockList accepts "rich content blocks" — arbitrary UI views interleaved with terminal blocks ("anything that can report a height can sit in the list") (https://www.warp.dev/blog/block-model-behind-warps-agentic-development-environment). This is the strongest existing pattern for mixing agent UI with PTY output.
- Warp 2.0+ (now open source, May 2026) agent management: status panel of all running agents, notifications on completion/needs-help, per-agent permissions (auto-accept diffs, file read, command allow/denylists). Claimed 6–7 hrs/week saved running multiple agents, 95% diff acceptance (https://www.warp.dev/blog/reimagining-coding-agentic-development-environment, https://knightli.com/en/2026/05/07/warpdotdev-warp-open-source-agentic-terminal/).

## 5. Agentic UX: state, parallelism, trust

- Emerged patterns: **Progress Ledger** (real-time collapsible timeline: Thinking → Searching → Drafting → Waiting for approval); **confidence signals** (review low-confidence work harder); **sandbox preview** before approve (https://www.eleken.co/blog-posts/agentic-ux-examples, https://fuselabcreative.com/ui-design-for-ai-agents/). NN/g State of UX 2026: trust is the central AI design challenge; "users grant autonomy only to systems they understand" — explainability is now design material.
- Claude Code rendering conventions: "Thinking for Ns" counter ticking live; elapsed counter appears at 5s; thinking collapsed by default; interruptions as colored-border cards; ACP streams render as discrete blocks (text delta, tool_call, agent_thought) not flat text (https://fazm.ai/t/watch-claude-code-desktop-agent-ui).
- Multi-agent orchestration (Addy Osmani, 2026): optimal 3–5 parallel agents; shared task list with 4 states (pending/in_progress/completed/blocked) + auto-unblocking; peer-to-peer agent messaging avoids lead bottleneck; **"the bottleneck is no longer generation, it's verification"** — dedicated @reviewer agent as quality gate; hooks (task done → run tests → fail keeps agent working); kill threshold at 3 stuck iterations; 5–10 min check-in cadence (https://addyosmani.com/blog/code-agent-orchestra/).
- Conductor (closest competitor): per-agent isolated workspace (branch, files, chat, terminal, preview, reviewable diff); all threads visible simultaneously with progress/diffs/test results; checkpoints for rollback; multi-model comparison tabs; praised as "most polished" precisely because layout "reduces cognitive overhead — no switching between terminal windows" (https://thenewstack.io/a-hands-on-review-of-conductor-an-ai-parallel-runner-app/, https://www.conductor.build/).
- Diff review trust: PR/diff size is the #1 quality factor (>500 lines overwhelms humans and AI); suggestion dismissal rate >30% erodes trust, target <20%; Graphite wins on "lowest-noise, highest-trust" via tracked acceptance rates (https://getspinal.com/blog/ai-code-review-tools-compared, https://graphite.com/guides/how-ai-code-review-works).

## 6. Dark-UI craft & density vs calm

- 2025 consensus: dark grays (#121212–#1C1C1C) not pure black; soft off-white text (#E0E0E0–#F5F5F5) not pure white; hierarchy via **layered luminance** (base/raised/hover surfaces) not borders everywhere; muted text, limited high-saturation accents; semantic tokens by meaning not hex (https://www.boundev.ai/blog/dark-ui-design-principles, https://natebal.com/best-practices-for-dark-mode/).
- Calm technology (Weiser/Brown) applied to agent swarms: low-urgency info moves to periphery as ambient/glanceable indicators; "surface the tiniest cue that allows users to act" (https://calmtech.com/, https://edges.ideo.com/posts/the-ambient-revolution-why-calm-technology-matters-more-in-the-age-of-ai).
- Motion: 100–500ms total range; ~100ms for simple state feedback; purposeful movement only; respect prefers-reduced-motion (https://www.nngroup.com/articles/animation-duration/).

## 7. Premium vs "AI-generated generic"

- "AI slop" markers to avoid: purple-cyan gradients, Inter-everywhere, glossy card grids, floating 3D blobs, glassmorphism-by-default (https://mcpmarket.com/tools/skills/design-anti-slop, https://www.theadpharm.com/insights/claude-design-without-the-ai-slop-look).
- Countertrend Taime sits inside: **"Technical Mono" / code brutalism** — monospace type, command-line simplicity, high contrast, "intentional incompleteness": UI that "doesn't decorate data or disguise structure" — schematic, brutally clear layouts; predicted to influence mainstream systems by 2026 (https://blog.tubikstudio.com/ui-design-trends-2026/, https://aigoodies.beehiiv.com/p/aesthetics-2026).

## Ranked actionable principles for Taime

1. **Verification is the product.** Design the diff-review surface as the hero; keep reviewable units small (<500 lines), show per-agent attribution, track your own accept/dismiss rates.
2. **Block model for agent output**: each tool call/command = a discrete unit with status, duration, exit code; interleave rich UI blocks with PTY output (Warp).
3. **Calm periphery, loud center**: agent grid as ambient glanceable state (color-coded dots/ledgers); only "needs attention" items earn saturation or motion. "Don't compete for attention you haven't earned" (Linear).
4. **Progress Ledger per agent**: live collapsible timeline (Thinking 12s → Editing src/x.rs → Tests running), thinking collapsed by default, elapsed counters ticking.
5. **Hierarchy via luminance layers, not borders**; warm grays (echoes Taime's #c8c7c2), one accent (blue), LCH-derived tokens from ~3 base variables.
6. **Keyboard identity = muscle memory**: hotkey + command palette + a consistent verb set are the product (Raycast); every mouse path needs a key path.
7. **Opinionated defaults, reduced scope**: one good way to run a multi-agent workflow, fewer features executed excellently (Linear method).
8. **Performance through perception**: <500ms cold start, no flash, instant input echo; GPU-smooth scrollback (Warp's bar: 60fps on huge logs).
9. **Structure felt, not seen**: soften separators; let monospace grid alignment do layout work (Geist/Swiss).
10. **Approval gates as first-class UI**: plan-before-code approval, sandbox preview, per-agent permission scopes visible at a glance.
11. **Motion restraint**: 100–200ms state feedback only; status changes animate, decoration doesn't; honor reduced-motion.
12. **Notifications on completion/needs-help only** (Warp/Conductor pattern); never interrupt for in-progress chatter.
13. **Lean into Technical Mono deliberately**: schematic, data-forward, no gradients/glass — this is both on-trend and the strongest anti-"AI-slop" signal.
14. **Differentiate from the Geist clone wave** via warmth, density tuning, and one signature element (e.g., the attribution/ledger view) rather than novel chrome.
15. **3–5 visible parallel agents is the sweet spot**; design the default layout for that count, with overflow demoted to a list.