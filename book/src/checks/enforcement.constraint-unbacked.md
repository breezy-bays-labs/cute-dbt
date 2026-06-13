<!-- GENERATED — do not edit. Source of truth: the `heuristics!` block in src/domain/checks.rs. Regenerate: GEN_HEURISTICS_LEDGER=1 cargo test --test heuristics_ledger -->

# enforcement.constraint-unbacked

**Declared constraint without a backing test**

| | |
|---|---|
| Group | `enforcement` |
| Tier | `total` |
| Instrument | `data-test` |

## Conditions

- the model declares a primary_key or unique constraint (model-level constraints[] or a column-level constraint) whose enforcement on the manifest's adapter is metadata-only (NotEnforced) — the warehouse accepts the declaration but does not enforce uniqueness at write time
- no enabled generic uniqueness data test (unique, attached to the model) backs the constrained column — the constraint→test edge is INFERRED by column + test-name match, since the manifest never links a constraint to its test
- the verdict is UNCOVERED only when the constraint is declared, metadata-only, AND has no inferred backing test; a backing test (even one cute-dbt could miss) keeps it silent

## Exclusions

- a constraint the adapter ENFORCES at write time (e.g. not_null / foreign_key on Postgres/DuckDB) is never a gap — the warehouse guarantees it
- a constraint kind with no column-level generic-test backing (check / custom / foreign_key) is out of this inference and never reported here
- the inferred edge can MISS a renamed test or a singular/custom test asserting the same uniqueness — the copy says "backing test" (an authoring-discipline cue), never that the warehouse lacks an index; columns are authored-YAML-only, so this is never a warehouse-truth claim
- the whole `enforcement` group is gated behind the governance experiment — off by default, it never fires on a non-governance report

## Recommendation

Add a uniqueness data test on the declared-but-unenforced constraint column (`unique` for a single column), so the grain the contract DECLARES is actually verified by a test on every run. The warehouse will not enforce it for you on this adapter.

## Rationale

A primary-key / unique constraint that the warehouse treats as metadata-only is a DECLARED guarantee with nothing checking it: duplicate rows load silently, and any downstream join or incremental merge that trusts the declared grain corrupts. A backing data test is the only thing that actually verifies the declared uniqueness on this adapter.
