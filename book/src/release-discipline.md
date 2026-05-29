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
--help`), the **exit-code mapping**, and the **zero-egress property** — not
the Rust library, which stays internal during v0.x. Contributor-facing
release mechanics (conventional-commit version inference, tag policy) live
in
[`AGENTS.md`](https://github.com/breezy-bays-labs/cute-dbt/blob/main/AGENTS.md#release-discipline).
