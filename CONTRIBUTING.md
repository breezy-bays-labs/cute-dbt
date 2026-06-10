# Contributing to cute-dbt

cute-dbt is a Breezy Bays Labs project. The repo is public for free GitHub
Actions and agent reviews; external contributions are welcome at v1.0+ once
the project ships its first crates.io release.

## Quick start

```bash
git clone git@github.com:breezy-bays-labs/cute-dbt.git
cd cute-dbt
lefthook install            # wires pre-commit + pre-push hooks
cargo build
cargo nextest run
```

`lefthook install` is one-time. After that, `cargo fmt --check` and
`gitleaks protect --staged --redact` run on every commit; the full pre-push
battery (fmt + non-mirror-guard + baseline-required-grep + feature-count +
pedantic clippy + nextest + cargo-deny + docs-as-errors) runs on every push
and matches CI exactly. See [`lefthook.yml`](lefthook.yml) for what each
hook runs.

## Development loop

| Step | Command |
|------|---------|
| Format | `cargo fmt --all` |
| Lint (with pedantic) | `cargo clippy --all-targets --locked -- -D warnings` |
| Test | `cargo nextest run --all-targets --locked` |
| BDD outer loop | `cargo test --test bdd` (cucumber-rs, `harness = false`; not nextest-compatible) |
| Coverage | `cargo llvm-cov nextest --locked --fail-under-lines 85` |
| Supply chain | `cargo deny check` |
| Doc lint | `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --document-private-items --locked` |
| Quick verify | `lefthook run pre-push` |
| Run binary | `cargo run -- --manifest <cur> --baseline-manifest <base> --out report.html` |

`#![warn(clippy::pedantic, clippy::cargo)]` lives at the crate root in
`src/lib.rs`, so `clippy -D warnings` enforces pedantic and cargo lints
automatically — no extra flag needed.

## Branch + PR

