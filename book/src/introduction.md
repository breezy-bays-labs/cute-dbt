# Introduction

> **Status: v0.x — unstable.** cute-dbt is available on crates.io from
> `v0.1.0` (`cargo install cute4dbt` installs the `cute-dbt` binary).
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
- A diff-scope banner naming the baseline and the in-scope test count.

In-scope = `state:modified.body`. cute-dbt diffs the current manifest
against a baseline manifest you supply, identifies models whose
**body** changed (the `checksum` differs), and surfaces every unit
test that targets those models — plus tests whose own body changed,
even if the target model didn't.

This page is the [introduction]. The next chapter sketches [what
cute-dbt is at a glance](./the-solution.md); after that, [Getting
started](./getting-started.md) walks the install + first-report path.
