# `templates/`

askama 0.16 templates and first-party CSS/JS, embedded into the binary at
compile time (`include_str!`) and emitted through askama with the `|safe`
filter — each generated page is one self-contained HTML file.

| File | Page family |
|---|---|
| `report.html` + `report.css` + `interaction.js` + `theme.js` + `cyto-dag.js` | PR-review report |
| `explore-dag.html` + `explore-lineage.js` + `explore-cte.js` | explore: lineage page |
| `explore-tests.html` + `explore-tests.js` | explore: tests page |
| `partials/test-card.html` | shared (askama `{% include %}` in report + explore-tests) |

`report.css` carries the design system (semantic tokens + the 8
`[data-theme]` blocks). See [`DESIGN.md`](../DESIGN.md) for the
source-of-truth map, design constraints, and divergence ledger, and
[`ARCHITECTURE.md` §5](../ARCHITECTURE.md) for the asset-embedding /
zero-egress contract (`src/adapters/asset_embed.rs`,
`assets/MANIFEST.toml`).
