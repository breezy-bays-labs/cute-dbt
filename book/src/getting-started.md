# Getting started

## Install

> **Not yet on crates.io** — cute-dbt is a v0.x walking-skeleton. The
> first crates.io release will be `v0.1.0`. This page assumes that
> release; if you are reading this before `v0.1.0` lands, build from
> source via `cargo install --git
> https://github.com/breezy-bays-labs/cute-dbt`.

Once `v0.1.0` is published:

```sh
cargo install cute-dbt
```

Verify:

```sh
cute-dbt --help
```

## Generate a report

`cute-dbt report` always reads a **current** `manifest.json`, plus
exactly one **scope source** telling it what changed:

- **Local dev** (this page): a `--baseline-manifest` to diff against.
- **CI / PR review**: `--pr-diff` with the PR's unified diff (`git diff
  --unified=0`) — see the [GitHub Actions PR-review
  recipe](./recipes/github-actions-pr-review.md), the headline team
  path. ([Which should I use?](./how-it-works.md#which-scope-source))

The local flow below uses a baseline manifest.

```sh
cute-dbt report \
    --manifest         target/manifest.json \
    --baseline-manifest path/to/baseline-manifest.json \
    --out              report.html
```

By default the baseline diff detects model **body** changes
(`state:modified.body`). To also catch config-only / relation /
macro-dependency / contract changes, opt into the wider fidelity with
`--modified-selectors` — the tokens match dbt's `state:modified.<sub>`
sub-selector vocabulary and compose alongside the always-on body
checksum (baseline mode only; the flag conflicts with `--pr-diff`):

```sh
cute-dbt report \
    --manifest          target/manifest.json \
    --baseline-manifest path/to/baseline-manifest.json \
    --modified-selectors configs,relation,macros,contract \
    --out               report.html
```

Then open `report.html` directly:

```sh
# macOS
open report.html

# Linux
xdg-open report.html
```

The HTML opens via `file://`. No server. No outbound requests. Your
data does not leave your machine.

## Experimental: explore the full project

> **Experimental** — the `explore` verb may change or be removed in
> any v0.x release. It stays listed and runnable (there is no gate);
> every invocation prints a one-line notice on stderr — stderr only,
> so stdout-consuming scripts are unaffected. `report` is the stable,
> polished surface; treat everything in this section as
> subject to change.

Where `report` is diff-scoped, the **`explore`** verb renders the
**full manifest** — no baseline, no scope source:

```sh
cute-dbt explore \
    --manifest target/manifest.json \
    --out-dir  explore/
```

It writes two self-contained pages under `explore/`: `dag.html` (the
**interactive** model-lineage DAG — pan/zoom/drag, fuzzy search, click
or search-select to highlight a model's full lineage, <kbd>Space</kbd>
to commit focus on the highlighted model; every node carries its
manifest-derived test counts as a `N data-tests · M unit-tests` badge)
and `tests.html` (the per-model unit-test index).
Explore is **fail-open** on uncompiled models — a `dbt parse`-only
model renders as a "not compiled" node instead of aborting the run
(the manifest itself must still be readable and schema v12+). Bare
`cute-dbt` without a verb is a usage error listing both.

### Change context: `explore --pr-diff`

To see *your branch's footprint* on the graph, pass the optional
`--pr-diff` — the same input shape as `report --pr-diff` (a raw
`git diff --unified=0` patch via `@file`):

```sh
git diff --unified=0 origin/main...HEAD > /tmp/branch.diff

cute-dbt explore \
    --manifest target/manifest.json \
    --out-dir  explore/ \
    --pr-diff  @/tmp/branch.diff
```

The models whose files changed in the diff get an **amber "changed"
glow** on `dag.html`, the header counts them ("N changed in this
diff"), and the legend gains a `changed` chip. Change context **never
narrows scope** — the full graph still renders every model; the diff
only highlights what you touched. The treatment composes with the
highlight/dim interaction: a changed node you highlight shows both,
and the detail card + hover tooltip note "changed in this diff".

Git renames are understood (a purely renamed model still marks as
changed at its new path), and if your dbt project lives in a repo
subdirectory, pass `--project-root <dir>` so the diff's repo-relative
paths match the manifest's project-relative ones (the flag requires
`--pr-diff` on this verb).

The explorer takes **no baseline manifest, ever** — the
developer-native diff signal is git, not environment manifests;
baseline comparison stays a `report`-only concern.

## Producing the manifests

cute-dbt expects compiled dbt manifests (i.e. produced by `dbt
compile` or any command that compiles, like `dbt run`/`dbt build`).
A `dbt parse`-only manifest will fail the second-stage compiled-SQL
preflight check.

A typical PR-review setup:

```sh
# Compile the diff
dbt compile

# Snapshot the baseline (target branch)
git fetch origin main
git stash --include-untracked
git checkout origin/main
dbt compile
cp target/manifest.json /tmp/baseline-manifest.json
git checkout -    # back to your branch
git stash pop

# Render the report
cute-dbt report \
    --manifest          target/manifest.json \
    --baseline-manifest /tmp/baseline-manifest.json \
    --out               report.html
```

Wrapping this in a make target / shell function is a common
ergonomic. cute-dbt itself is intentionally unopinionated about how
you produce the two manifests.

## What you'll see

If the diff has at least one in-scope unit test:

- A **diff-scope banner** at the top naming the baseline and the
  in-scope unit-test count.
- One **model card** per in-scope model, with each in-scope test as
  a nested section.
- A **Mermaid CTE DAG** per model with join-colored edges and the
  always-visible legend.

If the diff has zero in-scope models or zero unit tests on the
in-scope models, the report is a valid empty-scope report with a
"0 unit tests in scope" banner. This is **exit-0 by design** — an
empty scope is information, not failure.

## What can fail

The CLI fails closed on a small set of preflight conditions, each
mapped to a non-zero exit code:

| Condition | Exit | What to do |
|---|---|---|
| Manifest file not found / not JSON | non-zero | Check the path; confirm the file is valid JSON |
| Pre-1.8 dbt schema | non-zero | Upgrade dbt (cute-dbt requires `metadata.dbt_schema_version` ≥ 1.8) |
| Baseline path missing or mismatched | non-zero | Verify `--baseline-manifest` resolves |
| In-scope unit test targets a model with `compiled_code: null` | non-zero | Compile fully (`dbt compile`, not `dbt parse`) |
| No scope source — neither `--baseline-manifest` nor `--pr-diff`, or both at once | clap usage error (exit 2) | Pass exactly one scope source |

There is never a partial report. Either you get a complete report or
a non-zero exit explaining what's missing.
