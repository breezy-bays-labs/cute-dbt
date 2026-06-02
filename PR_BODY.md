# chore: retire vestigial committed dbt manifest + gitignore target/ (seed-gate stays)

## Summary

The committed `dbt-project/target/manifest.json` is **vestigial** — nothing
consumes its bytes. This PR deletes it, gitignores all of `target/`, and
narrows the synthetic-only `fixture-manifest-gate` to the data carriers that
remain committed (the seed CSVs). It also fixes the docs/comments that
asserted the now-false "compile-once-commit / consume directly, never
recompile" claim.

It delivers issue #115's **gate-coverage AC** (the seeds are under the
synthetic-only invariant, mechanically enforced) and **reframes the rest**:
the "consolidate render fixtures onto dbt-project" headline is a **non-goal**
(see below). The single, working close keyword is in the footer.

## The vestigial-manifest finding

A repo-wide `git grep "dbt-project/target"` shows the committed manifest's
only references are in `.github/workflows/report-preview.yml` — and that job
**recompiles an ephemeral manifest before reading it**:

- `report-preview.yml` runs `dbt compile --profiles-dir .` at the PR's HEAD,
  producing a fresh `target/manifest.json`, then feeds *that* just-compiled
  file to `cute-dbt --pr-diff` (`--manifest dbt-project/target/manifest.json`).
  The job even **excludes** `dbt-project/target/` from its own diff
  (`:(exclude)dbt-project/target/`), confirming the committed bytes are not a
  source input.
- The Rust tests read `tests/fixtures/*.json` (never dbt-project's manifest).
- The example-regen path uses those same borrowed fixtures.

So the committed 567 KB manifest is a leftover from the pre-#118 design. Its
`.gitignore` comment ("downstream consumers never recompile") is now **false**,
and — worse — it carries the **root_path leak vector**: dbt writes each node's
`root_path` as the **absolute** build-machine path, and a later
`dbt compile`/`build`/`test` silently re-injects it on every recompile
(gitleaks does not catch it). cute-dbt ignores `root_path` entirely (it drives
off the relative `original_file_path` + the explicit `--project-root`), so
there is **no reason** to commit the manifest and a real privacy reason not to.

## What changed

1. **`git rm dbt-project/target/manifest.json`** — delete the vestigial
   committed manifest.
2. **`dbt-project/.gitignore`** — replace the `!target/manifest.json`
   un-ignore exception with a plain `target/`; rewrite the stale comment
   (build output, recompiled fresh by CI/local, never committed, never
   re-added — leak vector).
3. **`tests/fixtures/MANIFEST.toml`** — drop the `[[project_data]]` entry for
   the manifest (the 3 seed entries stay); flip the header + schema prose to
   "seeds-only committed data carriers; the compiled manifest is gitignored
   build output, not covered."
4. **`.github/workflows/ci.yml` + `lefthook.yml`** — narrow the
   `fixture-manifest-gate` enumeration
   `git ls-files dbt-project/seeds/ dbt-project/target/manifest.json` →
   `git ls-files dbt-project/seeds/` in **both mirrors atomically**
   (lefthook ↔ CI gate-mirror rule), with matching comment updates. Seed
   coverage unchanged; the deleted manifest is no longer expected.
5. **`tests/fixture_manifest_listed.rs`** — remove the `PROJECT_MANIFEST_PATH`
   const + its `git ls-files` arg (would otherwise be unused → `dead_code` →
   clippy `-D warnings` failure); flip all module/fn/section/assert prose to
   seeds-only.
6. **`dbt-project/README.md`** — rewrite the "## The compiled manifest"
   section: the manifest is gitignored build output, recompiled fresh by the
   CI preview job and local dev, and **must never be re-committed** (root_path
   leak vector).
7. **`.github/workflows/report-preview.yml`** — comment-only fix: the
   `target/` exclude comment said "(the manifest cute-dbt consumes)"; it now
   correctly says gitignored build output recompiled fresh below. The
   `dbt compile` step and `--manifest` path are **unchanged** (that job is the
   evidence the manifest is vestigial — and is itself what recompiles).
8. **Bucket-A/B clarification** (MANIFEST.toml header, one paragraph) — why
   synthetic *test fixtures* exist, so it isn't re-litigated: (a)
   fault-injection fixtures encode manifest shapes a healthy `dbt compile`
   structurally cannot emit (kept by design); (b) the historical "no real
   project yet" hand-rolled-manifest workaround is **retired** now that
   `dbt-project/` exists.

## The reframe (consolidation is a non-goal)

#115's original headline — "consolidate the happy-path render/diff fixtures
onto `dbt-project/`, retire the borrowed manifest pairs" — is **dropped on
purpose**. Frozen committed *test* fixtures (`tests/fixtures/*.json`) are
**correct**: they are deterministic, reviewable, and decoupled from any
toolchain. Consolidating render fixtures onto a *committed* dbt-project
manifest would **reintroduce the exact committed-manifest pattern this PR
deletes** — including the root_path leak vector. So the borrowed
`jaffle-shop-*` / `playground-*` pairs stay fully wired and listed; no
`examples/*.html` or snapshot is touched.

## #115 AC status

- **Gate coverage (delivered):** the embedded `dbt-project/` committed data
  carriers (the seed CSVs) are under the synthetic-only invariant,
  mechanically enforced across all three mirrors (cargo test, CI shell,
  lefthook pre-push).
- **Consolidation (reframed → non-goal):** intentionally not done — see above.

## Verification (no `dbt compile` run — that is the whole point)

All gates green locally: `cargo nextest run` (546 passed), `cargo test --test
fixture_manifest_listed` (6/6), `cargo test --test bdd` (74 scenarios / 477
steps / 10 features, incl. the zero-egress headless network-block gate),
`cargo clippy --all-targets --locked -- -D warnings`, `cargo fmt --check`,
`RUSTDOCFLAGS="-D warnings" cargo doc`, `cargo deny check`, and
`lefthook run pre-push` (all 10 hooks). Examples are byte-unchanged.

**Gate has teeth (verified):** temporarily `git rm`-ing a seed makes the
`fixture-manifest-gate` FAIL (the listed-but-not-tracked direction); restored
from git → green.

Closes #115

🤖 Generated with [Claude Code](https://claude.com/claude-code)
