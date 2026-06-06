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
│                     │    │  • PR unified diff          │
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

### Source 2 — PR unified diff (`--pr-diff`)

For CI / PR review. The workflow runs `git diff --unified=0 <base>...<head>`
and hands cute-dbt the resulting patch (the `@<path>` value-parser reads
the diff from a file — the leading `@` means "read from this file"; an
inline literal also works). cute-dbt parses the diff's `+++ b/<path>`
headers — mapping each changed path to its manifest node via
`original_file_path` — and the per-block `@@ … @@` hunks, which drive
block-precise updated-test detection (below). No baseline to publish or
cache — the diff GitHub already computed *is* the scope signal. cute-dbt
never shells out to `git` or reads the GitHub event itself; the workflow
owns *how* the diff is produced. The [GitHub Actions PR-review
recipe](./recipes/github-actions-pr-review.md) wires this up copy-paste.

### Which scope source?

| | `--baseline-manifest` | `--pr-diff` |
|---|---|---|
| **Use for** | local dev, ad-hoc review | CI / pull-request review |
| **Needs** | a baseline manifest to diff against | the PR's unified diff (`git diff --unified=0`) |
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

How "updated" is derived depends on the scope source. Both are precise,
but they read different inputs:

- **`--baseline-manifest`** — a test is updated when its parsed
  `unit_test` differs from the baseline (a structural `StateComparator`
  diff over the two manifests), independent of which file it lives in.
- **`--pr-diff`** — **block-precise**: a test is updated **iff a changed
  diff hunk overlaps that test's YAML block span**. A changed multi-test
  file no longer marks *every* test it declares as updated — only the
  tests whose block a `@@ … @@` hunk actually touched. The other tests in
  that file stay in scope as **context** (their target model is still
  changed); they're just not flagged as updated.

Two conservative fallbacks keep the block-precise signal honest:

- A pure **deletion** inside a block counts as touching it — the test
  stays updated even though the hunk only removed lines.
- If the supplied diff no longer lines up with the working-tree YAML
  (revision drift — the diff was taken against a different head), **or** a
  block moved but is itself unchanged, cute-dbt **degrades gracefully**:
  it keeps the file-granular "updated" mark for that file and drops the
  inline diff, rather than risk mislabeling a test. The
  [same-revision contract](./recipes/github-actions-pr-review.md) in the
  CI recipe (diff taken `base...head`, manifest compiled at `head`) is
  what keeps the hunks lined up so this fallback rarely fires.

### v0.1 fidelity limits (PR-diff scoping)

`--pr-diff` reads the diff's changed **paths** to pick the in-scope set
and its **hunks** to flag updated tests — but **scope selection stays
path-granular**. So in v0.1:

- A changed `.yml` still brings **every unit test that file declares**
  into scope as **context** (the path can't, on its own, tell which test
  changed body semantics). Block precision then flags only the
  hunk-overlapping tests as **updated**, so the multi-test-file
  false-positive that used to mark every test as updated is gone.
- A `packages.yml` / `dbt deps` change that alters compiled output
  without touching a model `.sql` / `.yml` is not detected — the diff
  carries no path that maps to an affected node.
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
   bracketing rule, plus the full description as authored. For an
   **updated** test under `--pr-diff`, the drawer adds an **Authored ↔
   Diff** toggle and opens on the **Diff** view (its summary reads
   "Authoring YAML — diff"), showing the inline YAML diff of just that
   test's block. When a test has no inline diff — it's a context test,
   the supplied diff is stale, or the change landed outside the test's
   block — the drawer shows the plain authored YAML exactly as before.
   The inline diff is **`--pr-diff`-only**: baseline mode computes
   `changed` from a structural manifest diff with no hunks, so it has no
   inline YAML diff and the drawer always shows the authored YAML.

   Because this drawer's inline diff is a **text** diff over the *whole*
   `- name:` block slice, changes to a test's **`overrides`** block
   (`macros` / `vars` / `env_vars` — where dbt pins `is_incremental`,
   `current_timestamp`, project vars, … to deterministic values) surface
   here like any other line change. An **override-only edit** is the one
   case that *needs* this fallback: it leaves the `given`/`expect` cells
   byte-identical, so the cell-level data-table diff (the Inspect /
   Expected panels, cute-dbt#98) correctly shows **no** change — the YAML
   drawer is the only place the edit is visible. cute-dbt does not parse
   the structured `overrides` payload at all (it is dropped on
   deserialize), and it does not need to: the text-diff fallback reads
   only the working-tree YAML the `--project-root` slice already provides
   (cute-dbt#125).
6. **Model SQL section** — the model's **raw Jinja source** (`raw_code`
   from the manifest — the diffable layer; compiled SQL is generated and
   un-diffable). When the PR diff changed the model's `.sql`, this section
   adds a **Raw ↔ Diff** toggle and opens on the **Diff** view, showing
   the inline diff of the model's SQL (cute-dbt#111). The diff reuses the
   exact line-diff substrate the Authoring YAML drawer uses — same
   change-pair rendering, same intra-line emphasis, same N7b drift guard.
   `raw_code` is read straight from the manifest (no `--project-root`
   filesystem read needed, unlike the YAML drawer), so the SQL diff fires
   on any changed model `.sql`. When there's no SQL diff — baseline mode,
   the model is in scope only via a changed *test*, the diff is stale, or
   the change was whitespace-only — the section shows the plain raw SQL
   exactly as before. dbt engines differ in one detail cute-dbt
   normalizes away: dbt-core ships `raw_code` with the file's trailing
   newline stripped, dbt-fusion ships it byte-identical but keeping the
   trailing newline; cute-dbt strips a single trailing newline so the SQL
   diff renders identically regardless of which engine compiled the
   manifest.

### Whitespace-only changes are ignored (standard)

Both inline diffs (the YAML drawer and the Model SQL diff) **ignore
whitespace-only differences** as standard behavior — no flag, no opt-in.
A re-indentation, a trailing-whitespace edit, or a blank-line churn that
leaves the substantive content unchanged renders as **context**, not as a
change (`git --ignore-all-space` semantics, compared per line-pair). If
*every* change in a block is whitespace-only, cute-dbt emits **no inline
diff** and shows the plain view. The drift guard upstream stays
whitespace-exact (a whitespace divergence between the diff and the
working tree is genuine revision drift, not a no-op), so whitespace
insensitivity applies only to the change/no-change decision, never to the
staleness check.

This is a best-effort filter over git's pre-computed `--unified=0` hunks,
not a re-diff: cute-dbt cannot re-pair lines git already split. The
dominant cases (re-indentation, trailing whitespace, blank-line churn)
are handled; for a multi-line replacement that mixes a re-indent with a
real edit, the removed line may render at the hunk's top rather than
paired to its own offset — the change-set stays correct, only the line
placement is approximate. For YAML, an indentation change that alters
*structure* still surfaces via the accompanying value changes.

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
