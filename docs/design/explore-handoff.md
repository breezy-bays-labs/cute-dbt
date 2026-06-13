# Claude Design handoff — the `explore` surface

> **Audience.** Claude Design (and any designer/agent) starting the
> explore design overhaul ([#241](https://github.com/breezy-bays-labs/cute-dbt/issues/241)).
> This is the navigation + constraints map for the `explore` verb's HTML
> output. It is grounded in the real tree — every path below is clickable
> and current as of cute-dbt#318.
>
> **What this is not.** Not a redesign and not a spec for one — it is the
> map and the fence. Read it before touching any explore template/CSS so
> a redesign respects the load-bearing invariants rather than tripping a
> CI gate on the first push.

---

## 0. The fastest way in (dogfood the live artifact first)

Before reading source, open the **published goldens** and click around —
they are the exact bytes the renderer emits:

- Lineage DAG: `examples/explore/dag.html`
- Unit-test index: `examples/explore/tests.html`

Both are committed and byte-identity gated in CI (the `explore` rows of
`example-report-check` in `.github/workflows/ci.yml`), rendered from the
synthetic `tests/fixtures/playground-current.json` fixture. As of #318
they publish to GitHub Pages under `/examples/explore/` (book → **Reference
→ Examples → Explore examples**), so they are clickable on the published
book at any time. Regenerate locally with:

```sh
cargo run --bin cute-dbt -- explore \
  --manifest tests/fixtures/playground-current.json \
  --out-dir examples/explore
```

(no baseline, no scope source — `explore` is full-manifest by design).

---

## 1. Where the HTML is generated (the map)

### 1.1 The renderer

`src/adapters/explore.rs` — the two-page renderer. Entry point:
`render_explore(out_dir, current, models, changed, payload)` (≈ line 1070).
It writes **`dag.html` then `tests.html`** into `--out-dir`. Key seams:

- It reuses the report's `ReportPayload` from a single `build_payload()`
  (`src/adapters/render.rs`) — the same JSON carrier the report embeds.
  This is the verified reuse seam: a payload schema change propagates to
  both page families atomically.
- `build_lineage_payload(...)` assembles the typed-node DAG carrier
  (`LineagePayload` → `LineageNodePayload`): every `model` plus, since
  cute-dbt#253, typed `snapshot` / `seed` / `source` / `exposure` nodes
  (`LineageNodeType`, ≈ line 96), with **forward** dependency edges only
  (the client traverses both directions).
- `explore_models(...)` builds the server-rendered per-model sections for
  `tests.html` (`ExploreModel` / `ExploreTest` PODs).
- **Fail-open contract**: an uncompiled model (`compiled_code: null`)
  renders as a "not compiled" node/badge on both pages. `explore` never
  raises the report's Stage-2 `NotCompiled`; `PreflightError` keeps its
  four variants. A redesign must preserve a visible "not compiled" state
  on both pages.

### 1.2 The askama templates (`templates/`)

| File | Role |
|---|---|
| `templates/explore-dag.html` | `dag.html` chrome — search combobox, focusable canvas host, legend, model-detail card, CTE⇄model view toggle. The page `<style>` blocks live here (≈ lines 22–90). |
| `templates/explore-tests.html` | `tests.html` chrome — per-model index + the shared test-card viewer. Page `<style>` ≈ lines 22–60. |
| `templates/partials/tokens.css` | **The design-system ROOT** (≈ 23 KB): the semantic token layer + all eight `[data-theme]` blocks. Extracted from `report.css` at cute-dbt#242, **identical bytes** to the report's include. Shared verbatim by both page families via askama `{% include %}`. |
| `templates/partials/base.css` | The minimal cross-family page chassis (token-consuming page-level rules both families share). |
| `templates/partials/test-card.html` | The one shared test-card DOM partial — identical markup in `report.html` and `explore-tests.html` (proves the partial mechanism). |
| `templates/explore-lineage.js` | The first-party lineage engine: Cytoscape init, dagre layout, pan/zoom/drag, fuzzy search, click/Space highlight, the external-drive commit. |
| `templates/explore-cte.js` | The CTE⇄model view toggle's per-model CTE DAG engine (rides the same Cytoscape core). |
| `templates/explore-tests.js` | The `tests.html` unit-test viewer engine. |
| `templates/appearance.js` | The **shared appearance engine** (cute-dbt#242): reads `cute-dbt.appearance.v1` from localStorage and applies the html-level hooks the token partial keys on (`[data-theme]`, `[data-style]`, `[data-density]`, accent overrides). Embedded into BOTH families. On explore it applies the saved appearance **read-only** (no settings UI — that's #219's lane). |

The templates embed every asset inline via askama interpolation with the
`|safe` filter — e.g. `templates/explore-dag.html` lines 222–262 emit the
JSON carrier + `appearance.js` + Cytoscape core + cytoscape-dagre + the
explore engines, each in a plain `<script>` tag (never `type="module"`).

### 1.3 Where assets live

`assets/` holds the vendored frontend bundle; `assets/MANIFEST.toml` is the
provenance index (pinned version + canonical source URL + SHA-256 + SPDX
license per asset). Integrity is double-gated: `tests/assets_manifest.rs`
(a `cargo test`) + the `assets-manifest-gate` CI job. The explore pages
consume:

- `sakura-1.5.0.css` — the classless base stylesheet (MIT).
- `cytoscape-3.30.2.min.js` — UMD core, MIT (shared with the report's DAG
  picker).
- `cytoscape-dagre-4.0.0.min.js` — the **explore-only** L-R rank layout
  extension, MIT (dagre + graphlib bundled internally; `sourceMappingURL`
  trailer stripped at vendoring). Never loaded by the report page.

Each is wired into Rust as a `&'static str` constant in
`src/adapters/asset_embed.rs` via `include_str!` (e.g. `SAKURA_CSS`,
`CYTOSCAPE_JS`, `CYTOSCAPE_DAGRE_JS`, `APPEARANCE_JS`, `EXPLORE_LINEAGE_JS`).
**There is no runtime asset directory and no `--assets-dir` flag** — inline
interpolation is the only path bytes reach the page.

---

## 2. Report-vs-explore rendering paths (what's shared, what diverges)

| | **Report** (`report.html`) | **Explore** (`dag.html` + `tests.html`) |
|---|---|---|
| Renderer | `src/adapters/render.rs` | `src/adapters/explore.rs` |
| Pages | one self-contained HTML | **two** pages, cross-linked (`dag.html` ⇄ `tests.html`) |
| Scope | baseline XOR `--pr-diff` | full-manifest, no scope source |
| Payload | `build_payload()` → `ReportPayload` | **same** `build_payload()` → `ReportPayload` (reuse seam) |
| Token layer | `partials/tokens.css` + `partials/base.css` + report-only remainder (`asset_embed::REPORT_CSS`) | `partials/tokens.css` + `partials/base.css` + small page-local `<style>` |
| Appearance | `appearance.js` (engine) + `theme.js` (settings UI) | `appearance.js` **read-only** (no settings UI) |
| DAG engine | Mermaid (default) ⇄ Cytoscape picker; Cytoscape uses a **first-party preset** layout (`templates/cyto-dag.js`), **no** layout plugin | Cytoscape core + **`cytoscape-dagre`** L-R layout extension |
| Tables | DataTables + jQuery | none (server-rendered static) |

The shared mechanism (askama partials + `ReportPayload` + `appearance.js`)
works and is in place. The remaining divergence is the **component layer**
and the **explore-side settings affordance** — see §4.

---

## 3. HARD constraints any redesign must respect

These are not preferences — each has a CI gate or a license boundary. A
push that violates one fails the PR.

### 3.1 Zero-egress (the core privacy property)

- **Headless network-block gate**: a headless browser opens the generated
  page over real `file://` with all network access denied and asserts
  **zero requests**. Re-runnable by anyone with the repo. Each explore page
  passes it independently.
- **Resource-ref lint** (the structured secondary): rejects real loading
  constructs — `<script src>`, `<link href>`, `<img src>`, CSS `@import` /
  `url()`, protocol-relative `//`. The ONLY hrefs allowed are the
  same-directory nav anchors (`dag.html` ⇄ `tests.html`) and the favicon
  `data:` URI. **No new external asset, font fetch, CDN, or web font** —
  ever. (System font stacks only.)

### 3.2 Asset-embedding contract

- Every vendored asset is embedded at compile time via `include_str!` /
  `include_bytes!` into the binary's `.rodata`, emitted through askama
  with `|safe`. **Never** a runtime asset directory.
- **UMD only, never ESM** (no `<script type="module">`). Mermaid and
  Cytoscape are the UMD builds; this is non-negotiable.
- Any new asset must land in `assets/` **with** a `MANIFEST.toml` entry
  (pin + SHA-256 + permissive SPDX license) or the `assets-manifest-gate`
  + `tests/assets_manifest.rs` fail. Practically: prefer **re-skinning with
  the existing token layer** over adding any asset.

### 3.3 The Cytoscape init contract (explore variant)

Shared clauses (both report + explore): UMD core only, **canvas-text labels**
(no HTML-label extension — `cytoscape-node-html-label` is forbidden;
`templates/explore-lineage.js` ≈ line 47), non-webfont **system `fontFamily`**,
no workers, handlers bound from our JS, per-click interaction **mutates
classes in place** (never re-calls a render entry point).

Explore-specific: the page pairs the pinned Cytoscape core with the vendored
**`cytoscape-dagre`** UMD extension for the **left-to-right** lineage layout
(`{ name: "dagre", rankDir: "LR", ... }`, `templates/explore-lineage.js`
≈ line 316; dagre runs **in-thread**, no workers). **`cytoscape-elk` stays
forbidden everywhere** (EPL license). A redesign may restyle nodes/edges via
our JS + tokens, but must not swap the layout engine or introduce a plugin.

### 3.4 The two-page output shape + the external-drive contract

- The output is **two pages**, not one. `dag.html` and `tests.html` are
  cross-linked and each is independently zero-egress.
- `dag.html` is an **external-drive surface** (`docs`/book:
  `book/src/explore-contract.md`; const `EXPLORE_CONTRACT_VERSION` in
  `explore.rs` ≈ line 79). It carries `data-cute-dbt-contract`, the
  `window.cuteDbtContract` global, the `window.focusModel` / `window.setView`
  hooks, the Space-only `data-selected-model` attribute, and a host-bridge
  `postMessage` commit. **A redesign must not break this surface** — the VS
  Code extension (#210) consumes it. A breaking change to a named surface is
  a contract-version bump (a v0.x-minor / v1.0+-major event), not a silent
  edit.

### 3.5 Synthetic-only goldens

The committed `examples/explore/{dag,tests}.html` are byte-identity gated.
If a redesign changes the output, **regenerate the goldens in the same PR**
(from `tests/fixtures/playground-current.json`) — and never render a golden
from the real `dbt-project/` (it would bake `metadata.root_path`, a
home/runner absolute path, into the HTML).

---

## 4. The #241 design gap (what the overhaul is for)

[#241](https://github.com/breezy-bays-labs/cute-dbt/issues/241) is the
design decision; this is the current-state read so the overhaul starts from
reality, not the issue's pre-#242 snapshot.

**Already closed by cute-dbt#242 (the seam now exists):**

- The semantic token layer + all eight `[data-theme]` blocks were extracted
  to `templates/partials/tokens.css` and are now included **verbatim** by
  both page families. Explore pages are no longer hardcoded light-mode —
  they react to all eight themes.
- `templates/appearance.js` is shared: explore pages now honor the saved
  `cute-dbt.appearance.v1` appearance (theme / style / density / accent),
  **read-only**.
- The page-local explore `<style>` blocks were re-expressed on the shared
  tokens (values mapped, not redesigned).

**Still open (the actual overhaul surface):**

1. **No explore-side settings affordance** — explore applies the saved
   appearance but offers no UI to change it (the user must set it on the
   report first). That's [#219](https://github.com/breezy-bays-labs/cute-dbt/issues/219)'s
   lane; the overhaul should decide explore's affordance.
2. **Component-layer parity** — the report's tokenized component layer (the
   report-only remainder of `report.css` via `asset_embed::REPORT_CSS`) is
   richer than explore's page-local styles. The shared test-card partial
   renders identically but is styled by the report's full chassis vs
   explore's lighter page-local rules. The overhaul should decide which
   report components explore adopts (and extract them into shared partials
   where they belong).
3. **Tooltip contract** — explore-dag's hover tooltip predates the report's
   tooltip contract (#146/#161 focusable + CSS-reveal, #232 edge-flip, #233
   anatomy). The overhaul should bring explore tooltips onto that contract.
4. **No mechanical CSS drift detection** between the two families beyond the
   shared partials (structure > CI: the more shared partials, the less drift
   is possible).

The #241 sequencing note: the tokens-layer extraction is exactly the seam
evidence the #209 restructure ADR wants (the explorer proving the seams
before the workspace split). The overhaul rides immediately before or folds
into that restructure.

---

## 5. Where to start / what to navigate (the checklist)

1. **Open the live goldens** (`examples/explore/dag.html` + `tests.html`,
   or the published book's Explore examples) and dogfood the current state.
2. **Read** `templates/explore-dag.html` + `templates/explore-tests.html`
   (page chrome + the page-local `<style>` blocks — the redesign surface).
3. **Read** `templates/partials/tokens.css` (the token vocabulary the
   redesign must re-skin onto) + `templates/partials/base.css`.
4. **Read** `src/adapters/explore.rs` `render_explore` (≈ line 1070) +
   `build_lineage_payload` to see what data is available per node.
5. **Read** `templates/explore-lineage.js` for the Cytoscape/dagre init +
   the highlight/commit interaction the design styles against.
6. **Cross-check** `assets/MANIFEST.toml` before reaching for any new asset
   (you almost certainly don't need one — re-skin with tokens).
7. **Re-read §3** of this doc before the first push so the redesign lands
   inside the gates, not against them.

Run the gates locally before pushing: `cargo nextest run`,
`cargo clippy --all-targets --locked -- -D warnings`, the headless
zero-egress test, and regenerate + byte-check the `examples/explore/`
goldens.
