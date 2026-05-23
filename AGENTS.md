# cute-dbt Agent Notes

Cross-provider agent operating guide. Both Claude Code and Codex should read
this before touching code.

## Repo identity

- `cute-dbt` is a **single-crate** Rust CLI (`cute4dbt`, lib + bin) in the
  `breezy-bays-labs` org. Public visibility from day one; **no crates.io
  publish, no GitHub Release tarballs, no `cargo install` path** until v1.0
  gates trip. Tags exist for git pinning only — internal versions like
  `v0.1.0` do not trigger any workflow.
- v0.x consumer model: the tool runs locally and privately — your data
  never leaves your machine. The core privacy property is *trivially
  auditable* zero data exfiltration: the generated report makes zero
  outbound requests when opened offline via `file://`.

## Architecture

Hexagonal layering as **inward-dependency discipline only** — `src/{domain,
ports, adapters, cli}` modules with `domain` depending on nothing outward.
Enforced by module convention + clippy + review (a single crate cannot fail
to compile on an inward `use`). The full layering invariant, the two-stage
fail-closed contract, the StateComparator strategy, and the conscious
design simplifications (no workspace, no per-crate versioning, no API shim,
no AST-purity grep, no JSON envelope) are recorded in [`ARCHITECTURE.md`](ARCHITECTURE.md).

```
domain  -> (no outward imports; std + serde derive only)
  ^
ports/  -> trait seams with >1 real-or-test impl
  ^
adapters/ -> serde manifest reader, sqlparser CTE engine, askama renderer
  ^
cli/    -> clap derive, ExitCode mapping, run loop composition
  ^
main.rs -> thin entry
```

**Never import inward.** The five "conscious design simplifications" (no
workspace, no per-crate versioning, no API shim, no AST-purity grep, no
JSON envelope) are documented absences, not omissions. Adding any of them
is a regression, not a "pattern completion."
The `non-mirror-guard` CI job rejects:
- a `[workspace]` table in `Cargo.toml`
- `bans.deny.wrappers` in `deny.toml`
- `pub use crate::…::…` re-export shim pattern in `src/lib.rs`

## Working rules

- **Branch + PR for everything** after the genesis commit. The genesis
  commit is the one-time exception to no-direct-push-to-main (an empty
  repo cannot accept a PR targeting `main`).
- **Worktrees** for parallel work: `git worktree add ../cute-dbt-issue-N -b
  <area>-<issue>-<slug>`.
- **TDD** — tests before implementation for all domain and adapter code.
- **Domain purity** — `src/domain/` may import only `std` and `serde`
  (derive). No I/O, no parser libs, no clap, no askama.
- **POD-only domain** — owned data, constructors, no method machinery beyond
  what the run loop calls.
- **Object-safe strategies** — `StateModifier` is `dyn`-compatible and
  deliberately **not** `Send + Sync`. A `#[cfg(test)] assert_obj_safe!` pins
  the contract.
- **Property tests** required for the JSON serde round-trip and for the
  StateComparator union semantics.
- **Regression files committed** — any `proptest-regressions/` dirs go into
  git, never gitignored.

## Two-stage fail-closed contract

1. **Stage 1 — schema-level pre-flight at the manifest adapter** rejects:
   `Unreadable` (file/JSON-level), `SchemaUnsupported` (pre-1.8
   `metadata.dbt_schema_version`), `BaselineUnusable` (`--baseline-manifest`
   path missing/mismatched).
2. **Stage 2 — semantic compiled-SQL-presence check in the domain**, run
   *after* the StateComparator selects the in-scope set, rejects
   `NotCompiled { node_id, unit_test }` *only* for in-scope unit tests
   whose target model has `compiled_code: null` (`dbt parse` case).

`PreflightError` is a `#[non_exhaustive]` enum with these four variants.
Baseline-missing is **not** a variant — it is a clap-level usage error
raised before the manifest is read. Adding a fifth variant for
baseline-missing would conflate usage-time and runtime errors; don't.

## State-modified contract

`StateComparator` is a domain strategy holding `Vec<Box<dyn StateModifier>>`
and reporting a node modified if *any* registered modifier says so (dbt's
OR semantics across sub-selectors). v0.1 has exactly one impl,
`BodyChecksumModifier`, comparing `node.checksum`. Sub-selectors
(`.configs`/`.relation`/`.macros`/`.contract`) arrive in v0.2+ as additive
`impl StateModifier` blocks — never a comparator/domain/scoping rewrite.

In-scope unit-test selection: unit tests whose target model is in
`modified_set`, **unioned with** unit tests whose own node is in
`modified_set` (a changed test on an unchanged model is in scope).

## Asset embedding contract

All vendored frontend assets (Sakura CSS, jQuery, DataTables, Mermaid)
are embedded at compile time via `include_str!`/`include_bytes!` into the
binary's `.rodata` and emitted through askama with the `|safe` filter.
**Never** a runtime asset directory. **Never** ESM Mermaid (`type=module`).
Mermaid initializes with `securityLevel: 'strict'` and an explicit
non-webfont `fontFamily` (system stack). The favicon is a `data:` URI.
Pin + SHA-256 + SPDX license per asset in `assets/MANIFEST.toml`.

## Zero-egress gate (the core privacy property)

The primary auditability artifact is the **headless-browser network-block
test** — opens the generated `report.html` via real `file://` with all
network access denied and asserts zero requests. Re-runnable by anyone with
the repo checked out. The secondary structured **resource-ref lint** rejects
real loading constructs (`<script src>`, `<link href>`, `<img src>`,
CSS `@import` / `url()`, protocol-relative `//`) — never raw `grep http`
(minified bundles carry hundreds of inert URL string literals).

## Synthetic-only fixtures

A hard, non-negotiable invariant: every committed fixture / `insta`
snapshot / `.feature` example must be synthetic or public-demo data —
no real data, ever. Enforced mechanically:
`tests/fixtures/MANIFEST.toml` lists every fixture with origin / source /
SHA-256 / `synthetic_only = true`; a `cargo test` parses the manifest
and a CI grep fails on any unlisted file under `tests/fixtures/`. Real
data in this public repo is a release blocker. The same mechanism shape
applies to `assets/MANIFEST.toml` (the vendored frontend bundle's
provenance index) — see
[`ARCHITECTURE.md` §5](ARCHITECTURE.md#5-asset-embedding-zero-egress-gate).

## Working commands

| Task | Command |
|------|---------|
| Build | `cargo build` |
| Test | `cargo nextest run` (or `cargo test`) |
| Coverage | `cargo llvm-cov nextest --lcov --output-path lcov.info` |
| Lint | `cargo clippy --all-targets -- -D warnings` |
| Format | `cargo fmt` |
| Supply chain | `cargo deny check` |
| BDD outer loop | `cargo test --test bdd` (cucumber-rs, `harness = false`; not nextest-compatible) |
| Quick verify | `lefthook run pre-push` |
