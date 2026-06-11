<!-- GENERATED — do not edit. Source of truth: the `heuristics!` block in src/domain/checks.rs. Regenerate: GEN_HEURISTICS_LEDGER=1 cargo test --test heuristics_ledger -->

# join.anti-join

**Anti-join exclusion untested**

| | |
|---|---|
| Group | `join` |
| Tier | `high` |
| Instrument | `unit-test` |
| Supersedes | [`join.left-null-propagation`](./join.left-null-propagation.md) |

## Conditions

- the model LEFT-JOINs a relation and filters `WHERE <right>.<key> IS NULL` in a top-level AND conjunct, where `<key>` is one of the join's ON equi-key right columns — the anti-join idiom: the join deliberately keeps the UNMATCHED left rows
- the recommendation INVERTS join.left-null-propagation's: the anti-join's risk is the matched class leaking through, so the missing fixture is a left row that DOES match a right row, with `expect` proving it is excluded
- satisfaction: some unit test's literal givens carry a left row whose ON equi-key matches a right given row (both cells non-NULL, equal on the value-normalized key)
- supersedes join.left-null-propagation on the same construct: NULL right-side columns are the anti-join's working mechanism, not an untested gap
- given binding follows the union.arm-coverage premise: only `given` entries carry statically-visible rows — an ungiven non-seed input is an empty mock; join sides bind directly by external leaf name or through a single-external simple-FROM closure

## Exclusions

- the NOT EXISTS / NOT IN anti-join equivalents are NOT detected in v1 — only the LEFT JOIN + IS NULL form is recognized (a declared gap: the construct is silent, never misclassified)
- an IS NULL on a non-key right column is a data filter, not the anti-join idiom — join.left-null-propagation governs that construct
- an IS NULL inside an OR branch has different semantics and is never treated as the anti-join filter
- unrecoverable join keys, unresolvable side bindings, external `fixture:` files, non-literal `format: sql` givens, and ungiven seed inputs degrade to UNKNOWN, never UNCOVERED

## Recommendation

Add a matching given pair: one left row whose join key IS present in the right-side given rows, then assert in `expect` that the matched row is excluded from the output. This finding's evidence carries a copy-pasteable given sketch.

## Rationale

An anti-join's output is defined by what it excludes. Every existing given that only carries unmatched rows proves the keep path, never the exclusion: if the ON key drifts or the IS NULL column changes, matched rows leak into the output and no test catches it.
