# What cute-dbt is

A single static, local, single-binary tool. The form factor:

```text
manifest.json  +  one scope source
                  ├─ --baseline-manifest    (local dev)
                  └─ --pr-diff               (CI / PR review)
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

- It does not run dbt for you. You bring a compiled `manifest.json`
  plus one scope source — a baseline manifest (local) or the PR's
  unified diff (`git diff --unified=0`, CI). (See [Getting
  started](./getting-started.md).)
- It does not execute SQL or talk to a warehouse.
- Its **default** in-scope detector is `state:modified.body` —
  body-checksum diffs — so a pure config / contract / relation / macros
  change is not flagged by default. The four sub-selector modifiers
  (`.configs` / `.relation` / `.macros` / `.contract`) now exist as
  additive `StateModifier` impls and compose via
  `StateComparator::with_sub_selectors()`; a CLI flag to opt into the
  wider scope is a follow-up, so the default path stays body-only.
- It does not (yet) export markdown or JSON. v0.1 ships HTML only.

## Scope discipline

cute-dbt is a **PR-review tool**. The first-class workflow:

1. A reviewer opens the PR.
2. CI compiles the PR head and runs cute-dbt, scoping the report to the
   PR's diff via `--pr-diff` (the `git diff --unified=0` output) — no
   baseline-manifest publishing job to maintain. (Locally, you scope the
   same report with a `--baseline-manifest` instead.)
3. The report shows **only the tests in scope for this diff** — with
   their CTE context, fixture data, and expected results, in one HTML
   file that opens offline.

The diff-scope banner names the scope source and the in-scope test
count, so what the report covers is unambiguous. The
[GitHub Actions PR-review recipe](./recipes/github-actions-pr-review.md)
wires this into any dbt repo with a copy-paste workflow.
