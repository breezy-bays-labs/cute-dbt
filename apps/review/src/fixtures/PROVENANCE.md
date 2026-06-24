# Fixture provenance

All fixtures here are **synthetic** PR-440 dogfood data (+ the comments-showcase
golden), extracted from committed/rendered, leak-free cute-dbt sources. They are
**scrub-clean**: verified zero `/Users/` / `root_path` / `/home/` / username
leaks (a CI grep + a pre-commit re-verify back this). They stand in for the
eventual Rust `--context-out` artifact (epic #485 / slice S3a) until that lands.

Never replace any of these with a render of a real local dbt project — that would
bake `metadata.root_path` (a home/runner absolute path) into the JSON. Synthetic
only is a hard, non-negotiable invariant (mirrors `tests/fixtures/MANIFEST.toml`
on the Rust side).

| File | Source | Carries |
|---|---|---|
| `context.440.json` | PR #440 live dogfood (`dbt-project/`, `--pr-diff`) | the everything sample — 16 models, all edge-type variants (incl. the bare `union`), governance, macro lens, seeds, a `deleted` model, findings, comment threads, the §3a lineage spine. The S0 primary. |
| `context.sample.json` | the comments-showcase golden | 2-model minimal — sql_diff + CTE DAG + comment threads. |
| `context.440.since-review.json` | PR #440 (since-review scope) | the thin shape — exercises the Zod tolerate-thin-fixtures path. |
| `avatars.json` | synthetic GitHub-style avatar handles | inline `data:` URI avatars (no webfont, no remote image). |
