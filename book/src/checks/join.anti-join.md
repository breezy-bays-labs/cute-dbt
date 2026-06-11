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
- OR the model filters `WHERE NOT EXISTS (SELECT … FROM <inner> WHERE …)` in a top-level AND conjunct, the inner being a single plain named relation whose WHERE carries a correlated reference to the outer query — the resolvable outer↔inner equi-conjuncts are the anti-join keys (cute-dbt#196)
- OR the model filters `WHERE <col> NOT IN (SELECT <col> FROM <inner>)` in a top-level AND conjunct, the inner being a single plain named relation projecting exactly one column — the membership pair (outer column ↔ inner projected column) is the anti-join key (cute-dbt#196). SQL honesty note: a NULL in the inner column makes NOT IN yield NO rows at all; detection still treats the construct as the anti-join idiom (that is how it is authored), and the matched-row fixture this check recommends is exactly what surfaces the NULL trap
- the recommendation INVERTS join.left-null-propagation's: the anti-join's risk is the matched class leaking through, so the missing fixture is a left row that DOES match a right row, with `expect` proving it is excluded
- satisfaction (all arms): some unit test's literal givens carry a left/outer row whose key matches an inner/right given row (both cells non-NULL, equal on the value-normalized key)
- supersedes join.left-null-propagation on the same construct: NULL right-side columns are the anti-join's working mechanism, not an untested gap (the subquery constructs are never enumerated by left-null-propagation at all)
- given binding follows the union.arm-coverage premise: only `given` entries carry statically-visible rows — an ungiven non-seed input is an empty mock; join sides bind directly by external leaf name or through a single-external simple-FROM closure

## Exclusions

- negated subqueries anywhere but a top-level AND conjunct of a SELECT's WHERE (OR branches, HAVING, JOIN ON, projections) are NOT detected — different semantics or position, silent, never misclassified
- non-negated EXISTS / IN subqueries (semi-join and membership inclusion — future evidence consumers) and scalar subqueries are NOT detected
- a NOT EXISTS / NOT IN whose inner is a derived table, joins or reads several relations, or carries its own WITH clause is NOT detected; an uncorrelated NOT EXISTS (zero outer references) is not a keyed anti-join — silent
- expression (non-column) correlations and projections are not key material: a correlated NOT EXISTS with no resolvable equi pair, and a NOT IN whose outer column does not resolve to a single relation, degrade to UNKNOWN, never UNCOVERED
- an IS NULL on a non-key right column is a data filter, not the anti-join idiom — join.left-null-propagation governs that construct
- an IS NULL inside an OR branch has different semantics and is never treated as the anti-join filter
- unrecoverable join keys, unresolvable side bindings, external `fixture:` files, non-literal `format: sql` givens, and ungiven seed inputs degrade to UNKNOWN, never UNCOVERED

## Recommendation

Add a matching given pair: one left row whose join key IS present in the right-side given rows, then assert in `expect` that the matched row is excluded from the output. This finding's evidence carries a copy-pasteable given sketch.

## Rationale

An anti-join's output is defined by what it excludes. Every existing given that only carries unmatched rows proves the keep path, never the exclusion: if the ON key drifts or the IS NULL column changes, matched rows leak into the output and no test catches it.
