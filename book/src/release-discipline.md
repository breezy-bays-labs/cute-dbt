# Release discipline

cute-dbt is published to crates.io from **`v0.1.0`+** via
[`release-plz`](https://release-plz.dev/) with OIDC trusted publishing —
no long-lived registry token, no manual `cargo publish`.

What you need to know as a user:

- **Install:** `cargo install cute-dbt` (from `v0.1.0`+).
- **SemVer, with a v0.x caveat.** Per Cargo convention, while cute-dbt is
  `0.x` a **minor** bump (`0.1 → 0.2`) MAY break — CLI flags, output shape,
  or exit codes can change; patch bumps (`0.1.0 → 0.1.1`) are bug-fix or
  additive only. So **pin to a minor** (`cute-dbt = "0.1"`) and skim the
  CHANGELOG before moving to `0.2`. `v1.0` ships the first stability
  commitment.
- **The CHANGELOG is canonical** —
  [`CHANGELOG.md`](https://github.com/breezy-bays-labs/cute-dbt/blob/main/CHANGELOG.md).
- **Yanks** happen only for security or licensing issues; cute-dbt never
  amends a published version, it ships the next one.

The contract SemVer covers at `v1.0`+ is the **CLI surface** (`cute-dbt
--help`), the **exit-code mapping**, the **zero-egress property**, and the
**explorer's external-drive JS contract** (the
[`window.focusModel`/`window.setView` hooks, the dual-bound commit signal,
and the payload-paths shape](./explore-contract.md)) — not the Rust
library, which stays internal during v0.x. The external-drive contract
carries its own readable version string (`data-cute-dbt-contract` /
`window.cuteDbtContract.version`) so embedding hosts can feature-detect,
but it is governed **here**, not by a separate versioning system: a
contract-breaking change is a v0.x minor (v1.0+ major) event, exactly
like a CLI flag rename. Contributor-facing release mechanics
(conventional-commit version inference, tag policy) live in
[`AGENTS.md`](https://github.com/breezy-bays-labs/cute-dbt/blob/main/AGENTS.md#release-discipline).
