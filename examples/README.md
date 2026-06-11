# cute-dbt examples

A committed end-to-end demonstration of cute-dbt's output. Open one of
these `.html` files in a browser to see what the tool produces against a
real compiled dbt manifest. These are the **golden examples** — committed
and byte-identity gated, the canonical reports a consumer browses. (The
live, per-PR dogfood of cute-dbt's *own* `dbt-project/` is a transient CI
artifact on the sticky PR comment, not committed here.)

## Files

| File | Source manifests | What it shows |
|---|---|---|
| [`jaffle-shop-report.html`](jaffle-shop-report.html) | [`tests/fixtures/jaffle-shop-current.json`](../tests/fixtures/jaffle-shop-current.json) + [`jaffle-shop-baseline.json`](../tests/fixtures/jaffle-shop-baseline.json) | One in-scope model (`stg_customers`) with 1 unit test carrying populated `tags` / `meta` / `original_file_path`. Three-node CTE DAG (`source` → `renamed` → `stg_customers`) demonstrating the import / transform / final role classification + edge coloring + click-to-inspect. |
| [`playground-report.html`](playground-report.html) | [`tests/fixtures/playground-current.json`](../tests/fixtures/playground-current.json) + [`playground-baseline.json`](../tests/fixtures/playground-baseline.json) | The richer example. Models from [`cmbays/dbt-playground`](https://github.com/cmbays/dbt-playground): `mart_dq_summary` (UNION-ALL of encounter + medication DQ metrics, 2 unit tests), `dim_payers` (UNION-ALL with unknown-sentinel, 1 unit test), `int_dq_quarantine__encounters` (modified-no-tests → empty-state card), `fct_encounters_incremental` — an **incremental** model whose unit test surfaces the incremental-mode badge, the expect-semantics tooltip, and the `prior model state` (`given: - input: this`) badge (cute-dbt#145) — and `fct_clinical_events` (modified-no-tests, 5-arm UNION). Exercises multi-model in-scope, multi-test-per-model, UNION arms, the empty-state card, fixture-format diversity (`sql` given + mixed `dict` / `csv` expect), incremental-model semantics, AND the full **coverage-checks panel** (cute-dbt#170): covered / uncovered / unknown checklist rows, TOTAL + HIGH tier chips, an uncovered UNION finding with copyable given-row YAML sketches (`fct_clinical_events`), an uncovered grain finding (`mart_dq_summary`), and a pragma-suppressed finding revealed with its reason (`dim_payers`). Rendered with `--project-root` so the Authoring-YAML drawer populates. |
| [`diff-showcase-report.html`](diff-showcase-report.html) | [`tests/fixtures/playground-current.json`](../tests/fixtures/playground-current.json) + [`tests/fixtures/playground-pr-diff.patch`](../tests/fixtures/playground-pr-diff.patch) (`--pr-diff`, no baseline) | The **diff-view showcase** — the `--pr-diff` feature set rendered against a synthetic hand-crafted patch: foldable inline SQL diffs, YAML block diffs, and cell-level data diffs with NULL-aware cells. Stays synthetic so no `root_path` leaks into the committed HTML. |

Open any file directly (`open examples/jaffle-shop-report.html` on macOS,
`xdg-open` on Linux, or drag into a browser). Every page makes **zero**
outbound requests; every asset (Sakura CSS, jQuery, DataTables, Mermaid,
Cytoscape) is inlined.

## How to regenerate

The flags must match the `example-report-check` matrix in `ci.yml`
exactly (the byte-identity gate), including `--project-root` where the
report slices Authoring YAML from committed source.

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
  --project-root tests/fixtures/playground-source \
  --out examples/playground-report.html

# diff-showcase (--pr-diff against the synthetic patch, no baseline)
cargo run --bin cute-dbt -- \
  --manifest tests/fixtures/playground-current.json \
  --pr-diff @tests/fixtures/playground-pr-diff.patch \
  --project-root tests/fixtures/playground-source \
  --out examples/diff-showcase-report.html
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
