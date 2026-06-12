# cute-dbt

[![Crates.io](https://img.shields.io/crates/v/cute-dbt.svg)](https://crates.io/crates/cute-dbt)
[![Documentation](https://docs.rs/cute-dbt/badge.svg)](https://docs.rs/cute-dbt)
[![Book](https://img.shields.io/badge/book-mdbook-blue.svg)](https://breezy-bays-labs.github.io/cute-dbt/)
[![CI](https://github.com/breezy-bays-labs/cute-dbt/actions/workflows/ci.yml/badge.svg?branch=main)](https://github.com/breezy-bays-labs/cute-dbt/actions/workflows/ci.yml)
[![License: MIT](https://img.shields.io/badge/License-MIT-yellow.svg)](LICENSE)

> **Product documentation**: [https://breezy-bays-labs.github.io/cute-dbt/](https://breezy-bays-labs.github.io/cute-dbt/) — mdbook with introduction, getting-started, features, examples, and the zero-egress privacy property.

**cute** = **C**TE · **C**ontextualized · **C**LI — **U**nit **T**est **E**xplorer
for **dbt**. (Pick whichever expansion suits the conversation: the headline
feature is the CTE dependency DAG; the value-prop is reading unit tests *in
context*; the form factor is a CLI.)

A zero-compute Rust CLI that parses a dbt `manifest.json` and emits
**one self-contained interactive HTML report** visualizing dbt unit tests
— per test, a header, Given/Expected DataTables panels, and a left-to-right
Mermaid CTE dependency DAG with join-type-colored edges.

Designed for analytics-engineering teams reading unit tests during development
and PR review. cute-dbt is a **static, local, single-binary tool** — it runs
entirely on your machine and your data never leaves it. The generated report
opens directly from the filesystem (`file://`), makes **zero outbound
requests**, and is *trivially auditable*: open it offline with DevTools →
Network and observe nothing.

> **Status: v0.x — unstable.** Available on crates.io from `v0.1.0`
> (`cargo install cute-dbt` installs the `cute-dbt` binary). v0.x follows
> Cargo SemVer convention: every minor bump (`0.1 → 0.2`) MAY carry
> breaking changes (CLI flag renames, output-shape changes, exit-code
> changes); v1.0 ships the first stability commitment. Full release-
> discipline policy in [`AGENTS.md` §Release discipline](AGENTS.md#release-discipline).

## What it shows

For each in-scope dbt unit test (diff-scoped via `state:modified.body` —
PR-review is the first-class use case), the report renders:

- A header (test name, target model, description).
- A **Given** data panel and an **Expected** data panel as searchable,
  sortable [DataTables](https://datatables.net/).
- A **CTE dependency DAG** of the target model — a `graph LR` Mermaid
  diagram whose edges are colored by edge type
  (`from` / `inner` / `left` / `right` / `full` / `cross` / `union_all` /
  `union_distinct`), with an always-visible colorblind-safe legend.

A diff-scope banner names the baseline reference and the in-scope test
count.

**Default scope = `state:modified.body`.** Out of the box cute-dbt
detects model **body** changes (a model's `checksum` differs between the
current and baseline manifests). Pure `.configs`-only / `.contract`-only
/ `.relation`-only / `.macros`-only changes leave the body checksum
identical, so they are **not** detected by the default scope. Opt into
the wider fidelity per run with `--modified-selectors` (cute-dbt#160) —
the tokens match dbt's own `state:modified.<sub>` sub-selector
vocabulary, and the chosen selectors compose **alongside** the always-on
body checksum (dbt's OR union across sub-selectors):

```bash
cute-dbt report --manifest target/current/manifest.json \
         --baseline-manifest target/baseline/manifest.json \
         --modified-selectors configs,relation,macros,contract \
         --out report.html
```

Baseline mode only — `--pr-diff` scopes by changed file paths, never a
`state:modified` comparator, so combining it with `--modified-selectors`
is a usage error rather than a silent no-op.

## Why your data stays on your machine

- **Zero compute.** Parses `manifest.json` only — no DB connection, no
  SQL execution, no warehouse driver. Reads bytes; writes one HTML file.
- **Zero telemetry.** No analytics, no crash reporting, no auto-update.
- **Zero egress (the core privacy property).** All assets — Sakura CSS,
  jQuery, DataTables, Mermaid — are vendored and inlined at compile time.
  The generated report has no `<script src>`, `<link href>`, `<img src>`,
  `@import`, `url()`, or protocol-relative `//` external references. Proven
  by a headless-browser network-block test (the **primary** auditability
  artifact; you can re-run it yourself). See
  [`ARCHITECTURE.md`](ARCHITECTURE.md) and [`SECURITY.md`](SECURITY.md)
  for the full zero-egress story.
- **Fail-closed.** A `dbt parse`-only manifest, a pre-1.8 manifest, an
  unreadable manifest, or an unusable baseline produces a non-zero exit and
  no HTML — *never* a partial report.
- **Synthetic-only fixtures.** Every committed fixture / snapshot /
  `.feature` example in this repo is synthetic or public-demo only — no
  real data, ever.

## How it's diff-scoped

cute-dbt is **PR-review-first**. Pass a current `manifest.json` and a
baseline `manifest.json`; the report covers only the unit tests whose
target model body changed (or whose test definition itself changed).
`--baseline-manifest` is required:

```bash
cute-dbt report --manifest target/current/manifest.json \
         --baseline-manifest target/baseline/manifest.json \
         --out report.html
```

`report` itself stays diff-scoped by design: bounded reports, narrow
fail-closed surface. Bare `cute-dbt` without a verb is a usage error.
For a full-manifest overview there is an experimental `explore` verb —
see [Experimental: the `explore` verb](#experimental-the-explore-verb).

## Experimental: the `explore` verb

> **Experimental** — `explore` may change or be removed in any v0.x
> release. It stays listed and runnable (no gate); every invocation
> prints a one-line notice on stderr. `report` is the stable,
> polished surface.

For a full-manifest overview, use the **`explore`** verb instead of a
diff trick:

```bash
cute-dbt explore --manifest target/manifest.json --out-dir explore/
```

It renders **every** model to two self-contained pages — `explore/dag.html`
(the model-lineage DAG) and `explore/tests.html` (the unit-test index) —
with no baseline, fail-open on uncompiled models (they render as
"not compiled" nodes).

## Known v0.1 fidelity limits

- **Default state-modified scoping is body-only.** The default
  comparator detects model body changes only; a pure `.configs` /
  `.contract` / `.relation` / `.macros` change leaves the model body
  checksum identical and is not flagged by default — note dbt's own bare
  `state:modified` is wider (it ORs every sub-selector together). The
  `--modified-selectors` flag
  ([`cute-dbt#160`](https://github.com/breezy-bays-labs/cute-dbt/issues/160))
  opts a run into the wider fidelity, wiring the four sub-selector
  modifiers
  ([`cute-dbt#17`](https://github.com/breezy-bays-labs/cute-dbt/issues/17),
  fulfilling the originally-tracked
  [`cute-dbt#14`](https://github.com/breezy-bays-labs/cute-dbt/issues/14));
  the default path stays body-only by design. Two residual caveats:
  `.configs` diffs the manifest's *resolved* config dict, which can
  over-report relative to dbt's unrendered-config diff (it never
  under-reports), and dbt's `persisted_descriptions` sub-selector has no
  cute-dbt modifier yet.
- **`source()` givens bind via the dominant authored form only.** The
  given-input parsers accept dbt's serialized single-quoted positional
  form (`source('a', 'b')`, `ref('name')`, whitespace/keyword-case
  tolerant). The engine-valid but rare double-quoted and
  `name=`/`table_name=` keyword-argument variants are deliberately not
  parsed — an unparsed given simply stays unbound (the empty-state
  copy), never an error.

## Import-CTE binding

cute-dbt binds each unit-test `given[].input` to a node in the model's
CTE DAG so the "Node detail" panel can render the fixture rows next to
the compiled SQL for the CTE the fixture mocks. The binding is a
two-pass match against the engine-parsed CTE graph, both passes
case-insensitive:

1. **Name match.** A leaf CTE whose own name equals the `ref()`
   target — the design's sample-data convention (`with stg_orders as
   (select * from {{ ref('stg_orders') }})`). Strict role gate: the
   CTE must classify as `Import` (single-source body), so a transform
   CTE that happens to share a name with the queried `ref()` cannot
   spuriously bind.
2. **Body match.** A leaf CTE (zero incoming edges, not the terminal
   `SELECT`) whose engine-extracted body-leaf table references contain
   the `ref()` target. Pass-2 catches two real shapes:
   - dbt's idiomatic `with source as (select * from
     "db"."schema"."MODEL")` unwrapper, where the CTE name is a
     convention (`source`, `src_*`, …) and the model name lives only
     inside the body.
   - The messy multi-ref shape: one CTE body referencing multiple
     `ref()` targets via `UNION ALL`, `JOIN`, or derived subqueries.
     Every leaf ref the engine extracts is independently bindable
     against a unit test's `given[]`; the report's node-detail panel
     stacks every matching given as its own fixture card on that
     single CTE node. Tracked:
     [`cute-dbt#34`](https://github.com/breezy-bays-labs/cute-dbt/issues/34).

A `given: source('a', 'b')` input takes one extra resolution hop before
the same two-pass match
([`cute-dbt#57`](https://github.com/breezy-bays-labs/cute-dbt/issues/57)):
dbt resolves `{{ source('a', 'b') }}` to the physical relation at
`dbt compile` time, so the compiled SQL never carries the literal
`source(...)` form. cute-dbt therefore resolves the authored
`(source_name, table)` pair against the manifest's top-level `sources`
block, takes the entry's physical `identifier` (falling back to the
last segment of `relation_name`, then to `name` — dbt's identifier
default), strips identifier quoting, and feeds that token through the
identical two-pass match above. The lookup runs on the YAML
`source_name` + `name` (the authored arguments), never on `identifier`
— an overridden identifier still binds. A pair missing from the
`sources` block leaves the given unbound — fail-open, same as an
unresolvable `ref()`; sources need no preflight (they are referenced by
models, never analyzed themselves).

When neither pass matches — and the node is an `Import` CTE — the
node-detail panel surfaces *"no fixture provided — dbt treats
unspecified inputs as empty"*. The same empty-state is the
intentional behaviour when a unit test simply does not declare a
`given[]` for some upstream input (dbt's documented semantics).

## Compiled-SQL fidelity

The per-node compiled-SQL drawer shows the model's `compiled_code`
**exactly as `dbt compile` produced it** — including the user's
indentation, casing, blank lines, and `--` / `/* */` SQL comments.
The CTE engine slices each CTE's source extent from `compiled_code`
via sqlparser's span metadata rather than emitting the AST back through
`Display`, which would drop comments
([`cute-dbt#31`](https://github.com/breezy-bays-labs/cute-dbt/issues/31)).
Jinja `{# #}` comments are stripped at `dbt compile` time and never
reach `compiled_code`.

## Architecture

Single-crate Rust CLI, hexagonal **inward-dependency discipline**.
`src/{domain, ports, adapters, cli}` + `main.rs`. The full architecture
invariants, the two-stage fail-closed contract, and the conscious design
simplifications (no workspace, no public-API shim, no JSON envelope) are in
[`ARCHITECTURE.md`](ARCHITECTURE.md).

## Documentation

- [`ARCHITECTURE.md`](ARCHITECTURE.md) — single-crate hexagonal discipline,
  two-stage fail-closed, StateComparator strategy, zero-egress gate
- [`SECURITY.md`](SECURITY.md) — plain-language zero-egress + privacy
  statement
- [`AGENTS.md`](AGENTS.md) — agent operating notes
- [`CLAUDE.md`](CLAUDE.md) — Claude-specific entry point
- [`CONTRIBUTING.md`](CONTRIBUTING.md) — how to contribute
- [`CHANGELOG.md`](CHANGELOG.md) — release notes (sparse during v0.x)

## License

MIT — see [`LICENSE`](LICENSE).
