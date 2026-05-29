# How it works

A product-focused look at what cute-dbt does to your manifest. (For
the engineering-level view — the hexagonal layering, the
StateComparator strategy, the fail-closed contract — see
[`ARCHITECTURE.md`](https://github.com/breezy-bays-labs/cute-dbt/blob/main/ARCHITECTURE.md)
in the repo.)

## The pipeline

```text
┌─────────────────────┐    ┌─────────────────────────────┐
│ current manifest    │    │ one scope source:           │
│ (your PR's diff)    │    │  • baseline manifest, OR    │
│                     │    │  • PR changed-file list     │
└──────────┬──────────┘    └──────────────┬──────────────┘
           │                              │
           └──────────────┬───────────────┘
                          ▼
              ┌───────────────────┐
              │ Diff-scope        │ ← baseline: state:modified.body
              │ selection         │   pr-diff: changed paths → nodes
              └─────────┬─────────┘
                        ▼
              ┌───────────────────┐
              │ In-scope models   │ ∪ in-scope tests
              └─────────┬─────────┘
                        ▼
              ┌───────────────────┐
              │ Per-model CTE     │ ← sqlparser-rs on compiled SQL
              │ graph extraction  │
              └─────────┬─────────┘
                        ▼
              ┌───────────────────┐
              │ Render            │ ← askama + inlined CSS/JS/Mermaid
              └─────────┬─────────┘
                        ▼
              ┌───────────────────┐
              │ report.html       │ ← one self-contained file
              └───────────────────┘
```

## Diff-scope selection

cute-dbt's first-class workflow is reviewing a **diff**, not the whole
project. You give it the current manifest plus **one scope source**
naming which models the diff touched. There are two.

### Source 1 — baseline manifest (`--baseline-manifest`)

For local dev. cute-dbt diffs the current manifest against a baseline
you supply and selects models whose SQL **body checksum** differs —
`state:modified.body`, the same selector `dbt run --select
state:modified` recognizes. cute-dbt mirrors it.

### Source 2 — PR changed files (`--scope-from-pr-diff`)

For CI / PR review. The workflow computes the PR's changed-file list and
hands it to cute-dbt, which maps each path to its manifest node via
`original_file_path`. No baseline to publish or cache — the diff GitHub
already computed *is* the scope signal. cute-dbt never shells out to
`git` or reads the GitHub event itself; the workflow owns *how* the file
list is produced. The [GitHub Actions PR-review
recipe](./recipes/github-actions-pr-review.md) wires this up copy-paste.

### Which scope source?

| | `--baseline-manifest` | `--scope-from-pr-diff` |
|---|---|---|
| **Use for** | local dev, ad-hoc review | CI / pull-request review |
| **Needs** | a baseline manifest to diff against | the PR's changed-file list |
| **Detects change by** | body-checksum diff (`state:modified.body`) | changed file paths → manifest nodes |
| **Setup cost** | snapshot/publish a baseline | none — reuse GitHub's diff |

Exactly one is required; passing neither or both is a usage error
(exit 2). Whichever you pick, a unit test is in scope if:

- Its **target model** is in the in-scope set, OR
- Its **own definition** changed (a changed test on an unchanged model
  is still in scope).

If both sets are empty, the report is a valid empty-scope report with a
"0 unit tests in scope" banner — exit-0 by design. (Empty scope is
information, not failure.)

### Updated vs context tests

Within the in-scope set, cute-dbt classifies each test as **updated** or
**context**:

- **Updated** — this diff changed the test's own definition (the report
  foregrounds these).
- **Context** — the test is in scope only because its target model is in
  scope; the test itself is unchanged.

The report opens in **Updated only** mode by default, so a reviewer sees
the tests the PR actually touched. A global **Updated only ↔ All tests**
toggle reveals the context tests (and the per-model count tracks the
toggle: the updated count in Updated-only mode, the total in All-tests
mode). When a diff updated *no* tests at all — the common SQL-only PR —
the report opens in All-tests mode so you land on content rather than an
empty view.

How "updated" is derived depends on the scope source, and the two
sources differ in precision:

- **`--baseline-manifest`** — precise. A test is updated when its parsed
  `unit_test` differs from the baseline (a structural diff), independent
  of which file it lives in.
