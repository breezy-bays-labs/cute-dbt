# Security

This document is the plain-language security statement for cute-dbt, written
for a non-engineer risk reviewer. Every claim here corresponds to a
mechanical artifact a risk team can re-run themselves; the technical
mechanism lives in [`ARCHITECTURE.md`](ARCHITECTURE.md) §5 (zero-egress)
and §6 (PHI-safe fixtures).

## What cute-dbt is

cute-dbt is a small command-line tool. You point it at one or two **dbt**
metadata files (`manifest.json`, produced by a separate `dbt` build step)
and it writes one self-contained HTML report visualizing the dbt unit
tests. It does not execute SQL, connect to a database, send telemetry,
or auto-update.

> **dbt** is the data-build tool a data team uses to define and test
> SQL models. **Unit tests** are dbt's built-in way to assert that a SQL
> model produces an expected result for a given input. cute-dbt reads
> dbt's existing metadata file and turns it into a report — it does not
> do the dbt build itself.

## What cute-dbt does **not** do

This list is the adoption contract for risk-restricted environments
(PHI-restricted, regulated, or air-gapped). Each item is either true by
construction (the code that would do the thing does not exist in the
binary) or verified mechanically (a CI gate enforces it).

### 1. No outbound network requests

The tool itself never makes a network request. Equally important: **the
generated `report.html` never makes a network request when you open it**,
whether you open it on a machine with networking allowed or with
networking denied.

Every frontend asset the report uses (the page stylesheet, the table
library, the diagram library) is *vendored* — a pinned copy of each
library is checked into this repository, compiled into the binary, and
inlined directly into the single HTML file the tool emits. The report
contains no `<script src="…">`, no `<link href="…">`, no `<img src="…">`,
no CSS `@import`, no CSS `url(…)`, and no protocol-relative `//…`
external references. It cannot call out because there is nothing in it
that calls out.

How this is proven, in two layers:

- **Primary proof (the one the risk team re-runs):** a *headless-browser
  network-block test* opens the generated `report.html` with all network
  access denied and asserts zero requests. The test runs from a real
  `file://` URL — the same way an operator opens the report on their own
  machine — not a `127.0.0.1` loopback that could behave differently. It
  runs in this repository's CI on every change to the renderer.
- **Secondary proof:** a *structured resource-ref lint* parses the
  generated HTML and rejects any real loading construct (the
  `<script src>`/`<link href>`/`<img src>`/`@import`/`url()`/`//`
  patterns listed above). It uses an HTML parser, not a raw text search,
  because minified asset bundles contain hundreds of inert URL string
  literals that are not loading constructs and would noise up a grep.

Both gates land in PR 9 and become enforced CI invariants from that
point onward. The PR 9 [`AUDIT.md`](AUDIT.md) lands as a one-page index
listing the exact command for each.

### 2. No database connection, no SQL execution

The binary does not link DuckDB, Snowflake, BigQuery, Postgres, or any
other database driver. It reads `manifest.json` bytes; it writes HTML
bytes. The actual dbt build — `dbt compile` or `dbt run`, which is what
produces the `manifest.json` cute-dbt reads — happens *outside* cute-dbt
on whichever machine the data team already runs dbt on. cute-dbt has no
warehouse credentials, no warehouse access, and no code path that could
acquire either.

### 3. No telemetry

Zero analytics, zero crash reporting, zero usage metrics, zero
auto-update. There is no opt-in toggle and no opt-out — the
telemetry-emitting code does not exist. The tool runs locally, reads
local files, writes local files, exits.

### 4. No partial reports (fail-closed)

If the inputs are unusable — a `manifest.json` produced by `dbt parse`
instead of `dbt compile`, a manifest from a dbt version older than 1.8
(before unit tests existed), an unreadable manifest, or an unusable
baseline manifest — cute-dbt exits with a non-zero status code and
writes no HTML. There is no half-finished report and no "best-effort"
output. The technical contract (the four named error variants) is in
[`ARCHITECTURE.md`](ARCHITECTURE.md) §3.

### 5. PHI-safe fixtures (this repository contains no real data)

Every committed test fixture, every snapshot, and every `.feature`
example table in this repository must contain only **synthetic** data or
**public demonstration** data. No real customer rows. No real PHI. No
exception.

This is not a checklist line; it is enforced mechanically:

- `tests/fixtures/MANIFEST.toml` lists every committed fixture with its
  origin (`synthetic-generated`, `tuva-demo`, `jaffle-shop`, etc.), URL
  when applicable, SHA-256 checksum, and an explicit `synthetic_only =
  true` affirmation per file.
- A `cargo test` parses the manifest and walks `tests/fixtures/` to
  verify every file is listed.
- A CI grep fails the build on any file under `tests/fixtures/` not
  listed in the manifest.

When you (the operator) run cute-dbt locally, the tool reads *your*
`manifest.json` from your machine and writes the report back to your
machine. **Your manifest never leaves your machine.** It is not uploaded
to this repository, not sent to any service, and not stored anywhere
cute-dbt controls.

### 6. Reproducible, auditable build

The build is supply-chain-auditable:

- `Cargo.lock` is committed, so anyone who builds this repository at a
  given commit gets the same dependency graph the maintainer built.
- `rust-toolchain.toml` pins the Rust toolchain (compiler and
  components) the project builds with.
- `deny.toml` runs as a CI gate (`cargo-deny check`) and enforces a
  license allowlist (MIT, BSD, Apache-compatible only), a security
  advisory scan, and dependency-source restrictions.
- `assets/MANIFEST.toml` records every vendored frontend asset with
  name, version, upstream URL, SHA-256, and SPDX license — the
  supply-chain artifact for the inlined assets that `cargo-deny` does
  not see (because they are not Cargo dependencies).

## How a risk team verifies all of the above

Every claim in this document maps to an artifact in this repository
that a risk team can inspect or re-run themselves:

| Claim | Artifact |
|-------|----------|
| Zero outbound requests (binary) | The source code under `src/` contains no HTTP client crate (`reqwest`, `ureq`, `hyper-client`, `curl`); `cargo-deny` would surface one if added. |
| Zero outbound requests (report) | Headless network-block test + resource-ref lint, both in CI from PR 9 onward; indexed in [`AUDIT.md`](AUDIT.md). |
| No database driver | `Cargo.toml` dependency list (no DuckDB, no Snowflake, no warehouse driver). |
| No telemetry | Source-level absence; reinforced by `cargo-deny`'s license + source policy. |
| Fail-closed contract | The four `PreflightError` variants are exhaustively covered by the `fail_closed.feature` scenarios under `features/`. |
| PHI-safe fixtures | `tests/fixtures/MANIFEST.toml` + the `cargo test` that parses it + the CI grep. |
| Reproducible build | `Cargo.lock` + `rust-toolchain.toml` + `cargo-deny` gate. |
| Vendored frontend provenance | `assets/MANIFEST.toml` (name, version, URL, SHA-256, SPDX). |

The PR 9 [`AUDIT.md`](AUDIT.md) is the single entry point — a one-page
index of every command and artifact above, written so a non-engineer can
follow it end to end without reading the source code.

## Reporting a vulnerability

If you find a security issue in cute-dbt, please open a private
security advisory on GitHub:

<https://github.com/breezy-bays-labs/cute-dbt/security/advisories/new>

For non-security issues (bugs, feature requests, documentation), use the
public issue tracker on the same repository.
