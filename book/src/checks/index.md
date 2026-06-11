<!-- GENERATED — do not edit. Source of truth: the `heuristics!` block in src/domain/checks.rs. Regenerate: GEN_HEURISTICS_LEDGER=1 cargo test --test heuristics_ledger -->

# Checks

The coverage-intelligence check registry. Each check pairs a construct trigger with a satisfaction predicate and reports a per-construct verdict: covered, uncovered, or unknown.

| Check | Name | Tier | Instrument |
|---|---|---|---|
| [`grain.unique-key-unbacked`](./grain.unique-key-unbacked.md) | Unique key without a uniqueness test | `total` | `data-test` |
| [`union.arm-coverage`](./union.arm-coverage.md) | Unexercised UNION arm | `high` | `unit-test` |
| [`join.left-null-propagation`](./join.left-null-propagation.md) | LEFT JOIN null propagation untested | `high` | `both` |
| [`join.anti-join`](./join.anti-join.md) | Anti-join exclusion untested | `high` | `unit-test` |
