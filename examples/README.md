# cute-dbt examples

A committed end-to-end demonstration of cute-dbt's output. Open one of
these `.html` files in a browser to see what the tool produces against a
real compiled dbt manifest.

## Files

| File | Source manifests | What it shows |
|---|---|---|
| [`jaffle-shop-report.html`](jaffle-shop-report.html) | [`tests/fixtures/jaffle-shop-current.json`](../tests/fixtures/jaffle-shop-current.json) + [`jaffle-shop-baseline.json`](../tests/fixtures/jaffle-shop-baseline.json) | One in-scope model (`stg_customers`) with 1 unit test carrying populated `tags` / `meta` / `original_file_path`. Three-node CTE DAG (`source` → `renamed` → `stg_customers`) demonstrating the import / transform / final role classification + edge coloring + click-to-inspect. |
| [`playground-report.html`](playground-report.html) | [`tests/fixtures/playground-current.json`](../tests/fixtures/playground-current.json) + [`playground-baseline.json`](../tests/fixtures/playground-baseline.json) | The richer example. Three in-scope models from [`cmbays/dbt-playground`](https://github.com/cmbays/dbt-playground): `mart_dq_summary` (UNION-ALL of encounter + medication DQ metrics, 2 unit tests), `dim_payers` (UNION-ALL with unknown-sentinel, 1 unit test), and `int_dq_quarantine__encounters` (modified-no-tests → empty-state card). Exercises multi-model in-scope, multi-test-per-model, UNION arms in two distinct patterns, the empty-state card, AND dbt unit_test fixture-format diversity (`sql` given + mixed `dict` / `csv` expect). |

Open either file directly (`open examples/jaffle-shop-report.html` /
`open examples/playground-report.html` on macOS, `xdg-open` on Linux,
or drag into a browser). Both pages make **zero** outbound requests;
every asset (Sakura CSS, jQuery, DataTables, Mermaid) is inlined.

## How to regenerate

```bash
# jaffle-shop
cargo run --bin cute-dbt -- \
  --manifest tests/fixtures/jaffle-shop-current.json \
  --baseline-manifest tests/fixtures/jaffle-shop-baseline.json \
  --out examples/jaffle-shop-report.html

# playground
cargo run --bin cute-dbt -- \
  --manifest tests/fixtures/playground-current.json \
  --baseline-manifest tests/fixtures/playground-baseline.json \
  --out examples/playground-report.html
```

CI verifies every committed example stays in sync with the renderer —
silent drift between the renderer and what reviewers see surfaces as a
red gate. The `example-report-up-to-date` job (and its
`example-report-check (<name>)` matrix workers) re-renders each
example against its committed fixtures and asserts byte-identical
output.

## Why `.html` is committed, not generated

A reviewer evaluating cute-dbt for adoption opens this file once and
sees what the tool delivers. They do not run `cargo build` first. The
committed artifact is the lowest-friction proof-of-life; the CI guard
keeps it honest.

When the renderer changes, the CI guard fails until you regenerate the
file and commit the new version — the diff in your PR is the
human-readable summary of what changed in the rendered output.

## Future examples

This directory grows as cute-dbt's renderer is exercised against new
manifest shapes. `playground-report.html` covers UNION CTEs, multi-
model in-scope, multi-test-per-model, and the modified-no-tests
empty-state. Remaining gaps (cross-join classification, fusion-
produced cross-engine fixture) are tracked as follow-ups to
[cute-dbt#39](https://github.com/breezy-bays-labs/cute-dbt/issues/39).
