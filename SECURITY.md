# Security

> **Skeleton.** The full plain-language zero-egress + PHI-safe statement
> (risk-team-followable; non-engineer-readable) lands in PR 2 alongside
> [`ARCHITECTURE.md`](ARCHITECTURE.md). The mechanical cross-references
> (the headless test command, the resource-ref lint command, the asset
> manifest, the fixture manifest, `deny.toml`, `Cargo.lock`) are finalized
> in PR 9 via a one-page [`AUDIT.md`](AUDIT.md) index.

## What cute-dbt is

cute-dbt parses a dbt `manifest.json` and emits one self-contained
interactive HTML report visualizing dbt unit tests. It does NOT execute
SQL, connect to a database, send telemetry, or auto-update.

## What cute-dbt does not do (the adoption gate)

- **No outbound network requests.** The tool itself does not call out.
  The *generated* report does not call out either — every asset (Sakura
  CSS, jQuery, DataTables, Mermaid) is vendored and embedded into the
  binary at compile time and inlined into the single HTML output. Open
  the report offline; observe zero requests.
- **No database execution.** cute-dbt reads `manifest.json` bytes and
  writes HTML bytes. It does not link DuckDB, Snowflake, or any database
  driver into the binary.
- **No telemetry.** No analytics, crash reporting, auto-update, or
  phone-home.
- **No partial reports.** A `dbt parse`-only manifest, a pre-1.8
  manifest, an unreadable manifest, or an unusable baseline produces a
  non-zero exit and no HTML.

## How to verify yourself

The PR 9 [`AUDIT.md`](AUDIT.md) (filed when the auditability package
lands) is a one-page index of every artifact a risk team can re-run:
- The headless-browser network-block test command.
- The structured resource-ref lint command.
- `assets/MANIFEST.toml` — pinned versions, SHA-256, SPDX licenses for
  every vendored frontend asset.
- `tests/fixtures/MANIFEST.toml` — every committed test fixture flagged
  `synthetic_only = true`; no real customer or PHI data, ever.
- `deny.toml` — supply-chain policy (advisories, licenses, bans).
- `Cargo.lock` — committed for reproducible builds.

## Reporting a vulnerability

Open a private security advisory via GitHub:
<https://github.com/breezy-bays-labs/cute-dbt/security/advisories/new>.
For non-security issues use the public issue tracker.
