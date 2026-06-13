# Reports examples

The `report` verb renders one self-contained HTML page per run. These are
the **golden** reports — committed and byte-identity gated in CI, the
canonical artifacts a consumer browses. Every page makes **zero** outbound
requests when opened offline; see [the zero-egress page](../zero-egress.md).

## jaffle-shop

👉 **[Open the rendered jaffle-shop report](../examples/jaffle-shop-report.html)**
([source](https://github.com/breezy-bays-labs/cute-dbt/blob/main/examples/jaffle-shop-report.html))

The structural minimum: one in-scope model, one unit test, a 3-node CTE
DAG. Good for "what does the output look like?"

**See the privacy property for yourself.** Open the page, then open
DevTools → Network and reload: **zero outbound requests** — every asset is
inlined, so your data never leaves your machine. The audit-grade version is
an automated headless-browser proof that opens the report via `file://`
with all network access denied:

```sh
git clone https://github.com/breezy-bays-labs/cute-dbt
cd cute-dbt
cargo test --test headless_zero_egress -- --ignored
```

See [the zero-egress page](../zero-egress.md) for how it works.

## dbt-playground

👉 **[Open the rendered playground report](../examples/playground-report.html)**
([source](https://github.com/breezy-bays-labs/cute-dbt/blob/main/examples/playground-report.html))

The richer example — cute-dbt run against a real dbt project
([`cmbays/dbt-playground`](https://github.com/cmbays/dbt-playground), built
on synthetic [Synthea](https://synthetichealth.github.io/synthea/) patient
data). One diff puts multiple models in scope, so the report shows off:

- **multiple model cards** side by side,
- **UNION arm rendering** in the CTE DAG (the dashed-orange edge + legend),
- **cross-join rendering** in the CTE DAG (`mart_date_state_grid` builds a
  dense months × states grid with an explicit `CROSS JOIN` CTE — the pink
  `cross` edge + its legend entry), exercised by a unit test that **mixes
  dict and csv givens** in a single test,
- a **`source()`-given unit test** (`stg_synthea__patients` mocks
  `source('synthea_raw', 'patients')` instead of a `ref()`),
- an **empty-state card** (a model in scope with no unit tests wired).

Open it and click around — the fastest way to get a feel for a real-world
report.

## diff-view showcase (PR-diff mode)

👉 **[Open the rendered diff-view showcase](../examples/diff-showcase-report.html)**
([source](https://github.com/breezy-bays-labs/cute-dbt/blob/main/examples/diff-showcase-report.html))

The two reports above run in **baseline** mode. This one runs in **`--pr-diff`
mode** — the way cute-dbt runs inside a CI PR review — so it shows the diff-view
feature set in action:

- an **inline SQL diff** with **hunk contraction** (three separated mid-file
  changes in a 158-line model collapse the long unchanged runs — including a
  folded section *between* each change — behind "Show N unchanged lines"
  controls you can expand **and re-collapse**, per-hunk or all at once),
- the **SQL/Jinja syntax palette** overlaid on the diff lines,
- a **YAML block diff** of the changed unit test, and
- a **cell-level data diff** across both expect rows, including a real **NULL**
  (rendered as muted italic, distinct from a string `"null"`).

It's rendered from a synthetic hand-crafted patch against the playground
fixtures (so it stays committable + zero-egress), and it's byte-identity-gated
in CI like the others.
