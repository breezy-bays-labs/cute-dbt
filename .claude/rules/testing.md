# Testing Overlay — cute-dbt

> **Thin tool-map.** This file says *which tool* answers each universal
> test level in cute-dbt, and *how mature* that rung is. It does **not**
> restate the rationale. The canonical model — *which level, why, the
> three axes, the Boundary Rule, the four quadrants, the five-leg health
> dashboard, the maturity ladder, enforcement* — lives in
> `~/Github/ops/standards/testing-strategy.md` (the taxonomy spine). The
> always-injected meta-principles are in
> `~/.claude/rules/testing-framework.md`. Read those for the *why*; read
> this for the *what*.

cute-dbt is a **single-crate Rust CLI** that *ingests* dbt artifacts (it
does not run dbt). Its own tests therefore map onto the **Rust column** of
the per-stack table (§6 of the canonical doc) — plus a distinctive
**browser-E2E leg** the Rust column doesn't fully name, and an unusually
**strong fitness-function inventory**. Status vocabulary is the canonical
ladder: **shipped** (in CI, gating) · **in-progress** · **planned** ·
**aspirational** · `—` (unclimbed).

## Universal levels → cute-dbt tools

| Level | Tool | Status | Notes |
|-------|------|--------|-------|
| **Unit** | `cargo nextest` | **shipped** | ~2040 tests across domain POD, render, adapter, cli. |
| **Property** | hand-rolled **exhaustive structured enumeration** (no `proptest` crate) | **shipped** | The property *invariants* (JSON serde round-trip; StateComparator union semantics over the 2⁴×2⁵ kind×facet cube; exhaustive struct-attr coverage) are pinned — but by a deliberate **"exhaustive over sampling, no proptest dev-dep"** house style, not generative `proptest`. A bounded domain is enumerated in full, which is strictly stronger than sampling for these spaces. AGENTS.md / CLAUDE.md require these invariants; this row records *how* they're discharged. |
| **Fuzz** | `cargo-fuzz` / `bolero` | **aspirational** `—` | No fuzz target yet. The honest first-pilot candidates are the untrusted-input parsers: the **manifest JSON reader**, the **hand-rolled RFC-4180 CSV parser**, and the **`--pr-diff` patch parser**. This is the org-wide Q4 default blind spot — cute-dbt fits it exactly. |
| **Integration** | `cargo nextest` integration tests under `tests/` | **shipped** | Real fixtures, exit-code contract, shelling the built bin (e.g. `changed_files_provider.rs`, `path_matching.rs`, `review_cli.rs`, `run_loop.rs`). |
| **Acceptance (BDD)** | `cucumber-rs` (`cargo test --test bdd`, `harness = false`) | **shipped / in-progress** | 30 `.feature` files / ~219 scenarios — the executable product spec. NOT nextest-compatible (`harness = false`); the `mutants`/default nextest profiles exclude the `bdd` binary by design. |
| **End-to-end / smoke** | shell the built binary; **headless_chrome** browser-E2E (see below) | **shipped** | The CLI's own exit-code/output contract is exercised by the integration suites; the rendered artifact is exercised by the browser leg. |
| **Coverage** | `cargo llvm-cov` (lcov) | **shipped** | — |
| **Risk (CRAP)** | `crap4rs` scorecard | **shipped** | `crap4rs.toml`: default ≤ 25; **strict ≤ 15** on the correctness-load-bearing modules (`state.rs`, `preflight.rs`, `manifest.rs`, `cte_engine.rs`, `asset_embed.rs`, `render.rs`, pr-diff). |
| **Mutation** | `cargo-mutants` | **shipped (targeted)** | Focused on the load-bearing modules (`cell_diff.rs`, `unit_test_table.rs`, the strict-keyed set) — *not* whole-suite. nextest kill-harness (`--profile mutants`); survivors carry tracking issues per the exclusions rule. |
| **Fitness functions** | many (see inventory) | **shipped — strong** | cute-dbt's standout leg. See the inventory below. |
| **Performance** | `criterion` | **aspirational** `—` | No benchmark gate. The other half of the org-wide Q4 blind spot. |

## Browser-E2E leg (cute-dbt-distinctive)

The Rust column of the canonical table doesn't name a browser leg;
cute-dbt has one because its deliverable is a **self-contained HTML
report**. **`headless_chrome`** drives a real Chromium via CDP against the
generated `report.html` over a real `file://` URL:

- **`tests/headless_zero_egress.rs`** — the **core auditability E2E**:
  opens the report with all network access denied and asserts **zero
  requests** (filter CDP `Network.requestWillBeSent` to http/https/ws/wss
  → empty). This is the product's load-bearing privacy promise made
  executable, re-runnable by anyone with the repo checked out.
- **`tests/headless_toggle.rs`** — introspects the **live** Cytoscape
  instance (`window.CuteCyto.cyInstance()` / `window.CuteExploreLineage`)
  and drives real clicks (settings-panel engine picker, view toggles).
  #371 extends this with rendered-geometry asserts.

