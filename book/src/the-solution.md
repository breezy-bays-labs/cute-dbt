# What cute-dbt is

A single static, local, single-binary tool. The form factor:

```
manifest.json + baseline-manifest.json
              │
              ▼
         cute-dbt
              │
              ▼
         report.html  ← open via file://
```

## Three properties that matter

- **Zero compute.** Parses `manifest.json` only. No DB connection,
  no SQL execution, no warehouse driver. Reads bytes; writes one HTML
  file. The tool is fast (≪1s on real manifests) because there's
  no work to do beyond reading + rendering.
- **Zero telemetry.** No analytics, no crash reporting, no
  auto-update.
- **Zero egress.** All assets — Sakura CSS, jQuery, DataTables,
  Mermaid — are vendored and inlined at compile time. The generated
  report has no `<script src>`, `<link href>`, `<img src>`,
  CSS `@import` / `url()`, or protocol-relative `//` external
  references. Proven by a headless-browser network-block test —
  re-runnable by anyone with the repo checked out. See
  [the privacy property page](./zero-egress.md) for the audit
  details.

## What you get per in-scope unit test

A model card with one section per unit test on that model. Each
section:

- Test name, target model, optional description.
- Tags, meta, defined-in path (when present in the manifest).
- A **Given** panel per `given[i]` fixture — bound to its import-CTE
  node when the engine matches `ref('…')` to a CTE.
- An **Expected** panel for the `expect` block.
- The model's **CTE dependency DAG** — a Mermaid `graph LR` with
  join-colored edges + an always-visible colorblind-safe legend.

## What it does NOT do

- It does not run dbt for you. You bring `manifest.json` and a
  baseline. (See [Getting started](./getting-started.md) for ways to
  produce both.)
- It does not execute SQL or talk to a warehouse.
- It does not detect every kind of model change in v0.x. Today's
  in-scope detector is `state:modified.body` — body-checksum diffs.
  Pure config / contract / relation / macros changes are not
  detected in v0.1 (a documented v0.x limit; sub-selectors arrive
  as additive `StateModifier` impls in later v0.x minors).
- It does not (yet) export markdown or JSON. v0.1 ships HTML only.

## Scope discipline

cute-dbt is a **PR-review tool**. The first-class workflow is:

1. A reviewer opens the PR.
2. A baseline manifest is available (the target branch's compiled
   manifest, or a cached snapshot).
3. cute-dbt runs against the PR's compiled manifest + baseline.
4. The report shows the reviewer **only the tests in scope for this
   diff** — with their CTE context, fixture data, and expected
   results, in one HTML file they can open offline.

The diff-scope banner makes it unambiguous which baseline the
report is against and how many tests are in scope.
