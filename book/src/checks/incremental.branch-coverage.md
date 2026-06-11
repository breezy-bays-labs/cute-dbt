<!-- GENERATED — do not edit. Source of truth: the `heuristics!` block in src/domain/checks.rs. Regenerate: GEN_HEURISTICS_LEDGER=1 cargo test --test heuristics_ledger -->

# incremental.branch-coverage

**Unexercised is_incremental() branch**

| | |
|---|---|
| Group | `incremental` |
| Tier | `high` |
| Instrument | `unit-test` |

## Conditions

- the model is materialized incremental (config.materialized = "incremental") — its body forks on is_incremental(), and a dbt unit test compiles exactly one side of that fork
- a unit test with overrides.macros.is_incremental = true exercises the incremental branch; an explicit false override OR no override at all exercises the initial full-build branch (dbt compiles is_incremental() as false in unit tests by default)
- branch coverage rolls up per model as none / false-only / true-only / both; only BOTH satisfies the construct, attributing every unit test on the model (each test compiles one side of the fork)
- the HIGH-tier cue boundary (the union.arm-coverage precedent): a test on a branch proves that branch's compiled SQL runs under fixtures — whether the branch's filter/merge semantics are meaningfully asserted, or whether the body's Jinja even calls is_incremental(), is not statically decidable from the manifest, so the recommendation is a cue, never an assertion of a bug

## Exclusions

- models whose materialization is absent, non-string, or anything other than incremental (view, table, ephemeral, custom) emit no finding — when the fork provably cannot exist or cannot be statically known, the check is silent, never misclassifies
- microbatch-strategy models (incremental_strategy = "microbatch", or any non-null event_time config) are OUT of rule #1: dbt replays them through event-time batch windows, not the is_incremental() fork, so true/false override coverage does not describe their semantics — never classified
- a non-boolean is_incremental override collapses to the no-override default at ingestion (cute-dbt#145 tolerant truthiness) and counts toward the full-build branch — the conservative side of the cue

## Recommendation

Add a unit test for each missing is_incremental() branch: one with `overrides: macros: is_incremental: true` (mock the prior model state with a `given: - input: this` entry) to exercise the incremental branch, and one without the override for the initial full build (dbt compiles is_incremental() as false by default). This finding's evidence carries a copy-pasteable unit-test sketch per missing branch.

## Rationale

An incremental model is two programs in one body: the initial full build, and the incremental run that filters on the high-water mark and merges by key. Each unit test compiles only one of them, so a suite living entirely on one branch ships the other untested — exactly where incremental models silently drop, duplicate, or re-process rows.
