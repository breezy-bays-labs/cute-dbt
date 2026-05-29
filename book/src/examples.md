# Examples

cute-dbt commits rendered example reports under
[`examples/`](https://github.com/breezy-bays-labs/cute-dbt/tree/main/examples).
Open one to see exactly what the CLI emits.

## jaffle-shop

👉 **[Open the rendered jaffle-shop report](./examples/jaffle-shop-report.html)**
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

See [the zero-egress page](./zero-egress.md) for how it works.

## dbt-playground

👉 **[Open the rendered playground report](./examples/playground-report.html)**
([source](https://github.com/breezy-bays-labs/cute-dbt/blob/main/examples/playground-report.html))

The richer example — cute-dbt run against a real dbt project
([`cmbays/dbt-playground`](https://github.com/cmbays/dbt-playground), built
on synthetic [Synthea](https://synthetichealth.github.io/synthea/) patient
data). One diff touches three models, so the report shows off:

- **multiple model cards** side by side,
- **UNION arm rendering** in the CTE DAG (the dashed-orange edge + legend),
- an **empty-state card** (a model in scope with no unit tests wired).

Open it and click around — the fastest way to get a feel for a real-world
report.
