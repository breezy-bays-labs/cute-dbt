<!-- GENERATED — do not edit. Source of truth: the `heuristics!` block in src/domain/checks.rs. Regenerate: GEN_HEURISTICS_LEDGER=1 cargo test --test heuristics_ledger -->

# union.arm-coverage

**Unexercised UNION arm**

| | |
|---|---|
| Group | `union` |
| Tier | `high` |
| Instrument | `unit-test` |

## Conditions

- the model's body (or a CTE within it) UNIONs arms the CTE engine resolved to union-typed edges — each checked arm is a join-free reference to an earlier CTE (`UnionAll` / `UnionDistinct`)
- an arm counts as exercised when at least one unit-test given with one or more in-manifest rows binds — by `ref(...)` / `source(...)` leaf name, case-insensitive — to any external relation in the arm's upstream CTE closure
- a given bound to a relation shared by several arms exercises every arm whose closure reads it: its rows provably enter each arm's scan, while per-arm filter survival is deliberately out of scope (no predicate evaluation) — the HIGH-tier cue boundary, never an assertion of output-level coverage
- verdict order: any provably-unfed arm makes the construct UNCOVERED; otherwise any statically-unattributable arm makes it UNKNOWN; otherwise COVERED, attributing every test that feeds an arm

## Exclusions

- arms that are not a join-free reference to an earlier CTE (join chains, derived tables, arms reading external tables directly, EXCEPT/INTERSECT arms) emit no union edge and are invisible to this check — never counted, never reported
- an arm whose upstream closure reads no resolvable external relation (constant SELECT, table functions) makes the construct UNKNOWN, never UNCOVERED
- an arm fed only by external-fixture or non-literal-sql givens (row counts not statically recoverable) makes the construct UNKNOWN, never UNCOVERED
- an arm whose only unbound feeding relation resolves to a seed is UNKNOWN, never UNCOVERED, when the model has unit tests (dbt lets seed inputs go ungiven and reads the real seed file)
- `this` givens (incremental prior state) never feed a union arm

## Recommendation

Add (or fill) a given row for each unexercised UNION arm's input so every arm contributes at least one row, then extend `expect` with the row(s) that arm should emit. This finding's evidence carries the per-arm input and a given-row sketch.

## Rationale

A UNION arm with no fixture rows contributes nothing to any unit test: its projection, casts, and filters run on zero rows, so a column mix-up or a dropped row in that arm ships silently. One given row per arm makes every branch's contribution visible in the expected output.
