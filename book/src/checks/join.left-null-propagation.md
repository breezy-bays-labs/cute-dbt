<!-- GENERATED — do not edit. Source of truth: the `heuristics!` block in src/domain/checks.rs. Regenerate: GEN_HEURISTICS_LEDGER=1 cargo test --test heuristics_ledger -->

# join.left-null-propagation

**LEFT JOIN null propagation untested**

| | |
|---|---|
| Group | `join` |
| Tier | `high` |
| Instrument | `both` |

## Conditions

- the model LEFT-JOINs a relation and right-side columns provably reach the containing SELECT's projection: a direct `<right>.<column>` item, a `<right>.*` qualified wildcard, or a bare `*`
- satisfaction: some unit test's literal givens carry a left-side row whose ON equi-key has no match among the right-side given rows — cells compare on the value-normalized equality key, and a left row whose key cell is NULL or absent never matches (SQL join semantics), so it exercises the no-match path
- given binding follows the union.arm-coverage premise: only `given` entries carry statically-visible rows — an ungiven non-seed input is an empty mock; join sides bind to givens directly by external leaf name or through an upstream closure of simple-FROM CTEs reading exactly one external relation
- instrument routing (catalog C4/C10): when the SELECT containing the LEFT JOIN dedups its output with DISTINCT — the dedup-after-fan-out signal — the data-test recommendation wins: prove the right key's grain with a uniqueness data test at the source instead of adding fixtures
- verdict order: any test exercising an unmatched left row makes the construct COVERED with attribution; otherwise any statically-unattributable binding or given makes it UNKNOWN; otherwise UNCOVERED

## Exclusions

- right-side columns reaching the output only through expressions (COALESCE, CASE, function calls) are not attributed — the construct stays silent unless a direct right-qualified item or wildcard projects (conservative: never a false fire)
- non-equi or non-column ON predicates, USING / NATURAL constraints, and unqualified key columns leave the join key statically unrecoverable — verdict UNKNOWN, never UNCOVERED
- a derived-table join side emits no fact; a CTE side whose upstream closure is not a single-external chain of simple-FROM CTEs is UNKNOWN, never UNCOVERED
- external `fixture:` files and non-literal `format: sql` givens make a test statically uncountable — UNKNOWN, never UNCOVERED
- an ungiven seed-side input reads real seed data — UNKNOWN, never UNCOVERED

## Recommendation

Add a no-match given: one left-side row whose join key is absent from the right-side given rows, then extend `expect` with that row carrying NULL right-side columns (or the intended fallback). This finding's evidence carries a copy-pasteable given sketch.

## Rationale

A LEFT JOIN whose right-side columns reach the output propagates NULLs on every unmatched left row — the most common real dbt unit-test catch. When every left given row has a right match, the no-match path runs on zero rows in every test, so an unhandled NULL (or a wrong fallback) ships silently.
