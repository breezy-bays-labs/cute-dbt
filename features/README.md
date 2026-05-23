# cute-dbt BDD acceptance specs

These five `.feature` files are the **ATDD outer loop** for cute-dbt v0.1.
Step definitions and the `cargo test --test bdd` harness land in PR 10
(#TBD) via cucumber-rs (`harness = false`; NOT nextest-compatible — set
`test_tool = "cargo"` in `.cargo/mutants.toml` to keep mutation testing
aware).

## Success-criteria mapping

| SC | Behavior | Spec |
|----|----------|------|
| SC1 | compiled dbt ≥1.8 manifest → offline-correct report | `report_generation.feature`, `zero_egress.feature` |
| SC2 | per in-scope test: header + Given/Expected panels + edge-colored CTE DAG + banner | `report_generation.feature`, `diff_scoping.feature`, `cte_rendering.feature` |
| SC3 | parse-only / partial / pre-1.8 → clear fail-closed error, no HTML | `fail_closed.feature` |
| SC4 | auditability package re-runnable by anyone | `zero_egress.feature` (headless-network proof + resource-ref lint). **Asset-manifest completeness is a CI invariant** (`cargo-deny` + `cargo test` over `assets/MANIFEST.toml`), NOT a scenario — same bucket as SC5/SC6. |
| SC5 | MIT, public, single crate, reproducible build | **Not a scenario** — verified by repo config + `cargo-deny` + committed `Cargo.lock` (CI gate). |
| SC6 | full quality/ATDD suite green in CI | **Not a scenario** — verified by CI pipeline existence (clippy pedantic, fmt, these features, insta, crap4rs, cargo-mutants, cargo-deny, lefthook). |

SC5/SC6 are repo/CI invariants, deliberately not Gherkin. Synthetic-only
fixture completeness is the same shape — enforced by
`tests/fixtures/MANIFEST.toml` + `cargo test` + CI grep.

## Conventions

- **Synthetic data only** in example tables (synthetic-only invariant). No
  real data, ever.
- **Observable behavior only**: scenarios assert exit code, file presence,
  DOM structure, network requests — never implementation detail.
- **`--baseline-manifest` is required**: every scenario invoking the CLI
  must pass it, except scenarios explicitly tagged
  `@no-baseline-usage-error` (the one intentional exception that
  exercises the usage-error path itself). The `baseline-required-grep`
  CI job enforces this structurally.

## File count is pinned (==5)

The `feature-count` CI job asserts exactly five `.feature` files exist
under `features/`. Adding a sixth feature requires updating the job.
