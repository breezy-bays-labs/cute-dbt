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
- a covering test whose own config weakens the guarantee — severity: warn, a where row filter, or a limit cap — still attributes, marked as DEGRADED backing with every cause enumerated on the finding (in-row honesty: a cue beside the attribution, never a fourth verdict, never a percentage)
- when no enabled generic uniqueness test covers the key but an enabled singular (SQL-file) test references the model through depends_on, the verdict degrades to UNKNOWN — a singular test may assert the declared grain, but its SQL is not statically classifiable (never a false Uncovered nag on singular-test shops)
- a uniqueness test on the declared grain that exists but is disabled — config.enabled: false on a nodes-map test, or a generic-test entry in the manifest disabled map — never counts as coverage and surfaces as `exists but disabled` evidence, distinct from absent

## Exclusions

- a unique_key value that is not a literal column name / list of column names is reported UNKNOWN, never UNCOVERED (the declared grain is not statically recoverable)
- a uniqueness test whose column set is WIDER than the key does not satisfy the check (uniqueness of a superset does not imply uniqueness at the declared grain)
- a disabled SINGULAR test (and every non-generic-test disabled-map entry) carries no statically recoverable model linkage — both engines empty depends_on and omit attached_node on disabled nodes — so it is never attributed and never surfaced here

## Recommendation

Add a uniqueness data test at the declared grain: `unique` on a single-column key, or `dbt_utils.unique_combination_of_columns` over the composite key columns.

## Rationale

Incremental merge / delete+insert semantics silently depend on the declared unique_key actually being unique — a duplicate key corrupts the merge with no test to catch it. Declaring a grain without a test at that grain is an unverified load-bearing assumption.
