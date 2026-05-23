# Auditability Package

This document is the one-page index for a risk-team reviewer. Every
claim in [`SECURITY.md`](SECURITY.md) corresponds to an artifact below
that you can re-run yourself with the repository checked out. The
intent is plain: read this document end to end and you have walked the
entire proof.

The two zero-egress gates land here. The headless network-block test
is the **primary** artifact — a real Chromium opens the generated
report via a real `file://` URL, every outbound request is observed,
and the assertion is that the list is empty. The structured
resource-ref lint is the **secondary** artifact — it walks the
rendered HTML with an HTML parser and rejects any real loading
construct in milliseconds. Together they pin the runtime property
(zero outbound requests in a real browser) and the structural property
(no loading constructs in the rendered template).

The remaining sections cover supply-chain provenance: the vendored
frontend bundle (`assets/`), the committed test fixtures
(`tests/fixtures/`), the Cargo dependency graph (`deny.toml` +
`Cargo.lock`), and the toolchain pin (`rust-toolchain.toml`).

---

## 1. Headless `file://` zero-egress proof (PRIMARY)

**What it proves.** Open the generated report in a real Chromium with
DNS denied (`--host-resolver-rules=MAP * ~NOTFOUND`) and subscribe to
every `Network.requestWillBeSent` event. The proof: zero external
requests (http / https / ws / wss / ftp) are emitted by the rendered
chrome.

**How to re-run.**

```bash
cargo test --test headless_zero_egress --locked -- --ignored
```

The `-- --ignored` flag is required because the test is `#[ignore]` by
default — the standard cross-platform `cargo nextest run --all-targets`
invocation does not carry a Chrome dependency. On CI this runs in the
[`headless-zero-egress`](.github/workflows/ci.yml) job with Chrome
installed via `browser-actions/setup-chrome`. The job is a required
gate on every PR to `main`.

**Where to look.** [`tests/headless_zero_egress.rs`](tests/headless_zero_egress.rs).
The test asserts the URL begins with `file://` (hard gate — Chromium
treats real `file://` as a stricter null-origin context than
`127.0.0.1` loopback, so a loopback-based proof would be invalid).
It opens the committed
[`examples/jaffle-shop-report.html`](examples/jaffle-shop-report.html),
records every observed external request with its initiator, and
prints them in the failure message if any fire.

**Failure mode.** A non-empty external-request list. The error
message names each captured URL and which DOM element initiated it,
so a CI log is sufficient evidence to act on.

---

## 2. Structured resource-ref lint (SECONDARY)

**What it proves.** The rendered HTML contains no real loading
construct: no `<script src="…">`, no `<link href="…">` (other than
`data:` URIs), no `<img src="…">` (other than `data:` URIs), no
CSS `@import`, no CSS `url(…)` to anything other than `data:`, and
no protocol-relative `//host/…` references.

**How to re-run.**

```bash
cargo test --test resource_ref_lint --locked
```

On CI this runs in the [`resource-ref-lint`](.github/workflows/ci.yml)
job. It is a required gate on every PR to `main`.

