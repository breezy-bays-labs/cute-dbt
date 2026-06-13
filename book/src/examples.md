# Examples

cute-dbt commits rendered example output under
[`examples/`](https://github.com/breezy-bays-labs/cute-dbt/tree/main/examples).
Open one to see exactly what the CLI emits. There are two families:

- **[Reports examples](./examples/reports.md)** — the `report` verb's
  single-page PR-review report (baseline mode and `--pr-diff` mode):
  `jaffle-shop`, `dbt-playground`, and the `--pr-diff` diff-view showcase.
- **[Explore examples](./examples/explore.md)** — the `explore` verb's
  two-page full-manifest explorer: the interactive lineage DAG
  (`dag.html`) and the unit-test index (`tests.html`).

Every page makes **zero** outbound requests when opened offline — every
asset (Sakura CSS, jQuery, DataTables, Mermaid, Cytoscape) is inlined.
See [the zero-egress page](./zero-egress.md) for how it works.