- Always branch off `main`; never push directly. The repo enforces this for
  ongoing work via branch protection (see [Branch protection](#branch-protection)
  below). The genesis commit is the one-time historical exception — an
  empty repo could not accept a PR targeting `main`.
- Use worktrees for parallel work:
  `git worktree add ../cute-dbt-issue-N -b <area>-<issue>-<slug>`.
- Title: `<conventional-prefix>(<area>): #<issue> — <one-liner>` (e.g.
  `feat(domain): #5 — StateComparator strategy`).
- Body: include `Closes #N` to link to the sub-issue.
- **1 PR closes exactly 1 sub-issue** (per the org issue-hierarchy convention).
  Never names like `PR2.a`, `Wave 1 PR-A`, `Stage 3.1`.

## Branch protection

`main` is protected via GitHub branch protection rules. The canonical
policy is committed at [`.github/branch-protection.json`](.github/branch-protection.json);
the live state is queryable at
`GET /repos/breezy-bays-labs/cute-dbt/branches/main/protection`.

**What is enforced:**

- **PRs required.** No direct pushes to `main` from any user, including
  the repo owner (`enforce_admins: true`). The genesis-commit one-time
  exception is historical and does not apply to ongoing work.
- **All required CI checks must pass** before merge — the required-check
  set is listed below.
- **Linear history.** Squash-merge only on `main`. Non-linear merges
  are blocked (`required_linear_history: true`); the repo's UI still
  exposes merge + rebase buttons, but they fail policy on `main`.
- **No force-pushes, no branch deletion.** `main` cannot be rewritten
  or removed.
- **Conversation resolution required.** Unresolved review threads —
  bot or human — block the merge button.

**What is NOT enforced (solo-phase posture):**

- **No required reviewers.** GitHub does not permit a PR author to
  approve their own PR; as a solo project, requiring a review would
  permanently block self-merge. The `## Branch + PR` rules above are
  the operative discipline for the author. Review-required protection
  will be added when external contributors arrive (v1.0+, alongside
  the first crates.io publish).

**Required status checks** — the CI job names that must succeed
before merge, in sync with `.github/workflows/ci.yml`:

- `Format`
- `Clippy`
- `Test (linux-x86)` / `Test (macos-arm)` / `Test (macos-x86)`
- `Coverage`
- `MSRV (1.85)`
- `cargo-deny (advisories + licenses + bans)`
- `cargo doc (warnings as errors)`
- `Non-mirror guard (no workspace / no deny.wrappers / no API shim)`
- `EdgeType enum/label completeness guard`
- `Baseline-required scenario invariant`
- `Feature file count`
- `Synthetic-only fixture gate (every fixture listed in MANIFEST.toml)`
- `Asset-provenance gate (every asset listed in MANIFEST.toml)`
- `Example report is byte-identical to renderer output`
- `Resource-ref lint (real loading constructs)`
- `Headless zero-egress proof (file://, network blocked)`
- `BDD outer loop (cucumber-rs, cargo test --test bdd)`
- `crap4rs scorecard`

Bot review checks (`CodeRabbit`, `Gemini`) are intentionally **not**
required: they rate-limit on rapid-cycle PR waves and would
unnecessarily block otherwise-green merges. Bot findings remain
advisory; PR authors still address them on the visible-disposition
discipline.

When a new mandatory CI job is added or an existing job is renamed,
update both `.github/workflows/ci.yml` and
`.github/branch-protection.json` in the same PR, then re-apply via:

```bash
gh api -X PUT repos/breezy-bays-labs/cute-dbt/branches/main/protection \
  --input .github/branch-protection.json
```

If the gate has a local mirror in `lefthook.yml` (e.g. `feature-count`),
update that file too — silent drift between the CI gate and the local
pre-push mirror was the failure mode that delayed PR 15.

## Architecture discipline

Read [`ARCHITECTURE.md`](ARCHITECTURE.md), [`CLAUDE.md`](CLAUDE.md), and
[`AGENTS.md`](AGENTS.md) before touching code. The hexagonal layering rule
is **strict**:

- **`src/domain/`** must never import outward — no `crate::adapters`, no
  `crate::cli`, no parser libs (sqlparser, askama), no clap. Only `std`
  and `serde` derive. Domain is POD-only.
- **`src/ports/`** is a trait seam, used only where >1 real-or-test impl
  exists. v0.1 introduces one port (the manifest source). The renderer is
  NOT a port — there is one output format in v0.1.
- **`src/adapters/`** depends on `domain` and `ports`. Houses serde
  manifest ingestion, the sqlparser CTE engine, the askama renderer, the
  asset-inlining infra.
- **`src/cli/`** depends on everything below. Houses clap derive, ExitCode
  mapping, and the run-loop composition. Application orchestration lives
  here (single-crate composition choice — see ARCHITECTURE).

The five **conscious design simplifications** (no workspace, no per-crate
versioning, no public-API shim, no AST-purity grep, no JSON envelope ADR)
are documented in `ARCHITECTURE.md` and enforced by the `non-mirror-guard`
CI job. Adding one is a regression, not a "pattern completion."

## Exclusions and tracking-issue rule

Every `#[ignore]`, every `if: false` workflow gate, every `exclude` /
`skip` array entry in config must carry an inline `# tracked: cute-dbt#N
— <reason>` comment OR `# adr: <path>` if permanent. See
`~/.claude/rules/exclusions.md` for the full rule.

## Issue discipline

- Every issue gets exactly one `type:*` label (`type:feature` /
  `type:bug` / `type:task` / etc.) and one `priority:*` label.
- Sub-issues are wired to their epic via GitHub native sub-issues (not
  manual checkboxes).
- Body skeleton: `## Summary` / `## Acceptance Criteria` / `## Context` /
  `## Discovery`.
- Wire `blocked-by` edges at creation time, not later.

## Authoring `.feature` scenarios

- Synthetic data only in example tables (no real data, ever) — see the
  synthetic-only fixtures rule below.
- Scenarios assert **observable behavior** — exit code, file presence,
  DOM structure, network requests — never implementation detail.
- Every scenario invoking the CLI must pass `--baseline-manifest`, except
  scenarios explicitly tagged `@no-baseline-usage-error` (the one
  intentional exception that exercises the usage-error path itself). The
  `baseline-required-grep` CI job enforces this.

## Synthetic-only fixtures

Every committed fixture / `insta` snapshot / `.feature` example **must**
contain only synthetic or public-demo data. No real data from any source,
ever. cute-dbt's privacy property is that when you run it, your manifest
stays on your machine; this public repository must reflect that property
by never including real data of its own. A real-data fixture in this
public repo would contradict the privacy story on day one.

The invariant is **mechanically enforced**, not a checklist line:

- **`tests/fixtures/MANIFEST.toml`** lists every committed fixture with:
  - `path` — the fixture file's repo-relative path
  - `origin` — upstream source name (e.g. `jaffle-shop`) or
    `synthetic-generated`
  - `url` — the upstream URL when the fixture is a public demo, omitted
    when synthetic
  - `sha256` — the SHA-256 of the fixture file
  - `synthetic_only = true` — explicit, per-file affirmation that the
    fixture contains no real data
- A **`cargo test`** parses the manifest, walks `tests/fixtures/`, and
  verifies every file is listed.
- A **CI grep** fails the build on any file under `tests/fixtures/` not
  listed in the manifest.

The same mechanism shape applies to `assets/MANIFEST.toml` (the vendored
frontend bundle's provenance index) — see
[`ARCHITECTURE.md` §5](ARCHITECTURE.md#5-asset-embedding-zero-egress-gate).
Real data in this public repo is a release blocker.

## Release discipline

cute-dbt publishes to crates.io from `v0.1.0+` via `release-plz` + OIDC
trusted publishing. v0.x is unstable per Cargo SemVer convention: every
minor bump (`0.1 → 0.2`) MAY carry breaking changes; v1.0 ships the first
stability commitment. Full policy in
[`AGENTS.md` §Release discipline](AGENTS.md#release-discipline).

Contributor notes:

- Use conventional-commit prefixes (`feat:` / `fix:` / `docs:` / `chore:`
  / `test:` / `refactor:` / `ci:` / `adr:` / `closeout:`) — `release-plz`
  infers version bumps from them. In v0.x, `feat` → patch; `BREAKING
  CHANGE` footer → minor (the Cargo SemVer "0.1 → 0.2 is the breaking
  line" convention). In v1.0+, `feat` → minor; `BREAKING CHANGE` → major.
- Manual `cargo publish` is forbidden — OIDC + `release-plz` is the only
  publish path.
- Tag deletion is forbidden — to "fix" a bad tag, ship the next patch.
- See `CHANGELOG.md` for release history.

## License

By submitting a PR you agree your contributions are licensed under MIT.
