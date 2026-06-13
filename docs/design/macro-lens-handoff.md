# Claude Design handoff — the macro-lens ("Macro changed") section

> **Audience.** Claude Design (and any designer/agent) starting the
> macro-lens visual pass (epic
> [#265](https://github.com/breezy-bays-labs/cute-dbt/issues/265),
> founder decision D7 — build it functionally + on-brand, then take it to
> Design for polish). This is the navigation + constraints map for the
> experimental "Macro changed" section of `cute-dbt report`. Every path
> below is clickable and current as of the Slice D merge.
>
> **What this is not.** Not a redesign and not a spec for one — it is the
> map and the fence. Read it before touching the macro-lens template/CSS
> so a redesign respects the load-bearing invariants rather than tripping
> a CI gate on the first push.

---

## 0. The fastest way in (dogfood the live artifacts first)

Two committed, byte-identity-gated goldens render the section — open them
and click around; they are the exact bytes the renderer emits:

- **Cap showcase**: `examples/macro-heavy-report.html` — a synthetic
  macro (`mask_pii`) reaching **14** root-project models with the default
  inline-body cap of **10**, so it exercises the Slice D cap: the
  "showing 10 of 14 model bodies" notice, 10 inlined SQL panels, and 4
  over-cap "body not inlined" affordances.
- **Feature showcase**: `examples/diff-showcase-report.html` — the macro
  body diff + the directory tree of the 3 models reaching
  `quarantine_filter` (all under the cap, so all inline).

Both are committed and byte-identity gated in CI (the `macro-heavy` and
`diff-showcase` rows of `example-report-check` in
`.github/workflows/ci.yml`). Regenerate locally with the macro-lens
experiment on:

```sh
CUTE_DBT_EXPERIMENTAL=1 cargo run --bin cute-dbt -- report \
  --manifest tests/fixtures/macro-heavy-current.json \
  --pr-diff @tests/fixtures/macro-heavy-pr-diff.patch \
  --project-root tests/fixtures/macro-heavy-source \
  --out examples/macro-heavy-report.html
```

---

## 1. Where the section is generated (the map)

### 1.1 The template

`templates/report.html` — the `{% match macro_lens %}` block (the
`macro-lens-panel` section). Server-rendered, positioned **above** the
governance chips. Key sub-structures, each with a stable `data-testid`
hook the headless guards key off:

- `macro-lens-panel` — the section root; `macro-lens-experimental` chip
  (founder D7), `macro-lens-fidelity` chip (`exact` on baseline,
  `heuristic` on pr-diff).
- `macro-lens-diff` / `macro-lens-body` — the changed macro's body diff
  (pr-diff arm) or plain body context lines (baseline arm).
- `macro-lens-tree` — the **always-full** collapsible directory tree of
  impacted models (founder D3; the lightweight surface, critique S3).
- `macro-lens-models` — the model-selector + per-model panels (founder
  D4). **Slice D cap lives here**: `macro-lens-body-cap` (the "showing N
  of M model bodies" notice), each `macro-lens-model-panel` carries
  `data-inline-body="true|false"`, and an over-cap panel renders
  `macro-lens-model-uninlined` instead of the SQL/call-site surface.
- A gated inline `<script>` at the section tail — a **pure renderer** that
  flips panel `hidden` on a `<select>` change. Never a recompute, never a
  fetch.

### 1.2 The renderer

`src/adapters/render.rs`:

- `build_macro_lens(current, changed_macros, scope_source, index, body_cap)`
  → `Option<MacroLensPayload>` — the entry point. `None` ⇒ zero section
  bytes.
- `MacroLensPayload` → `ChangedMacroView` (per changed macro) →
  `ImpactedModelView` (per impacted model) — the POD carriers. The Slice
  D fields: `ChangedMacroView::inlined_count` and
  `ImpactedModelView::inline_body`.
- `impacted_model_views(...)` applies the cap: the first `body_cap` models
  (id order) inline a body; past it the view carries identity only.

### 1.3 The domain + cli

- `src/domain/macro_lens.rs` — `macro_blast_radius` (the reverse
  reachability walk) + `changed_macros_{pr_diff,baseline}`. Pure
  std+serde.
- `src/cli/mod.rs` — `gather_macro_lens` (picks the arm by scope source)
  and `resolve_macro_body_cap` (the gen-time knob resolution: flag >
  `[experimental] macro_body_cap` > `DEFAULT_MACRO_BODY_CAP`).

---

## 2. The fence (invariants a redesign must not trip)

- **Experimental-gated.** The whole section is behind
  `Experiment::MacroLens` (`macro-lens` id). Off ⇒ **zero bytes** ⇒ the
  non-macro goldens (`jaffle-shop`, `playground`) stay byte-identical.
- **Zero-egress.** No new vendored asset, no `src`/`href`. The inline
  `<script>` is a pure renderer over server-rendered panels. The
  headless `file://` network-block test gates this.
- **Honest naming (critique S2).** The copy says **"macro changed"** —
  never a `state:modified.macros` selector name. A guard asserts the
  string is absent.
- **The cap is a gen-time knob (founder D5).** Not a post-render HTML
  toggle — the report is frozen at render. The selector lists every
  impacted model; only the first N inline a body. Don't add a client-side
  "inline more" control that recomputes — that would need a fetch or a
  recompute and break both the zero-egress and frozen-report contracts.
- **No focused DAG, no report→explore link (critique S4).** The focused
  impacted-model DAG is **explore-only** (#345). A report→explore
  hyperlink would break the single-file zero-egress contract.
- **Unstyled beyond the chassis hooks.** The `data-testid` / `data-*`
  hooks are the stable selectors the headless guards and the BDD steps
  key off — keep them when restyling.

---

## 3. The guards that will catch a regression

- **BDD**: `features/macro_perspective.feature` (+
  `tests/steps/macro_perspective.rs`) — section present/absent, the cap
  scenarios (above-cap "N of M" + list-only tail; below-cap all inline).
- **Headless** (`tests/headless_toggle.rs`,
  `macro_lens_*_in_a_real_browser`) — the section + tree + selector + the
  cap affordance render in a real Chromium, including on the **committed**
  `macro-heavy` golden.
- **Render unit tests** (`src/adapters/render.rs`) — the cap math, the
  over-cap affordance, and the worst-case **byte-budget** assertion (a
  200-model macro stays under the budget — the cap's reason to exist).
- **Byte-identity goldens** — the `macro-heavy` + `diff-showcase` matrix
  rows. Restyle ⇒ regenerate both and commit the new bytes.

---

## 4. What's next (not this lane)

The macro **report** lane is functionally complete + experimental +
dogfooded after Slice D. The explorer macro view — the focused DAG, full
`ref()`-lineage downstream, and dagre layout — is
[#345](https://github.com/breezy-bays-labs/cute-dbt/issues/345)
(sub-#99), a separate surface with its own design pass.
