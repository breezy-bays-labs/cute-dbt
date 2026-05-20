# `templates/`

askama 0.16 templates that reproduce the returned Claude Design
`report.html` DOM/class contract. Real templates land with PR 8b (#TBD)
after Claude Design delivers the HTML built from
`workspace/cute-dbt/.../claude-design-handoff-spec.md` (in the private ops
repo, paired with this codebase).

The asset-inlining infrastructure that templates depend on
(`include_str!` / `include_bytes!` of vendored Sakura / jQuery /
DataTables / Mermaid bundles, plus the `assets/MANIFEST.toml` provenance
contract) lands in PR 8a (#TBD) — buildable the moment PR 6 and PR 7
land, NOT blocked on the Claude Design hand-back.

This directory is committed at bootstrap as a docs-only stub for
module-roster completeness — see `ARCHITECTURE.md` and the v0.1 roadmap.
