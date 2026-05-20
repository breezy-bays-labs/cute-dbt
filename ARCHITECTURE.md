# cute-dbt — Architecture

> **Skeleton.** The full neutralized derivation (single-crate hexagonal
> discipline + conscious non-mirrors + two-stage fail-closed + StateComparator
> strategy + zero-egress gate + PHI-safe fixture invariant) lands in PR 2.
> The genesis commit ships this skeleton with the load-bearing pointers so
> downstream PRs have a stable target for cross-references.

## Topology

Single-crate Rust CLI (`cute4dbt`, lib + bin), hexagonal **inward-dependency
discipline only**:

```
src/
├── domain/          # owned data + pure computation; depends on std + serde derive only
├── ports/           # trait seams with >1 real-or-test impl (v0.1: manifest source)
├── adapters/        # serde manifest reader, sqlparser CTE engine, askama renderer
├── cli/             # clap derive, ExitCode mapping, run-loop composition
└── main.rs          # thin entry
```

Dependency direction: `domain → ports → adapters → cli`. No layer imports
inward. Enforced by module convention + clippy + review (a single crate
cannot fail to compile on an inward `use`, so the discipline is editorial,
not Cargo-mechanical).

The run loop is named in `cli`:
`scope → preflight_compiled → parse_ctes → render`. Application
orchestration lives in `cli` as a conscious single-crate composition
choice — not an accident.

## Conscious non-mirrors (filled in PR 2)

The sibling sensor tools (`dry-rs`, `scrap-rs`, `crap4rs`) carry apparatus
that this single-crate project deliberately does not adopt:

- No multi-crate workspace — single linkage-level consumer.
- No per-crate independent versioning — one artifact.
- No public-API shim — no library consumers; the bin is the product.
- No AST-purity `cargo-deny` bans or `ast-purity` CI grep — one parser,
  one consumer, no invariant to enforce.
- No nested-JSON-envelope ADR — HTML-primary output; `--format json` is
  v0.2+.

The full rationale (per-row Y-statement) lands in PR 2. The
`non-mirror-guard` CI job rejects future additions of any of these.

## Two-stage fail-closed (filled in PR 2)

`PreflightError` is a `#[non_exhaustive]` enum with four variants:
`Unreadable` and `SchemaUnsupported` raised by the manifest adapter at load
(Stage 1); `BaselineUnusable` raised by the manifest adapter when
`--baseline-manifest` is supplied but unusable (also Stage 1);
`NotCompiled { node_id, unit_test }` raised by the domain compiled-SQL
presence check *after* StateComparator scoping, only for in-scope unit
tests (Stage 2). Missing `--baseline-manifest` is a clap usage error
raised before the manifest is read — not a `PreflightError` variant.

## StateComparator strategy (filled in PR 2)

`StateModifier` is an object-safe trait (deliberately NOT `Send + Sync`).
`StateComparator` holds `Vec<Box<dyn StateModifier>>` with OR-union
semantics matching dbt. v0.1 ships exactly one impl,
`BodyChecksumModifier`. Sub-selectors (`.configs`/`.relation`/`.macros`/
`.contract`) are future `impl StateModifier` blocks — never a
restructuring of the comparator.

## Zero-egress gate (filled in PR 2)

Assets embedded at compile time via `include_str!`/`include_bytes!` into
`.rodata`, emitted through askama with `|safe`. Mermaid is the UMD
bundle (NOT ESM), `securityLevel: 'strict'`, system `fontFamily`,
`data:` favicon. Primary CI gate: headless-browser network-block test
opening the generated `report.html` via real `file://`. Secondary:
structured resource-ref lint over real loading constructs (`<script
src>` / `<link href>` / `<img src>` / `@import` / `url()` / `//`).

## PHI-safe fixture invariant (filled in PR 2)

Every committed fixture / `insta` snapshot / `.feature` example must be
synthetic or public-demo. Mechanically enforced via
`tests/fixtures/MANIFEST.toml` (every fixture lists origin + SHA-256 +
`synthetic_only = true`) + a `cargo test` that parses the manifest + a
CI grep that fails on any unlisted file under `tests/fixtures/`.

## Cross-references

- [`AGENTS.md`](AGENTS.md) — cross-provider agent operating guide
- [`CLAUDE.md`](CLAUDE.md) — Claude-specific entry
- [`CONTRIBUTING.md`](CONTRIBUTING.md) — developer workflow
- [`SECURITY.md`](SECURITY.md) — plain-language zero-egress + PHI-safe statement
