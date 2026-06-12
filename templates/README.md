# `templates/`

askama 0.16 templates and first-party CSS/JS, embedded into the binary at
compile time (`include_str!` or askama `{% include %}`) and emitted with
the `|safe` filter — each generated page is one self-contained HTML file.

| File | Page family |
|---|---|
| `partials/tokens.css` | shared (askama `{% include %}` in all three pages) — the **design-system root**: semantic tokens + all 8 `[data-theme]` blocks |
| `partials/base.css` | shared (askama `{% include %}` in all three pages) — minimal cross-family page chassis |
| `partials/test-card.html` | shared (askama `{% include %}` in report + explore-tests) |
| `appearance.js` | shared (both page families) — reads/applies `cute-dbt.appearance.v1`, exposes `window.CuteAppearance` |
| `report.html` + `report.css` + `interaction.js` + `theme.js` + `cyto-dag.js` | PR-review report (`report.css` = the report-specific design layers; `theme.js` = the report-only settings UI over the shared appearance engine) |
| `explore-dag.html` + `explore-lineage.js` + `explore-cte.js` | explore: lineage page |
| `explore-tests.html` + `explore-tests.js` | explore: tests page |

`partials/tokens.css` carries the design system (semantic tokens + the 8
`[data-theme]` blocks — extracted from `report.css` at cute-dbt#242);
`report.css` keeps the report-specific remainder (style packs, settings
chrome, tokenized components, density overrides, reconciliation +
coverage layers). See [`DESIGN.md`](../DESIGN.md) for the source-of-truth
map, design constraints, and divergence ledger, and
[`ARCHITECTURE.md` §5](../ARCHITECTURE.md) for the asset-embedding /
zero-egress contract (`src/adapters/asset_embed.rs`,
`assets/MANIFEST.toml`).
