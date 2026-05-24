# cute-dbt — Architecture

This document is the public derivation of the cute-dbt v0.1 architecture: the
single-crate hexagonal layering, the conscious design simplifications, the
two-stage fail-closed contract, the `StateComparator` strategy, and the
asset-inlining + zero-egress gate. The synthetic-only-data invariant for
fixtures, snapshots, and `.feature` examples lives in
[`CONTRIBUTING.md`](CONTRIBUTING.md#synthetic-only-fixtures) (human
contributors) and [`AGENTS.md`](AGENTS.md#synthetic-only-fixtures) (AI
agents); the structural mechanism (`tests/fixtures/MANIFEST.toml` + a
`cargo test` listed-file gate) is the same shape as §5's
`assets/MANIFEST.toml`. The canonical source for each architectural
decision is the project's decision records (ADR-1 through ADR-5); this
file translates those decisions into a public-repo narrative that does
not require access to the private records to read or audit.

The `.feature` files under [`features/`](features/) are the **executable
acceptance contract** (cucumber-rs ATDD outer loop, automated in PR 10);
this document is the *structural* contract that supports them.

## 1. Single-crate hexagonal layout

cute-dbt is a **single-crate** Rust CLI (`cute4dbt`, lib + bin). `Cargo.toml`
declares one package and one set of dependencies; there is no `[workspace]`
table.

```
src/
├── domain/          # owned data + pure computation; std + serde derive only
├── ports/           # trait seams with >1 real-or-test impl (v0.1: manifest source)
├── adapters/        # serde manifest reader, sqlparser CTE engine, askama renderer
├── cli/             # clap derive, ExitCode mapping, run-loop composition
└── main.rs          # thin entry
```

**Dependency direction is inward only:**
`domain → ports → adapters → cli`. No layer imports inward. `domain` may
import `std` and `serde` (derive) only — no parser libs, no `clap`, no
`askama`, no I/O.

Because cute-dbt is a single crate, this discipline is enforced by **module
convention + `clippy` + review**, not by Cargo crate boundaries (a single
crate cannot fail to compile on an inward `use`). The single-crate choice
is itself an architectural decision — see §2.

**`ports/` are introduced only where there is more than one real-or-test
implementation** that the run loop must select between. v0.1 has exactly one
such seam (the manifest source: real file vs in-memory test fixture);
everything else (CTE engine, renderer, config loader) is a free function or
a concrete adapter struct called directly. The renderer is *not* a port —
v0.1 has one output format (HTML); `--format json` is explicitly v0.2+.

**`domain/` is POD-only** — owned data with constructors, no method
machinery beyond what the run loop calls. This keeps the model trivial to
build in tests from literals.

