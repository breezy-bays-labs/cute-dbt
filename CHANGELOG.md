# Changelog

All notable changes to this project will be documented in this file. The
format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/).

cute-dbt follows a deliberate **no-public-release** policy through v0.x —
the tool runs locally during development and PR review; tags exist for git
pinning only. v1.0 ships when the CLI surface, the askama template
contract, and the auditability package have stabilized.

## [Unreleased]

### Added
- Genesis bootstrap commit: single-crate Cargo workspace skeleton, full CI
  battery (fmt / clippy pedantic / nextest / llvm-cov / MSRV / cargo-deny /
  docs / crap4rs / non-mirror-guard / baseline-required-grep /
  feature-count), repo chrome (README, AGENTS, CLAUDE, CONTRIBUTING,
  SECURITY, ARCHITECTURE skeletons), src/{domain,ports,adapters,cli}/mod.rs
  stubs, `tests/binary_smoke.rs`, 5 `.feature` ATDD specs (corrected per
  the locked baseline-required policy), `assets/MANIFEST.toml` +
  `tests/fixtures/MANIFEST.toml` skeletons.