- **`--scope-from-pr-diff`** — file-granular in v0.1. A test is updated
  when its declaring `.yml` appears in the diff, so a changed multi-test
  file marks *every* test it declares as updated. Block-precise PR-diff
  classification (diff-hunk overlap) is tracked as
  [cute-dbt#96](https://github.com/breezy-bays-labs/cute-dbt/issues/96)
  and swaps the precise signal underneath this same report UX with zero
  UI change.

### v0.1 fidelity limits (PR-diff scoping)

`--scope-from-pr-diff` maps changed **file paths** to nodes; it does not
read the *contents* of a diff. So in v0.1:

- A changed `.yml` brings in **every unit test that file declares** —
  the path can't distinguish a one-test edit from a whole-file rewrite.
- A change to only a YAML **config block** (not a `unit_tests:` block)
  isn't path-distinguished from a test change.
- A `packages.yml` / `dbt deps` change that alters compiled output
  without touching a model `.sql` / `.yml` is not detected.
- A **renamed** model shows as deleted-path + added-path; the deleted
  path maps to no current node.

Need any of these? Use `--baseline-manifest`, which compares compiled
bodies directly. The [recipe](./recipes/github-actions-pr-review.md)
carries the same list at its adopter-facing layer.

## CTE graph extraction

For each in-scope model, cute-dbt parses the model's **compiled SQL**
(the `compiled_code` field of the manifest) using
[`sqlparser-rs`](https://github.com/sqlparser-rs/sqlparser-rs) and
extracts:

- The list of CTEs.
- The edges between them, classified by edge type:
  `from`, `inner`, `left`, `right`, `full`, `cross`, `union_all`,
  `union_distinct`.
- A terminal node representing the model's final `SELECT`.
- A node-role classification: `import` (CTEs whose body is a simple
  `SELECT * FROM ref('…')` or `source('…')`), `transform` (everything
  else), `final` (the terminal node).
- For each CTE, the **leaf table refs** in its `FROM` clause — used to
  bind unit-test `given[i]` fixtures to the import-CTE node they
  exercise.

## Render

The askama template walks the per-model payload and emits one
self-contained HTML file. Every asset (CSS, JS, Mermaid library) is
embedded at compile time via `include_str!` and `include_bytes!`;
nothing is loaded from the network at runtime.

The report's privacy property — that `file://` open makes zero
outbound requests — is the **load-bearing trait** of the tool. See
[the zero-egress page](./zero-egress.md) for how this is mechanically
guaranteed and how you can re-verify it.

### Per-test layout

When a reviewer selects a unit test in the rendered report, the
page lays out as:

1. **Header chrome** — model + unit-test selectors.
2. **CTE dependency graph** — the Mermaid panel mapping how the unit
   test's target model composes its CTEs.
3. **Description banner** — the test's `description:` from the
   authoring YAML, surfaced in a clearly-bounded banner directly
   above the inspect/expected substance. Authored descriptions are
   the analytics-engineer's hypothesis statement ("this test asserts
   that the join produces no nulls in the foreign key column"); the
   banner lives here so reviewers read it next to the rows that
   prove it. An empty description suppresses the banner entirely.
4. **Inspect / Expected panels** — the unit test's `given` fixture(s)
   alongside the `expected` rows.
5. **Authoring YAML drawer** — the raw `unit_test` slice from the
   source `.yml` (collapsible, defaults open). The drawer carries
   leading + inside + trailing `#`-comment lines per the slicer's
   bracketing rule, plus the full description as authored.

The description banner started life at the top of the test-selection
section. It moved to between the CTE DAG and the inspect/expected
panels (cute-dbt#74) so reviewers don't scroll back and forth between
hypothesis and proof. The Authoring YAML drawer below remains the
catch-all home for long-form context.

## What cute-dbt is NOT doing

- **Not** running dbt. You compile; cute-dbt reads the artifact.
- **Not** executing SQL. Manifest parsing only.
- **Not** computing rows. Every fixture row + expected row is lifted
  verbatim from the manifest's unit-test payload.
- **Not** running a server. The HTML is static and opens from
  `file://`.
- **Not** doing any telemetry. There is no analytics, no crash
  reporting, no auto-update.