**Where to look.** [`tests/resource_ref_lint.rs`](tests/resource_ref_lint.rs).
The test parses the committed example with the
[`tl`](https://crates.io/crates/tl) HTML parser and walks elements
structurally — not via raw `grep http`. Raw `grep` is misleading
because the inlined Mermaid + DataTables + jQuery bundles contain
hundreds of inert URL string literals inside regex constants and
template strings, none of which ever trigger a real HTTP request.

**Failure mode.** A non-empty violation list. The error message names
each construct kind and value (e.g. `<script src>: https://…`), so a
CI log is sufficient evidence to act on.

---

## 3. Vendored frontend asset provenance

**What it proves.** Every file under [`assets/`](assets/) — the
inlined frontend bundle that gets compiled into the binary's
`.rodata` section — is pinned to a known upstream version, with a
recorded SHA-256 and an SPDX license identifier.

**How to re-run.**

```bash
cargo test --test assets_manifest --locked
```

CI additionally enforces the structural invariant in the
[`assets-manifest-gate`](.github/workflows/ci.yml) job — every file
under `assets/` must be listed in
[`assets/MANIFEST.toml`](assets/MANIFEST.toml) and every listed
license must be on the permissive allowlist (MIT / BSD / Apache-2.0 /
ISC / 0BSD).

**Where to look.** [`assets/MANIFEST.toml`](assets/MANIFEST.toml).
Each entry records `name`, `version`, `path`, `source` (upstream URL),
`sha256`, and `license`. To update an asset: re-download from
`source`, replace the bytes, update `version` + `sha256` + `license`,
re-run the headless test in §1 to re-validate zero-egress on the new
bundle.

---

## 4. Synthetic-only fixture provenance

**What it proves.** Every committed test fixture is synthetic or
public-demo data. No real customer rows, no real records of any
kind. This is the hard public-repo invariant — a real-data fixture in
this MIT-licensed public repo is a release blocker.

**How to re-run.**

```bash
cargo test --test fixture_manifest_listed --locked
cargo test --test fixture_parse --locked
```

CI additionally enforces the structural invariant in the
[`fixture-manifest-gate`](.github/workflows/ci.yml) job — every file
under `tests/fixtures/` must be listed in
[`tests/fixtures/MANIFEST.toml`](tests/fixtures/MANIFEST.toml) with
`synthetic_only = true`.

**Where to look.**
[`tests/fixtures/MANIFEST.toml`](tests/fixtures/MANIFEST.toml). Each
entry records `path`, `origin` (`synthetic-generated`,
`tuva-demo`, `jaffle-shop`, …), `source` URL when applicable,
`sha256`, and the affirmative `synthetic_only = true` flag.

---

## 5. Cargo dependency supply chain

**What it proves.** Every Cargo dependency carries a permissive
license (MIT / BSD / Apache-2.0 / ISC / Unicode-3.0 / Zlib), has no
known security advisory, and comes from
`https://github.com/rust-lang/crates.io-index`.

**How to re-run.**

```bash
cargo deny check
```

CI runs this in the [`deny`](.github/workflows/ci.yml) job on every
PR. The action itself is SHA-pinned (a tag-poisoning attack on the
gate action would otherwise silently disable every other supply-chain
check).

**Where to look.** [`deny.toml`](deny.toml). The license allowlist,
the advisory policy, and the source allowlist are all declared
explicitly. New licenses are introduced in the PR that introduces the
new dependency, so the policy moment lands when it matters.

---

## 6. Reproducible build

**What it proves.** Anyone who checks out the repository at a given
commit and runs `cargo build` produces a binary built from the same
dependency graph, with the same compiler and the same components, as
the maintainer.

**How to re-run.**

```bash
cargo build --locked
```

The `--locked` flag asserts that [`Cargo.lock`](Cargo.lock) matches
the current dep graph; CI uses `--locked` on every build/test step.
The Rust toolchain (compiler + components) is pinned in
[`rust-toolchain.toml`](rust-toolchain.toml).

---

## 7. Architecture-level non-mirror guard

**What it proves.** cute-dbt is a single-crate Rust CLI by deliberate
design. Several pieces of common Rust apparatus (multi-crate
workspaces, public-API re-export shims, `bans.deny.wrappers`) exist for
projects whose shape cute-dbt does not have — they would guard no
invariant here, and adding them would be pattern-completion noise
that hides the real architecture. The
[`non-mirror-guard`](.github/workflows/ci.yml) CI job rejects each
specifically.

**How to re-run.** This gate runs structurally on CI. The grep
commands the job runs are inlined in the workflow file (`grep -qE`
checks for `[workspace]` in `Cargo.toml`, for `bans.deny.wrappers` in
`deny.toml`, and for `pub use crate::…::…` in `src/lib.rs`).

**Where to look.** [`ARCHITECTURE.md`](ARCHITECTURE.md) §2
("Conscious design simplifications") records each non-mirror with its
rationale and enforcement layer.

---

## Cross-references

| Topic | Document |
|-------|----------|
| Plain-language privacy statement | [`SECURITY.md`](SECURITY.md) |
| Architecture invariants | [`ARCHITECTURE.md`](ARCHITECTURE.md) |
| Cross-provider agent operating guide | [`AGENTS.md`](AGENTS.md) |
| Public README | [`README.md`](README.md) |