On the canonical map this leg is `(system, example, verification)` —
end-to-end scope, and simultaneously the mechanical enforcer for the
zero-egress *fitness function*.

## Fitness-function inventory (the strong leg)

All run as always-on CI jobs (most mirrored in `lefthook.yml` pre-push).
These are §5 meta-layer **fitness functions** — structural invariants, not
product tests:

- **non-mirror-guard** — rejects a `[workspace]` table, `bans.deny.wrappers`
  in `deny.toml`, and the `pub use crate::…::…` API-shim pattern (the three
  conscious-simplification tripwires).
- **Resource-ref lint** — the zero-egress *structural* gate: rejects real
  loading constructs (`<script src>`, `<link href>`, `<img src>`,
  CSS `@import` / `url()`, protocol-relative `//`) in the emitted HTML.
  (The headless network-block test above is the *behavioral* twin.)
- **Synthetic-only fixture gate** — every file under `tests/fixtures/` must
  be listed in `MANIFEST.toml` with origin/SHA-256/`synthetic_only = true`.
- **Asset-provenance gate** — same shape over `assets/MANIFEST.toml` (the
  vendored frontend bundle's pin + SHA-256 + SPDX index).
- **EdgeType completeness guard** — every `EdgeType`/`JoinType` variant is
  covered by the render layer (no silent legend gap).
- **Heuristics-ledger-from-SPECS** — the coverage-check ledger is generated
  from the specs and diffed; drift fails.
- **Byte-identity golden gate** — committed example reports
  (`jaffle-shop`, `playground`, `diff-showcase`) are byte-identical to
  renderer output.
- **Baseline-required invariant** — every `report` feature line passes
  exactly one scope source (`--baseline-manifest` XOR `--pr-diff`) unless
  tagged `@no-baseline-usage-error`; a zero-match tripwire fails loudly if
  the trigger prose drifts.
- **Feature-count mirror** — the `.feature` count is asserted against a
  single `expected=` source of truth.
- **Domain hexagonal-purity** (`tests/domain_clean_arch.rs`) — fails the
  build if any `src/domain/**` line imports `crate::adapters` / `crate::cli`
  or an adapter-layer crate (`sqlparser`, `askama`, `clap`, `dbt_yaml`).
- **MSRV**, **`clippy --all-targets --locked -- -D warnings`**, **`cargo fmt
  --check`**, **`cargo-deny`** (advisories + licenses + bans), **`cargo doc`
  `-D warnings`** — the standard Rust gate battery.

## Quadrant read (where the blind spots are)

| Quadrant | cute-dbt status |
|----------|-----------------|
| **Q1** (unit / integration) | **strong** — nextest unit + integration suites. |
| **Q2** (acceptance / BDD) | **strong** — 30 features / ~219 scenarios as the executable spec. |
| **Q3** (exploratory / UAT) | **manual today** — the founder dogfoods each release; the orchestrator runs live Playwright probes ad-hoc. Not yet a deliberate pass. |
| **Q4** (fuzz / perf / security) | **WEAK** — the framework's named default blind spot, and cute-dbt fits it exactly: **no fuzz target** on the three parsers, **no performance budget**. The zero-egress headless test is the one Q4-adjacent (security) leg, and it is load-bearing. |

## Five-leg health (the §7 dashboard, cute-dbt instance)

1. **Coverage** — `cargo llvm-cov`, **shipped**.
2. **Risk (CRAP)** — `crap4rs`, default ≤ 25 / strict ≤ 15, **shipped**.
3. **Mutation** — `cargo-mutants`, **shipped (targeted)** on the
   load-bearing set.
4. **Taxonomy inventory** — partially mechanized: the BDD hygiene gates
   (baseline-required, feature-count, heuristics-ledger) seed it, but no
   mis-leveling lint yet (the canonical §9 mechanizable slice — an
   acceptance scenario asserting only a private symbol — is **review-time
   judgment** here).
5. **Fitness functions** — **strong**, see inventory above.

## Bring-into-shape (prioritized)

- **Wire-now (highest value):** a first **`cargo-fuzz`/`bolero`** target on
  the **`--pr-diff` patch parser** or the **RFC-4180 CSV parser** — the
  highest-risk untrusted-input surface, and the org's named first fuzz
  pilot. This is the single most valuable Q4 move.
- **Defer (aspirational):** `criterion` performance budgets — no
  established hot path under a regression ceiling yet.
- **Watch:** the **mis-leveling** Boundary-Rule call stays review-time —
  keep BDD scenarios that assert only a private symbol out of the
  `.feature` corpus (push them down to unit tests).

---

*Maintenance:* when a rung's status changes (e.g. the first fuzz target
lands), update the row here **and** the cute-dbt entry in
`~/Github/ops/standards/quality-manifest.md`. A rung never moves backward
without an ADR (canonical §8).
