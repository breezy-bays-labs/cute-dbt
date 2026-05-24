# cute-dbt

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

> **Status: v0.x — pre-release.** Public repo for free GitHub Actions and
> agent reviews; **no crates.io publish, no GitHub Release tarballs, no
> `cargo install` path** until v1.0. Tags during v0.x exist for git pinning
> only and do not trigger any workflow.

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

**v0.1 scope = `state:modified.body`.** cute-dbt detects model **body**
changes (a model's `checksum` differs between the current and baseline
manifests). Pure `.configs`-only / `.contract`-only / `.relation`-only /
`.macros`-only changes leave the body checksum identical, so they are
**not** detected in v0.1 — a documented, named limit, not a defect; the
missing sub-selectors arrive as additive `impl StateModifier` blocks in
v0.x.

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
cute-dbt --manifest target/current/manifest.json \
         --baseline-manifest target/baseline/manifest.json \
         --out report.html
```

A full-manifest overview is a documented trick: diff against an empty/
genesis baseline. There is no implicit "full manifest" path — keeping
diff-scoping the default keeps reports bounded and the fail-closed surface
narrow.

## Known v0.1 fidelity limits

- **Body-only state-modified scoping.** Pure `.configs` / `.contract` /
  `.relation` / `.macros` changes leave the model body checksum
  identical and are **not** detected in v0.1 — a documented limit, not
  a defect. Tracked:
  [`cute-dbt#14`](https://github.com/breezy-bays-labs/cute-dbt/issues/14)
  → sub-selectors land as additive `impl StateModifier` blocks in v0.2+
  ([`cute-dbt#15`](https://github.com/breezy-bays-labs/cute-dbt/issues/15)).

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
