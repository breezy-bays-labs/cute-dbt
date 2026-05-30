# Introduction

> **Status: v0.x — unstable.** cute-dbt is available on crates.io from
> `v0.1.0` (`cargo install cute-dbt`).
> v0.x follows Cargo SemVer convention: every minor bump (`0.1 → 0.2`)
> MAY carry breaking changes. v1.0 ships the first stability commitment.

## The problem

Reading dbt unit tests during code review is harder than it should be.

- The test lives in a YAML file. The model lives in `.sql`. The CTE
  structure under test is invisible in the diff.
- A reviewer asking *"what does this `given` row exercise?"* has to
  cross-reference the model body, find the matching CTE, and reason
  about whether the test actually touches the changed code path.
- Diff-scope is ambiguous. A PR that touches three models pulls in
  unit tests for all of them — but which ones actually changed body
  semantics? Which tests are still in scope for *this* diff?

The information is in the manifest. It's just not surfaced anywhere a
reviewer naturally looks.

## What cute-dbt does about it

cute-dbt is a **zero-compute Rust CLI** that reads a dbt
`manifest.json` and emits **one self-contained interactive HTML
report**. Open the file directly from your filesystem — no server,
no warehouse connection, no telemetry, no outbound requests.

For each in-scope unit test, the report renders:

- A header (test name, target model, description).
- **Given** and **Expected** data panels as searchable, sortable
  tables.
- A **CTE dependency DAG** of the target model — a left-to-right
  Mermaid diagram whose edges are colored by edge type
  (`from` / `inner` / `left` / `right` / `full` / `cross` /
  `union_all` / `union_distinct`).
- A diff-scope banner naming the scope source and the in-scope test
  count.

In-scope means **changed by this diff**. cute-dbt learns what changed
in one of two ways:

- **Local dev** — supply a `--baseline-manifest`; cute-dbt diffs the
  current manifest against it and selects models whose **body** changed
  (`state:modified.body` — the `checksum` differs).
- **CI / PR review** — supply `--pr-diff` with the PR's unified diff
  (`git diff --unified=0`); cute-dbt maps the changed paths to manifest
  nodes, with no baseline-manifest publishing job to maintain.

Either way, the report surfaces every unit test that targets an
in-scope model — plus tests whose own definition changed, even if the
target model didn't.

This page is the introduction. The next chapter sketches [what
cute-dbt is at a glance](./the-solution.md); [Getting
started](./getting-started.md) walks the install + first-report path;
and teams wiring cute-dbt into pull-request CI can jump straight to the
[GitHub Actions PR-review recipe](./recipes/github-actions-pr-review.md).
