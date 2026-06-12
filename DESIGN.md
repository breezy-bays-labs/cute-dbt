# DESIGN.md — design source of truth & session orientation

Orientation for design sessions (Claude Design or any design agent) that
use **this repository as the source of truth**. Peer of
[`ARCHITECTURE.md`](ARCHITECTURE.md) (system invariants) and
[`AGENTS.md`](AGENTS.md) (agent operating rules): this file maps where the
shipped design system lives, how it has diverged from earlier design
handoffs, and the constraints any new design work must hold.

## The one rule

**Where this repository and a prior design handoff disagree, the
repository wins.** The shipped CSS/JS has been empirically validated
(measured WCAG contrast on every theme, headless-browser interaction
tests, byte-gated golden artifacts) and has deliberately moved past the
handoffs in several places — see the divergence ledger below. Treat the
handoff zips as historical input, not as the current contract.

## Source-of-truth map

| Concern | Lives in | Consumed by |
|---|---|---|
| **Design-system ROOT**: semantic tokens + all 8 `[data-theme]` blocks | `templates/partials/tokens.css` (askama `{% include %}`) | **both** page families (extracted from `report.css` at #242 — exact values, zero changes) |
| Shared page chassis: `[hidden]`, html/body token skin, links, form controls | `templates/partials/base.css` (askama `{% include %}`) | **both** page families |
| Report-specific design layers: style packs, settings chrome, tokenized components, density overrides, reconciliation + coverage layers (incl. the per-theme surface-scoped AA overrides) | `templates/report.css` | report page |
| Report page shell | `templates/report.html` (askama) | `cute-dbt` report output (single self-contained HTML) |
| Report interactivity (selectors, DAG views, diff renderers, fixture grids, settings panel) | `templates/interaction.js` | report page |
| Shared appearance engine: reads/applies `cute-dbt.appearance.v1` (theme/style/density/accent/coverage attributes on `<html>`) | `templates/appearance.js` (`window.CuteAppearance`) | **both** page families |
| Report-only appearance settings UI (settings-panel controls, DataTables reflow, DAG-engine dispatch) — drives the shared engine | `templates/theme.js` | report page |
| Report-page Cytoscape preset layout (first-party, no layout plugin) | `templates/cyto-dag.js` | report page |
| Explore pages (page styles re-expressed on the shared tokens at #242) | `templates/explore-dag.html`, `templates/explore-tests.html` + `templates/explore-lineage.js`, `templates/explore-cte.js`, `templates/explore-tests.js` | `cute-dbt explore` two-page output |
| Shared markup partial (test card: given/expected fixture grids) | `templates/partials/test-card.html` (askama `{% include %}`) | **both** report and explore-tests |
| Vendored frontend bundles + provenance (pin, SHA-256, SPDX) | `assets/` + `assets/MANIFEST.toml` | both page families |
| Rendered, committed reference artifacts | `examples/*.html` (report: jaffle-shop, playground, diff-showcase) and `examples/explore/{dag,tests}.html` | what you should open and look at — these are byte-identity-gated in CI and regenerate on every change |
| Design-system regression guards | `tests/headless_toggle.rs` (real-Chromium: theme application, WCAG contrast pins per surface per theme, tooltip behavior, layout containment — report AND explore since #242) | CI |

The 8 themes: `light` (default `:root`), `dark`, `solarized`, `latte`
(Catppuccin), `rosepine` (Rosé Pine Dawn), `tokyo` (Tokyo Night),
`gruvbox`, `dracula`.

## Divergence ledger — where the repo moved past the handoffs

These shipped after the pass-2 handoff and are **intentional**; do not
"correct" them back toward the handoff:

- **WCAG AA override families.** Per-theme, surface-scoped token
  overrides bring every measured text surface to ≥ 4.5:1: tier chips
  (solarized), suppressed finding rows (opacity dimming was replaced by
  muted-but-AA tokens — composited opacity is not used for text),
  normal-row construct chips (latte/rosepine/solarized). Pattern: a
  scoped override on the surface, never a re-tint of a theme block.
- **The pass-2 spec's own `color-mix(in oklab, var(--accent) 60%, white)`
  tooltip-key value fails AA on latte** (the latte tooltip fill makes
  60% land ≈ 4.0:1). The repo uses a latte-scoped 35% stand-in. Lesson
  encoded below: design output is validated empirically here; claims of
  contrast are re-measured.
- **Tooltip anatomy unification.** The model-name-badge tooltip and the
  column-header tooltip share one anatomy (`.ct-tests` / `.ct-test` /
  `.ct-key` / `.ct-vals` / `.ct-val`) with color-mix accent keys and
  chip-styled values.
- **Edge-aware tooltip positioning.** A first-party JS tagger annotates
  `data-tip-edge="left|right"` from geometry; CSS owns the flip. Bubbles
  carry a `max-width: min(70vw, calc(100vw - 16px))` cap and 13.44px
  body text (see the rem trap below).
- **Expected-panel badges sit on the left**, mirroring the given block
  (`.fixture-view-bar` is always emitted).

## Inviolable constraints for any new design work

1. **Tokens are law.** Design within the existing semantic tokens and the
   8 theme blocks. No new color literals. If a genuinely new semantic
   token is needed, name it and bind it to existing theme values per
   theme — the implementation side decides bindings.
2. **WCAG AA, measured.** Text surfaces ≥ 4.5:1 against the **effective
   backdrop** (the element's own fill composited over its ancestors —
   not the page background). The repo's headless guards enforce this per
   surface per theme; any spec claim of contrast is re-validated
   empirically on integration.
3. **The tooltip contract.** Tooltips are load-bearing affordances:
   focusable trigger (real button), `aria-label` on the trigger,
   `aria-hidden` bubble, reveal on `:hover` **and** `:focus` in pure
   CSS. JS may only annotate geometry (e.g. the `data-tip-edge` tagger);
   it never owns visibility. Native `title` attributes are never the
   mechanism for a primary affordance. Bubbles must contain their
   content (long monospace tokens wrap) and stay within the viewport.
4. **Zero egress.** The generated HTML makes zero outbound requests when
   opened offline via `file://`. No webfonts (system font stacks only),
   no external images, no `@import`/`url()` resource loads, no
   protocol-relative refs. Everything is inlined at compile time;
   vendored bundles are pinned in `assets/MANIFEST.toml`. A
   headless-browser network-block test gates this in CI.
5. **The rem trap.** Sakura sets `html { font-size: 62.5% }` — root is
   **10px**, not 16px. Never size in bare `rem` assuming 16px (the
   13.44px tooltip text exists because `0.78rem` once rendered as
   7.8px). Prefer px or em-relative-to-a-known-px context for fine type.
6. **DAG engine rules.** Mermaid is the static default on the report
   page; Cytoscape (UMD core, canvas-text labels, no HTML-label
   extension, no workers, system fonts) is the opt-in second engine
   there with a **first-party preset layout — no layout plugins**. The
   explore lineage page additionally uses the vendored `cytoscape-dagre`
   (MIT). `cytoscape-elk` is forbidden everywhere (EPL license).
7. **Single-file outputs.** Each generated page is one self-contained
   HTML file; there is no runtime asset directory and no shared CSS file
   at runtime. "Sharing" happens at the askama template layer
   (`{% include %}` partials), not via HTTP.

## Current explore-page state

**The #242 extraction landed**: `templates/report.css` was re-layered
into askama partials — `partials/tokens.css` (the 8 theme blocks +
semantic tokens, exact current values, zero changes) and
`partials/base.css` (the minimal shared page chassis) — included by
**both** page families, plus a minimal shared appearance engine
(`templates/appearance.js`, `window.CuteAppearance`) so the explore
pages honor the saved `cute-dbt.appearance.v1` appearance and theme on
all 8 themes. The explore inline `<style>` blocks now re-express on the
shared tokens (values mapped, not redesigned); headless guards pin
per-theme application + AA on key explore surfaces. **Design sessions
must treat `templates/partials/tokens.css` as the design-system root.**

One deliberate exception: legend/status chips paired to canvas-drawn
colors (the lineage/CTE legend chips, the not-compiled / changed chips)
keep the **fixed** canvas palette — the report's fixed-DAG-palette
posture — so the legend can never desync from what the Cytoscape engines
actually draw; fills/strokes with an exact token twin use the fixed
`--dag-*` tokens, the rest keep their canvas literals.

Remaining gaps (design opportunity):

- The lineage hover tooltip (`.lineage-tooltip`) is a one-off bubble
  that predates — and does not honor — the tooltip contract (its colors
  are tokenized since #242; the contract retrofit is #241 design-pass
  work).
- No settings affordance (theme/density/style-pack picker) exists on
  explore pages — the saved appearance applies read-only. The
  explore-side settings/coverage affordance is issue #219.
- The report's style packs (`html[data-style]`) and density overrides
  are report-only layers; the explore pages apply the attributes (one
  cross-page attribute contract) but ship no rules keyed on them yet.

## What a returned design spec should look like

The pass-2 handoff's layered shape worked well — keep it:

- Layered CSS files (`tokens` / `base` / page chrome) plus reference
  HTML, rather than one monolith.
- State explicitly which existing tokens each new surface consumes.
- Separate **interactive behavior notes** (what JS must do) from static
  styling — JS here is hand-rolled and contract-bound, not framework
  code.
- Include contrast claims per theme, knowing they will be re-measured.
- Flag any place the spec deliberately deviates from current repo
  reality, with rationale — silent deltas are treated as drift and
  audited out.

## Maintenance

Keep this file truthful: a PR that moves a design-system source location
(e.g. the #242 extraction), adds a theme, or changes a constraint above
must update this file in the same PR.
