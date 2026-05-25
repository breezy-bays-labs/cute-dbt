# Changelog

All notable changes to this project will be documented in this file. The
format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/).

cute-dbt publishes to crates.io from `v0.1.0+` via `release-plz` + OIDC
trusted publishing. v0.x is unstable per Cargo SemVer convention: every
minor bump (`0.1 → 0.2`) MAY carry breaking changes; v1.0 ships the
first stability commitment when the CLI surface, the askama template
contract, and the auditability package have stabilized. Full release-
discipline policy in
[`AGENTS.md` §Release discipline](AGENTS.md#release-discipline).
`release-plz` generates entries below from conventional commits.

## [Unreleased]

### Added
- **askama 0.16 renderer (#11).** `src/adapters/render.rs` +
  `templates/report.html` produce the v0.1 interactive `report.html`:
  per-model + per-test JSON payload, node-role classification
  (`final` / `import` / `transform`), clean-import-CTE binding via
  `ref('NAME')` parsing, edge coloring keyed by the `EdgeType`
  vocabulary, inlined `interaction.js` (dark-mode stripped, Mermaid
  `<g>` selector runtime-constructed against the
  `{svgId}-flowchart-` prefix Mermaid 11.14+ stamps). DataTables init
  pinned to `paging:false / info:false / searching:false /
  scrollX:false / ordering:true`. The renderer is wired into the `cli`
  run loop's `render` step.
- `tests/render_integration.rs` — fixture-driven integration coverage:
  asset bundle assembly, design DOM contract, payload shape, JSON
  carrier integrity, structured resource-ref egress lint, and an insta
  chrome-only snapshot of the rendered jaffle-shop report.
- `examples/jaffle-shop-report.html` — end-to-end demonstration of the
  renderer's output against the committed jaffle-shop fixture pair.
  Open the file directly to see the real renderer behavior; a CI guard
  (`example-report-up-to-date`) regenerates and asserts byte-equality
  on every PR. `.gitattributes` marks `examples/*.html` as `-diff
  linguist-generated` so the GitHub UI suppresses the unreadable wall
  of inlined-asset bytes. Tracked follow-up for richer fixtures: #39.
- Genesis bootstrap commit: single-crate Cargo skeleton, full CI
  battery (fmt / clippy pedantic / nextest / llvm-cov / MSRV / cargo-deny /
  docs / crap4rs / non-mirror-guard / baseline-required-grep /
  feature-count), repo chrome (README, AGENTS, CLAUDE, CONTRIBUTING,
  SECURITY, ARCHITECTURE skeletons), src/{domain,ports,adapters,cli}/mod.rs
  stubs, `tests/binary_smoke.rs`, 5 `.feature` ATDD specs (corrected per
  the locked baseline-required policy), `assets/MANIFEST.toml` +
  `tests/fixtures/MANIFEST.toml` skeletons.

### Changed
- `src/adapters/asset_embed.rs` — dropped `MERMAID_INIT` constant + its
  pin test (the real init now lives in the inlined interaction script
  with `startOnLoad: false`) and `smoke_report_html` +
  its tests (the real renderer replaces it).
- `src/cli/mod.rs` — widened the run-loop `render` step signature to
  thread `current` / `models_in_scope` / `baseline_label` into the
  renderer; `parse_ctes()` is now a named no-op (per-model parsing
  happens inside the renderer during payload assembly).
- `ARCHITECTURE.md` — §6 (synthetic-only fixture invariant) relocated to
  `CONTRIBUTING.md` and `AGENTS.md` (the rule is build-hygiene, not
  architectural); section numbers tightened (§7 → §6, §8 → §7); the
  `tests/fixtures/MANIFEST.toml` mechanism shape is documented as a
  parallel of §5's `assets/MANIFEST.toml`.
- `README.md` — the diff-scope banner no longer carries the v0.1
  fidelity-limit wording; the body-checksum scoping note moved into the
  README's "What it shows" section as a single paragraph documenting
  what `state:modified.body` captures.
- `features/report_generation.feature` — dropped the stale "And the
  banner states the v0.1 fidelity limit 'model body changes'" line
  (Discovery #1 from #11; the banner contract is no longer that string).
