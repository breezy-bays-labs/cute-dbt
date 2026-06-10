<!-- GENERATED — do not edit. Source of truth: the `heuristics!` block in src/domain/checks.rs. Regenerate: GEN_HEURISTICS_LEDGER=1 cargo test --test heuristics_ledger -->

# grain.unique-key-unbacked

**Unique key without a uniqueness test**

| | |
|---|---|
| Group | `grain` |
| Tier | `total` |
| Instrument | `data-test` |

## Conditions

- the model declares config.unique_key (a column name or a list of columns)
- no enabled uniqueness data test (unique, or a composite unique_combination_of_columns) attached to the model has a column set that is a subset of the declared key

## Exclusions

- a unique_key value that is not a literal column name / list of column names is reported UNKNOWN, never UNCOVERED (the declared grain is not statically recoverable)
- a uniqueness test whose column set is WIDER than the key does not satisfy the check (uniqueness of a superset does not imply uniqueness at the declared grain)

## Recommendation

Add a uniqueness data test at the declared grain: `unique` on a single-column key, or `dbt_utils.unique_combination_of_columns` over the composite key columns.

## Rationale

Incremental merge / delete+insert semantics silently depend on the declared unique_key actually being unique — a duplicate key corrupts the merge with no test to catch it. Declaring a grain without a test at that grain is an unverified load-bearing assumption.
