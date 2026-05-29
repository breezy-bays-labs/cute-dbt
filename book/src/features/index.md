# Features

What cute-dbt does, by capability. Each is backed by an executable
[Gherkin spec](https://github.com/breezy-bays-labs/cute-dbt/tree/main/features)
that doubles as its acceptance test — so the documentation and the
behaviour can't drift.

## Report generation

Reads a compiled `manifest.json` (plus a scope source) and emits one
self-contained `report.html`: a model card per in-scope model, each unit
test rendered with its **Given** / **Expected** data and the model's CTE
dependency graph.

## Diff scoping

Scopes the report to what a change actually touched — via a
`--baseline-manifest` diff (`state:modified.body`) for local dev, or the
PR's changed files (`--scope-from-pr-diff`) for CI / PR review. An empty
scope is a valid exit-0 report, not an error. See
[How it works](../how-it-works.md).

## CTE dependency graph

Parses each in-scope model's compiled SQL and renders its CTE DAG with
join-typed, colour-coded edges (`from` / `inner` / `left` / `right` /
`full` / `cross` / `union_all` / `union_distinct`) and an always-visible
colourblind-safe legend.

## Unit-test fixture rendering

Renders `given` / `expect` fixtures whatever the authored format — dict,
CSV, or raw SQL — into uniform, searchable tables, and surfaces the raw
`unit_test` YAML (comments included) alongside the data.

## Fail-closed safety

A two-stage preflight refuses to emit a partial or misleading report:
schema-level checks (unreadable / pre-1.8 / unusable baseline) and a
semantic check that an in-scope model was actually **compiled** (`dbt
compile`, not `dbt parse`). Every failure is a non-zero exit with a
remediation message.

## Zero egress

The generated report makes **zero** outbound network requests — every
asset is inlined at compile time. This is the load-bearing privacy
property; see [zero egress](../zero-egress.md).

## Configurable report

`--config <PATH>` supplies a custom report title + subtitle via a small
TOML file.
