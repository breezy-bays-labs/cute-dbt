# cute-dbt examples

A committed end-to-end demonstration of cute-dbt's output. Open one of
these `.html` files in a browser to see what the tool produces against a
real compiled dbt manifest.

## Files

| File | Source manifests | What it shows |
|---|---|---|
| [`jaffle-shop-report.html`](jaffle-shop-report.html) | [`tests/fixtures/jaffle-shop-current.json`](../tests/fixtures/jaffle-shop-current.json) + [`jaffle-shop-baseline.json`](../tests/fixtures/jaffle-shop-baseline.json) | One in-scope model (`stg_customers`) with 1 unit test carrying populated `tags` / `meta` / `original_file_path`. Three-node CTE DAG (`source` → `renamed` → `stg_customers`) demonstrating the import / transform / final role classification + edge coloring + click-to-inspect. |

Open the file directly (`open examples/jaffle-shop-report.html` on macOS,
`xdg-open` on Linux, or drag into a browser). The page makes **zero**
outbound requests; every asset (Sakura CSS, jQuery, DataTables, Mermaid)
is inlined.

## How to regenerate

```bash
cargo run --bin cute-dbt -- \
  --manifest tests/fixtures/jaffle-shop-current.json \
  --baseline-manifest tests/fixtures/jaffle-shop-baseline.json \
  --out examples/jaffle-shop-report.html
```

CI verifies the committed file stays in sync with the renderer — a
silent drift between the renderer and what reviewers see surfaces as a
red gate (the `example-report-up-to-date` job re-renders against the
same committed fixtures and asserts byte-identical output).

## Why `.html` is committed, not generated

A reviewer evaluating cute-dbt for adoption opens this file once and
sees what the tool delivers. They do not run `cargo build` first. The
committed artifact is the lowest-friction proof-of-life; the CI guard
keeps it honest.

When the renderer changes, the CI guard fails until you regenerate the
file and commit the new version — the diff in your PR is the
human-readable summary of what changed in the rendered output.

## Future examples

This directory will grow as cute-dbt's renderer is exercised against
richer manifests (multi-model, multi-test-per-model, UNION CTEs,
modified-models-with-zero-tests, etc.). Tracked at
[cute-dbt#39](https://github.com/breezy-bays-labs/cute-dbt/issues/39).
