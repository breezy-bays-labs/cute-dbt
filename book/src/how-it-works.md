# How it works

A product-focused look at what cute-dbt does to your manifest. (For
the engineering-level view — the hexagonal layering, the
StateComparator strategy, the fail-closed contract — see
[`ARCHITECTURE.md`](https://github.com/breezy-bays-labs/cute-dbt/blob/main/ARCHITECTURE.md)
in the repo.)

## The pipeline

```
┌─────────────────────┐    ┌─────────────────────┐
│ current manifest    │    │ baseline manifest   │
│ (your PR's diff)    │    │ (target branch)     │
└──────────┬──────────┘    └──────────┬──────────┘
           │                          │
           └────────────┬─────────────┘
                        ▼
              ┌───────────────────┐
              │ Diff-scope        │ ← state:modified.body
              │ selection         │   (body checksum diff)
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

## Diff-scope selection (`state:modified.body`)

cute-dbt's first-class workflow is reviewing a **diff**, not the
whole project. The diff-scope selector identifies which models the
current manifest has *meaningfully* changed since the baseline.

In v0.1, "meaningfully changed" = `state:modified.body`: the model's
SQL body checksum differs between current and baseline. This is the
same selector `dbt run --select state:modified` recognizes — cute-dbt
mirrors it.

A unit test is in scope if:

- Its **target model** is in the modified set, OR
- Its **own body** is in the modified set (a changed test on an
  unchanged model is still in scope).

If both sets are empty, the report is a valid empty-scope report with
a "0 unit tests in scope" banner — exit-0 by design. (Empty scope is
information, not failure.)

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

## What cute-dbt is NOT doing

- **Not** running dbt. You compile; cute-dbt reads the artifact.
- **Not** executing SQL. Manifest parsing only.
- **Not** computing rows. Every fixture row + expected row is lifted
  verbatim from the manifest's unit-test payload.
- **Not** running a server. The HTML is static and opens from
  `file://`.
- **Not** doing any telemetry. There is no analytics, no crash
  reporting, no auto-update.
