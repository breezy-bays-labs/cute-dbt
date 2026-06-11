# `dbt-project/` — embedded dbt-fusion example (cute-dbt dogfood target)

A small, real, **dbt-fusion**-compilable dbt project that cute-dbt runs
against to self-dogfood the `--pr-diff` sticky preview. A PR can edit a
model's `.sql` or a `unit_test`'s YAML here, and the preview job can run
cute-dbt `--pr-diff` on the PR's *own* git diff — making every PrDiff
diff feature (cute-dbt #91/#96/#111/#98) validatable from one CI artifact.
This directory is the **live self-dogfood source** for those PR preview
reports: the sticky comment's `dbt-project` row renders only on PRs that
touch files under `dbt-project/`.

This directory holds **dbt project source only** — models, tests, seeds,
config. The compiled `target/manifest.json` is **gitignored build output**:
it is recompiled fresh by every consumer (the CI preview job runs
`dbt compile`; local dev does the same) and is **never committed**. The
sticky-preview job runs `cute-dbt --pr-diff` against this project on the
PR's own diff — see cute-dbt #114 / #118.

## Provenance (synthetic-only invariant)

- **Base**: [`dbt-labs/jaffle_shop_duckdb`](https://github.com/dbt-labs/jaffle_shop_duckdb)
  — dbt Labs' canonical starter project (the same source as the committed
  `tests/fixtures/jaffle-shop-*.json` fixtures). The base models
  (`stg_customers`, `stg_orders`, `stg_payments`, `customers`, `orders`),
  the `schema.yml` tests/docs, and the three seed CSVs are taken verbatim.
- **License**: Apache-2.0 (jaffle_shop_duckdb).
- **Data**: 100% synthetic. The seed CSVs (`raw_customers`, `raw_orders`,
  `raw_payments`) are jaffle's Faker-style demo records — no real
  customers, no real records.
- **Added for cute-dbt** (authored fresh, fusion-clean by construction):
  - `models/marts/order_metrics.sql` — a "rich" mart whose transform CTEs
    reference each other through **every** cute-dbt v0.1 `EdgeType`
    (`src/domain/cte.rs`): `From`, `Inner`, `Left`, `Right`, `Full`,
    `Cross`, `UnionAll`, `UnionDistinct` — plus a comma cross-join
    (`from a, b`) that exercises the cute-dbt#40 multi-source heuristic
    (which renders as two `From` edges, **not** a `Cross` edge; only an
    explicit `CROSS JOIN` produces the `Cross` edge).
  - `models/marts/_marts__models.yml` — three `unit_tests` covering all
    three dbt fixture formats (see below).

## What it exercises (for cute-dbt rendering)

- **CTE DAG variety** — `order_metrics`'s graph carries all eight
  `EdgeType` variants, so cute-dbt's DAG + JoinType legend render real,
  varied material.
- **3 `unit_tests`, all three fixture formats** (the divergent surface
  cute-dbt parses at render time), spread across CTE-carrying models so a
  future PR editing them produces a meaningful diff:

  | unit_test | model | given | expect |
  |---|---|---|---|
  | `test_order_metrics_computes_amount_share` | `order_metrics` | dict | dict |
  | `test_orders_sums_payment_methods` | `orders` | csv | csv |
  | `test_customers_aggregates_orders_and_payments` | `customers` | sql | dict |

  Under dbt-fusion the `csv` fixture normalizes to a **raw CSV string**
  (not an array of dicts) — cute-dbt's hand-rolled RFC 4180 parser handles
  this (cute-dbt #66). All three unit tests **pass** under fusion against a
  materialized warehouse (verified during authoring).

## Setup & compile (dbt-fusion, pip-free)

dbt-fusion is a standalone Rust binary — **no Python, no pip**. This
project has **no dbt packages**, so there is no `dbt deps` step and no
network access of any kind: install the binary, then compile.

### 1. Install dbt-fusion (official installer)

```sh
curl -fsSL https://public.cdn.getdbt.com/fs/install/install.sh | sh
```

This drops the `dbt` binary in `~/.local/bin`. Confirm it is fusion:

```sh
dbt --version   # → dbt-fusion 2.0.0-preview.NNN
```

> `dbt_project.yml` carries `require-dbt-version: [">=1.11.0", "<3.0.0"]`
> (inherited from upstream jaffle). This example is **deliberately
> fusion-targeted** — that floor is the version range this dogfood project
> is pinned to, not a statement about cute-dbt's own engine support
> (cute-dbt ingests any schema-v12 manifest, which dbt-core 1.8+ and
> dbt-fusion both emit — see `AGENTS.md`).

### 2. Compile → `manifest.json`

```sh
dbt compile --profiles-dir .
```

`profiles.yml` lives **inside this directory** (duckdb `:memory:`), so
compile succeeds fully offline — it parses + renders SQL without touching a
warehouse or any data. Output: `target/manifest.json` (schema v12) — this
is **gitignored build output**: feed it to cute-dbt, but never commit it
(see "The compiled manifest" below).

### Optionally: run the project end-to-end (to execute the unit tests)

`dbt compile` is all cute-dbt needs. To actually *run* the models + unit
tests you need a file-backed warehouse (the in-memory target does not
persist relations across `seed`/`run`/`test`):

```sh
dbt build --profiles-dir .   # with a file-backed duckdb target
```

(The pip-free `uv` workflow only matters when re-syncing source that uses
dbt's *deprecated* generic-test argument format, which fusion rejects —
run the official autofix ephemerally, no venv, no pip:
`uvx dbt-autofix@latest deprecations --path .`. The current source is
already in the modern `arguments:` format, so this is not needed today.)

## The compiled manifest

`target/manifest.json` is **gitignored build output** — the whole of
`target/` is ignored (`.gitignore`), and the manifest is **never
committed**. Every consumer recompiles it fresh from this project source:

- **CI** — the `report-preview` job (cute-dbt #118) runs `dbt compile`
  ephemerally at the PR's HEAD, then feeds that just-compiled manifest to
  `cute-dbt --pr-diff`. The committed source is the input; the manifest is
  rebuilt every run.
- **Local** — run `dbt compile --profiles-dir .` (above), then point
  cute-dbt at the resulting `target/manifest.json`.

> **NEVER re-commit `target/manifest.json` (root_path leak vector).** dbt
> writes each node's `root_path` as the **absolute** build-machine project
> path (e.g. `/Users/you/...`). A committed manifest therefore carries a
> machine-specific path, and — worse — a later `dbt compile`/`build`/`test`
> silently re-injects it on every recompile (gitleaks does not catch it).
> cute-dbt ignores `root_path` entirely (it drives off the relative
> `original_file_path` + the explicit `--project-root`), so there is **no
> reason** to commit the manifest and a real privacy reason not to. This is
> why an earlier `!target/manifest.json` un-ignore exception was retired
> (cute-dbt #115).