**AST-derived structural facts flow through the domain as POD.** Adapters
that parse external grammars (the `sqlparser` CTE engine, the
serde-on-dbt-schema manifest reader) precompute the structural facts
the downstream layers need and write them back as POD fields on the
relevant domain type. Example: `CteNode::is_simple_from_shape` and
`CteNode::body_leaf_table_refs` are populated by the CTE engine during
the existing single-parse pass — they are POD (`bool` and
`Vec<String>`); the renderer reads them via accessors and never holds a
parser, an AST reference, or re-parses the raw SQL slice
(cute-dbt#40). New facts of this kind are additive POD fields with
`#[serde(default)]`. No domain layer ever pulls in `sqlparser`. This is
the data-flow echo of the inward-dependency discipline: the single
parser pass in the adapter is the single source of truth for everything
downstream of it.

## 2. Conscious design simplifications

cute-dbt is a single artifact (one binary, one product) with HTML-primary
output and exactly one parser in the dependency graph. Several pieces of
common Rust apparatus exist for projects whose shape cute-dbt does not have
— multi-crate workspaces for crates with multiple linkage-level consumers,
public-API shims for library consumers, AST-purity bans for shared cores
with rival adapter parsers, JSON wire envelopes for machine-readable output.
cute-dbt deliberately does **not** adopt them. The absences are documented
architectural choices, not accidents — recording them stops a future
contributor (human or agent) from "completing the pattern" by adding
machinery that guards no invariant here.

| # | Apparatus | cute-dbt | Why N/A | Enforcement |
|---|---|---|---|---|
| 1 | Multi-crate Cargo workspace + per-crate `Cargo.toml` | Single crate `cute4dbt` (lib + bin) | No second linkage-level consumer in the v0.x horizon. A workspace exists to serve >1 crate; importing the apparatus here would be a project-value violation (R7: "not overly complex"). | **CI:** `non-mirror-guard` job rejects a `[workspace]` table in `Cargo.toml`. |
| 2 | Per-crate independent versioning | Single artifact version | Moot — one crate, one version. The release cadence is whole-product, not per-component. | Absence (no second crate to version independently). |
| 3 | `public-api-shim` re-export pattern (`pub use crate::…::…` from `lib.rs` curating a stable surface for library consumers) | None | The binary is the product; there are no library consumers to shield from internal renames. An API shim with no consumer would add indirection that guards nothing. | **CI:** `non-mirror-guard` job rejects `pub use crate::…::…` in `src/lib.rs`. |
| 4 | AST-purity `cargo-deny` bans + `ast-purity` CI grep (keep adapter AST libraries out of a shared core) | None | The AST-purity invariant exists to protect a *shared* core crate from adapter parser dependencies when several adapter crates each pull in different AST libraries. cute-dbt has exactly one parser (`sqlparser-rs`) and one consumer of it; there is no shared core and no rival AST surface — no invariant to enforce. (Bonus: `bans.deny.wrappers` is fragile under proc-macro dependency chains; this project never needs to depend on that mechanism.) | **CI:** `non-mirror-guard` job rejects `bans.deny.wrappers` in `deny.toml`. |
| 5 | `nested-json-envelope` ADR for wire output | None | Output is HTML-primary, single self-contained file. There is no JSON wire envelope to version. `--format json` is explicitly deferred to v0.2+; if it lands it will be a new ADR, not a retroactive shim. | Absence (no JSON output in v0.1). |
| 6 | `proc-macro2 span-locations` toolchain gate | None | No direct `proc-macro2` dependency. The `sqlparser` tokenizer carries its own spans, and the tokenizer pass itself (for `-- @desc` per-CTE breakdowns) is v0.2-deferred. There is no proc-macro span-precision invariant in scope to gate. | Absence (no `proc-macro2` direct dep). |

**Enforcement layering.** Three of the six rows have a literal CI grep
backing them — rows 1, 3, and 4 are guarded by the
[`non-mirror-guard`](.github/workflows/ci.yml) job, which rejects the
specific tripwires (`[workspace]`, `pub use crate::…::…`,
`bans.deny.wrappers`) that would silently reintroduce the apparatus. Rows
2, 5, and 6 are enforced by **absence** — there is no second crate to
version, no JSON output path, and no `proc-macro2` direct dependency, so
the apparatus cannot be added incidentally; adding any of them would
require a discrete code change visible at review.

This is deliberate. The strongest tripwires get CI; the absence-enforced
ones get this section.

## 3. Two-stage fail-closed contract

Fail-closed inputs (a `dbt parse`-only manifest, a pre-1.8 manifest, an
unreadable manifest, an unusable baseline, or a `dbt parse`-only target
model for an in-scope unit test) produce a non-zero exit and no HTML.
There is *never* a partial report. Detection is split into two stages
because requiring `compiled_code` for *every* node at load would wrongly
reject a manifest that is fine for the diff-scoped subset:

- **Stage 1 — schema-level pre-flight at the manifest adapter** runs on
  load, before the domain sees the manifest. Raises:
  - `Unreadable { detail }` — file missing, not JSON, or missing required
    keys.
  - `SchemaUnsupported { found, minimum }` —
    `metadata.dbt_schema_version` is below the dbt ≥ 1.8 floor.
  - `BaselineUnusable { detail }` — `--baseline-manifest` was supplied but
    is unreadable or mismatched.
- **Stage 2 — semantic compiled-SQL-presence check in the domain** runs
  *after* the `StateComparator` selects the in-scope set (§4). Raises:
  - `NotCompiled { node_id, unit_test }` — an in-scope unit test's target
    model has `compiled_code: null` (the `dbt parse` case). Only in-scope
    models are checked; an out-of-scope uncompiled model is not a fail
    condition.

### The error type

`PreflightError` is a `#[non_exhaustive]` enum with **exactly four**
variants — `Unreadable`, `SchemaUnsupported`, `BaselineUnusable`,
`NotCompiled`. New fail-closed reasons are additive (the enum is forward-
compatible per the *enums-yes-structs-no* rule for public pattern-matched
types). Remediation strings live with the CLI exit-code mapping, not on
the enum.

### Baseline-missing is NOT a `PreflightError` variant

`--baseline-manifest` is a **required CLI argument**, declared on the clap
parser. Omitting it is a **clap usage error** — raised **before any
manifest is read** — not a `PreflightError`. Conflating usage-time errors
(the operator misused the CLI) with runtime preflight errors (the input
data is unusable) would muddy the contract and add a fifth enum variant
that the CLI exit-code mapping would have to route differently from the
other four. Don't add it.

Locked policy consequences:

- Baseline present, no in-scope changes → **exit 0** with a valid (small)
  report; the diff-scope banner reads `0 unit tests in scope`. Empty-but-
  valid; fail-closed is reserved for *unusable* input, never *empty*
  scope. The banner text is exposed as a single shared constant referenced
  by both the CLI banner code path and the report template, to prevent
  CLI/template drift.
- Stage 2 remains narrow — it only inspects in-scope models. A full-
  manifest overview is a documented trick (diff against an empty/genesis
  baseline), not a separate code path.

### The named run loop

The composition lives in `cli` as four named call-sites:

```
scope → preflight_compiled → parse_ctes → render
```

Each stage is greppable. The fail-closed contract has clean seams the
`.feature` scenarios can assert against without depending on internal
implementation detail.

## 4. `StateComparator` strategy

dbt's `state:modified` is the diff-scope selector — cute-dbt is PR-review-
first, so output is scoped to the unit tests whose target model body
changed (or whose test definition itself changed). v0.1 ships honest
body-checksum fidelity; sub-selectors (`.configs` / `.relation` /
`.macros` / `.contract`) arrive in v0.2+ as additive trait impls.

**`StateComparator` is a domain strategy, not a port.** It is pure
computation over two already-parsed domain manifests with no I/O. Putting
it behind a port would imply an external implementation that does not
exist. (Hexagonal discipline from §1: ports are for I/O or polymorphic
seams; strategies are domain.)

```rust
// domain/state.rs — pure; no I/O; lives next to the manifest model.
pub trait StateModifier {                       // object-safe; NOT Send + Sync
    fn kind(&self) -> ModifierKind;             // Body | Configs | Relation | Macros | Contract
    fn is_modified(&self, current: &Node, baseline: Option<&Node>) -> bool;
}

pub struct BodyChecksumModifier;                // the ONLY v0.1 impl
pub struct StateComparator { modifiers: Vec<Box<dyn StateModifier>> }
```

`StateComparator::body_only()` constructs the v0.1 default. `modified_set()`
applies **OR-union semantics** across registered modifiers (matching dbt's
behavior across sub-selectors).

**Object-safe, deliberately not `Send + Sync`.** v0.1 scoping is single-
threaded; bounds add at a call site if parallelism ever arrives. A
`#[cfg(test)] assert_obj_safe!` pins object-safety so a future
generic-method addition that breaks `Box<dyn StateModifier>` fails the
build, not review.

**In-scope unit-test selection** = unit tests whose target model is in
`modified_set`, **unioned with** unit tests whose own node is in
`modified_set` (a changed test on an unchanged model is in scope).

### v0.1 fidelity limit (named, not silent)

Body-checksum misses a pure `.configs` or `.contract` change. The README,
the diff-scope banner, and a tracking issue at the `StateComparator` site
all name this limit. It is not a defect — it is the v0.1 scope. Adding a
sub-selector is a single additive `impl StateModifier` block plus
registration in a `StateComparator::with_sub_selectors(...)` constructor;
the comparator, the domain model, and the scoping step do not change.

If a sub-selector ever needs data the parsed `Node` does not carry, that
is a manifest-ingestion additive change, not a `StateComparator` redesign.

## 5. Asset embedding (zero-egress gate)

The adoption gate is *trivially auditable* zero data exfiltration: the
generated report makes zero outbound requests when opened offline. This is
made structurally true by embedding every vendored frontend asset into
the binary at compile time and emitting them inline in the single HTML
file.

### Build constructs

- **Embedding:** every v0.1 asset is text (Sakura CSS, jQuery, DataTables
  JS/CSS, the Mermaid UMD bundle), so each is embedded with `include_str!`
  at compile time — the bundle carries no binary asset and no
  `include_bytes!` user. Asset bytes land in the binary's `.rodata`
  section; **there is no runtime asset directory and no code path that
  fetches them.** The askama template interpolates them inside
  `<style>` / `<script>` blocks with the `|safe` filter.
- **Mermaid:** pinned 11.x, vendored `dist/mermaid.min.js` (**UMD bundle,
  never the ESM `type="module"` variant**). Initialized inline with
  `securityLevel: 'strict'` and an explicit non-webfont system
  `fontFamily` stack:
  ```
  mermaid.initialize({
    startOnLoad: true,
    securityLevel: 'strict',
    fontFamily: 'system-ui,-apple-system,"Segoe UI",sans-serif'
  })
  ```
  The system-font stack suppresses Mermaid's default Google Fonts fetch
  (proven empirically in the R1 spike); without it the report would emit
  a network request when opened in a browser with networking allowed.
- **Favicon:** an empty `data:` URI favicon, emitted as
  `<link rel="icon" href="data:,">`, so the browser's automatic favicon
  request resolves in-document and never leaves it. Reinforces the
  "literally zero requests" story.

### Vendored-asset provenance

`assets/MANIFEST.toml` records every vendored asset with:

- `name` — the asset's library identifier (e.g. `mermaid`)
- `version` — the pinned upstream version
- `path` — the asset's filename within `assets/`
- `source` — the canonical upstream URL the bytes were fetched from
- `sha256` — the SHA-256 of the vendored file
- `license` — SPDX identifier (all MIT/BSD/Apache-compatible)

`cargo-deny` covers crate-level supply-chain provenance; the asset
manifest covers the embedded frontend bundle. Together they are the
supply-chain artifact any auditor can read directly. The update flow is bounded:
bump version → re-download → update `sha256` + `license` in
`MANIFEST.toml` → the headless-network test (below) re-validates
zero-egress on the new bundle.

### Zero-egress gate (primary)

The **headless-browser network-block test** opens the generated
`report.html` via a real `file://` URL with all network access denied and
asserts **zero requests**. This is the R1-spike method; it is re-runnable
by anyone with the repository checked out; it is the strongest auditability artifact.
It is a required CI gate on every PR to `main`; the test crate is
[`tests/headless_zero_egress.rs`](tests/headless_zero_egress.rs).

The proof is invalid against a `127.0.0.1` loopback origin — the report
must be tested against a real `file://` URL, the same way an operator
opens it. This is a hard gate condition, not a stylistic preference.

### Zero-egress gate (secondary)

A **structured resource-ref lint** targets *real loading constructs* over
the generated HTML — `<script src>`, `<link href>`, `<img src>`,
CSS `@import`, CSS `url()`, protocol-relative `//`. It uses the `tl`
HTML parser (zero-dep, sufficient for attribute extraction),
**never raw `grep http`.** Minified bundles carry hundreds of inert URL
string literals (the R1 spike confirmed this empirically); a raw grep is
false-positive noise that would hide the real signal under more text than
the headless test's clear zero-requests output. The test crate is
[`tests/resource_ref_lint.rs`](tests/resource_ref_lint.rs).

### Auditability index

[`AUDIT.md`](AUDIT.md) is the one-page index of every artifact a
reviewer can re-run — the headless command, the resource-ref lint,
`assets/MANIFEST.toml`, `tests/fixtures/MANIFEST.toml` (see
[`CONTRIBUTING.md`](CONTRIBUTING.md#synthetic-only-fixtures) and
[`AGENTS.md`](AGENTS.md#synthetic-only-fixtures) for the fixture-hygiene
rule), `deny.toml`, `Cargo.lock`. See [`SECURITY.md`](SECURITY.md) for
the plain-language version of this story.

## 6. Composition note — the run loop lives in `cli`

The `scope → preflight_compiled → parse_ctes → render` run loop is
composed in `cli`. This is a **conscious single-crate composition
choice**, not an accident — the alternative (a separate `app/` or
`usecase/` crate that owns composition) is a multi-crate pattern that
would impose workspace machinery (§2 row 1) this project does not need.

If cute-dbt ever splits into multiple crates, the run loop migrates to an
`app` or `usecase` crate at that point. Until then, `cli` owns clap
wiring, `ExitCode` mapping, *and* run-loop composition — and that is the
right level of indirection for one consumer.

## 7. Acceptance contract

The five `.feature` files under [`features/`](features/) are the
executable acceptance contract:

- `report_generation.feature`
- `diff_scoping.feature`
- `cte_rendering.feature`
- `fail_closed.feature`
- `zero_egress.feature`

They cover the success criteria (manifest → offline-correct report,
per-test header + Given/Expected panels + edge-colored CTE DAG + banner,
fail-closed paths, zero-egress proofs). The cucumber-rs step definitions
and the `cargo test --test bdd` harness land in PR 10. The
[`features/README.md`](features/README.md) is the source of truth for the
SC-to-scenario mapping; scenarios are not listed inline here to keep this
document and the spec files from drifting.

Three CI invariants pin the feature-spec contract — see
[`.github/workflows/ci.yml`](.github/workflows/ci.yml):

- `feature-count` asserts exactly **5** `.feature` files exist (adding a
  sixth requires a deliberate update).
- `baseline-required-grep` asserts every scenario invoking the CLI passes
  `--baseline-manifest`, except scenarios tagged
  `@no-baseline-usage-error` (the one intentional exception that
  exercises the clap usage-error path itself).
- `non-mirror-guard` (§2) preserves the architectural non-mirrors over
  time.

## Cross-references

- [`SECURITY.md`](SECURITY.md) — plain-language zero-egress + privacy
  statement (non-engineer-readable companion to §5)
- [`AGENTS.md`](AGENTS.md) — cross-provider agent operating guide
- [`CLAUDE.md`](CLAUDE.md) — Claude-specific entry point
- [`CONTRIBUTING.md`](CONTRIBUTING.md) — developer workflow
- [`README.md`](README.md) — what cute-dbt does and why your data stays
  on your machine
- [`features/README.md`](features/README.md) — acceptance-spec map
