@AGENTS.md

# CLAUDE.md — cute-dbt

Zero-compute dbt unit-test HTML visualizer. Single-crate Rust CLI
(`cute4dbt`, lib + bin). The full architecture invariants and the
load-bearing decisions are in [`AGENTS.md`](AGENTS.md) (imported above)
and [`ARCHITECTURE.md`](ARCHITECTURE.md). This file adds Claude-specific
operating notes.

## Quick architecture mental model

- **`src/domain/`** — owned data + pure computation. `Manifest`, `Node`,
  `UnitTest`, `CteGraph`, `ModifiedSet`, `PreflightError`,
  `StateComparator`, `StateModifier`. No I/O, no parser deps, no clap.
- **`src/ports/`** — one trait in v0.1: the manifest source (real file vs
  in-memory test fixture). Ports earn their keep only with >1 impl.
- **`src/adapters/`** — serde manifest reader, sqlparser CTE engine,
  askama renderer, asset-inlining infra.
- **`src/cli/`** — clap derive, ExitCode mapping, the named run loop:
  `scope → preflight_compiled → parse_ctes → render`.
- **`src/main.rs`** — thin entry; parses args, calls `cli::run`, maps
  `ExitCode`.

## Phased roadmap (v0.1 → v1.0)

| Phase | Adds                                                                          | Release? |
|-------|-------------------------------------------------------------------------------|----------|
| v0.1  | Walking skeleton: domain + ingestion + StateComparator + fail-closed + CTE engine + askama render + zero-egress gate + ATDD | No — git tag only |
| v0.2  | `-- @desc` per-CTE descriptions + collapsible raw-SQL drawer (tokenizer + CommentMap seam); StateComparator sub-selectors | No |
| v0.3  | Performance at scale (large-manifest pagination, optional lazy renderer); markdown / JSON export modes | No |
| v0.4+ | Cross-tool integration (cute-dbt scorecard composite action, mokumo consumer if relevant) | No |
| **v1.0** | API + CLI surface + JSON envelope stabilize; first crates.io publish + binstall path | **YES** |

## Commit convention

```
feat(domain):    feat(ports):    feat(adapters):    feat(cli):
fix(domain):     test:           ci:                docs:           chore:
adr:             closeout:
```

## Run-loop sketch (the v0.1 vertical)

```text
1. cli::parse_args                       -> AnalysisConfig
2. adapters::manifest::load              -> Manifest          (Stage-1 preflight)
3. adapters::manifest::load_baseline     -> Manifest
4. domain::state::modified_set           -> ModifiedSet       (StateComparator)
5. domain::scope::in_scope_unit_tests    -> Vec<UnitTest>
6. domain::preflight::compiled_required  -> ()                (Stage-2 preflight)
7. adapters::cte_engine::extract         -> CteGraph (per in-scope model)
8. adapters::render::report              -> Html
9. cli::write_out                        -> ExitCode 0 | non-zero with remediation
```

Steps 2–3 raise `PreflightError::{Unreadable,SchemaUnsupported,BaselineUnusable}`.
Step 6 raises `PreflightError::NotCompiled { node_id, unit_test }`. Missing
`--baseline-manifest` is a **clap usage error** raised before step 1 returns,
not a `PreflightError`.

## Property test invariants

| Function | Key invariants |
|----------|---------------|
| `BodyChecksumModifier::is_modified` | `None` baseline ⇒ true (new node modified); reflexive (`is_modified(n, Some(n)) == false`); symmetric in equality (`a.checksum == b.checksum` ⇒ both directions agree) |
| `StateComparator::modified_set` | Union semantics: a node is in the set if ANY modifier returns true; empty modifier vec ⇒ empty set |
| In-scope selection | `target_model ∈ modified_set ∨ self ∈ modified_set ⇒ in_scope` |
| CTE engine | Acyclic graph emitted; every edge carries a `JoinType` from the v0.1 vocabulary; the legend lists every edge's `JoinType` |
| Render output | Zero `<script src>`, `<link href>`, `<img src>`, `@import`, `url()`, `//` resource refs in the generated HTML |

## Worktree setup

```bash
git worktree add ../cute-dbt-issue-N -b <area>-<issue>-<slug>
```

`<area>` = the issue title's prefix slug (e.g. `domain`, `adapters`, `cli`,
`infra`, `docs`, `test`).

## Compact instructions

Preserve: single-crate hexagonal discipline + ADR-1 conscious non-mirrors;
two-stage fail-closed contract + the four `PreflightError` variants; the
baseline-required CLI policy; StateComparator union semantics; asset-inlining
+ Mermaid UMD constraints; the headless `file://` zero-egress gate; the
PHI-safe fixture invariant.

Discard: full file contents from old reads, search results not acted on,
completed PR details, intermediate API-shape deliberations already encoded
in `ARCHITECTURE.md`.
