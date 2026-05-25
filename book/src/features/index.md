# Features

cute-dbt's behavior is specified by **Gherkin `.feature` files** —
executable acceptance criteria that double as product documentation.
Each feature is a self-contained statement of what cute-dbt does and
does not do for a slice of behavior.

The features live under
[`features/`](https://github.com/breezy-bays-labs/cute-dbt/tree/main/features)
in the repo and run as a cucumber-rs BDD outer-loop test
(`cargo test --test bdd`).

## v0.1 feature set

| Feature | What it covers | Source |
|---|---|---|
| **Report generation** | End-to-end: load manifest + baseline, scope-select, parse CTEs, render `report.html`. The vertical slice. | [`report_generation.feature`](https://github.com/breezy-bays-labs/cute-dbt/blob/main/features/report_generation.feature) |
| **CTE rendering** | Per-model CTE DAG: nodes, edges, edge-type vocabulary (`from`/`inner`/`left`/`right`/`full`/`cross`/`union_all`/`union_distinct`), legend rendering. | [`cte_rendering.feature`](https://github.com/breezy-bays-labs/cute-dbt/blob/main/features/cte_rendering.feature) |
| **Diff scoping** | `state:modified.body` selection: which models + tests are in scope for a given current+baseline pair. Empty-scope exit-0. | [`diff_scoping.feature`](https://github.com/breezy-bays-labs/cute-dbt/blob/main/features/diff_scoping.feature) |
| **Fail closed** | Two-stage preflight: schema-level (unreadable, pre-1.8, unusable baseline) and semantic (in-scope unit tests on parse-only models). Non-zero exit + no partial report. | [`fail_closed.feature`](https://github.com/breezy-bays-labs/cute-dbt/blob/main/features/fail_closed.feature) |
| **Zero egress** | The load-bearing privacy property: the generated `report.html` opens via `file://` and makes zero outbound network requests. | [`zero_egress.feature`](https://github.com/breezy-bays-labs/cute-dbt/blob/main/features/zero_egress.feature) |
| **Config** | `--config <PATH>` flag + `AnalysisConfig` TOML for operator-supplied report title + subtitle. | [`config.feature`](https://github.com/breezy-bays-labs/cute-dbt/blob/main/features/config.feature) |

## Why Gherkin

- Each feature is a **product-level contract**, not an engineering
  detail. A scenario reads like a use case ("Given a manifest with
  two modified models, When I run cute-dbt with a baseline, Then the
  report shows two model cards").
- The same file is both the spec and the test. Drift between
  documentation and behavior is impossible — drift breaks CI.
- A new feature lands as a new `.feature` file + step definitions.
  Reviewers see exactly what behavior the PR adds.

## Reading a `.feature` file

A typical scenario:

```gherkin
Scenario: Empty scope produces an exit-0 report with the empty-state banner
  Given a current manifest matching its baseline byte-for-byte
  When I run cute-dbt with --baseline-manifest <baseline> and --out report.html
  Then the exit code is 0
  And report.html exists
  And the diff-scope banner reads "0 unit tests in scope"
```

`Given` sets up the input. `When` runs the tool. `Then` asserts on
the output. Each line corresponds to a step definition in
[`tests/bdd.rs`](https://github.com/breezy-bays-labs/cute-dbt/blob/main/tests/bdd.rs)
that drives the real CLI binary.

## v0.x roadmap (feature-level)

The current six features describe the v0.1 walking skeleton. New
features land as new `.feature` files; the [`feature-count` CI
gate](https://github.com/breezy-bays-labs/cute-dbt/blob/main/.github/workflows/ci.yml)
ensures the count is mirrored locally (in `lefthook.yml`) — drift
between local and CI gates is mechanically caught.

Planned for later v0.x (filed as open issues):

- Richer fixtures exercising the multi-model selector cascade + the
  UNION CTE edge + empty-state cards
  ([#39](https://github.com/breezy-bays-labs/cute-dbt/issues/39)).
- `source()` resolution for non-`ref()`-only manifests
  ([#57](https://github.com/breezy-bays-labs/cute-dbt/issues/57)).
- Sub-selectors for `state:modified.configs` /
  `state:modified.contract` /
  `state:modified.relation` /
  `state:modified.macros` (mentioned as v0.x-future in
  [the v0.1 fidelity-limit note](./how-it-works.md)).
