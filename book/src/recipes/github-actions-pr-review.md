# CI recipe: GitHub Actions PR review (Pages preview + artifact fallback)

Wire cute-dbt into your dbt repo's pull-request workflow so every PR gets
a **diff-scoped** unit-test report — scoped to the models and tests the
PR actually changed, with no baseline-manifest publishing job to
maintain. Reviewers open the report in one or two clicks.

This recipe uses the `--pr-diff` flag (cute-dbt v0.1.x): the workflow runs
`git diff --unified=0 <base>...<head>` and hands the resulting patch to
cute-dbt, which scopes the report to the models/tests those changed paths
touch and uses the diff's hunks to flag the tests the PR actually edited.
It is the CI counterpart to the local [`--baseline-manifest`][diff] flow.

[diff]: ../how-it-works.md

Two delivery options:

- **Pages preview** (public repos): publishes the report to a per-PR
  GitHub Pages subdirectory and posts a clickable link. ≤2-click reviewer
  UX.
- **Artifact link** (private repos / Pages-less): uploads the report as a
  workflow artifact and links to it.

> ⚠️ **Privacy — read this before choosing.** GitHub Pages on a
> **private** repository is **still publicly reachable by URL** — Pages
> does not authenticate visitors on the Free or Pro tier. If your dbt
> models, fixtures, or column names are sensitive, **use the artifact
> link** (artifacts are gated behind repo-read auth). The Pages preview is
> only safe for data you would publish openly. cute-dbt does not detect
> this mismatch for you in v0.1 — the choice is yours to make.

## 1. What this gives you

On each pull request:

1. CI compiles the dbt project at the PR head and renders a cute-dbt
   report scoped to the PR's changed files.
2. The report is published (Pages) or uploaded (artifact).
3. A sticky PR comment links to it — one comment per PR, updated in place
   on each push.

The report is a single self-contained HTML file with
[zero external resource requests](../zero-egress.md), so it opens offline
in any browser.

## 2. Prerequisites

- **A dbt project** in your repo (this recipe assumes it lives in a
  `dbt_project/` subdirectory; adjust paths if yours is at the repo root).
- **`dbt compile` must work in CI.** cute-dbt's fail-closed contract
  rejects an in-scope model whose `compiled_code` is null, so `dbt parse`
  alone is not enough — you need `dbt compile`, which needs a working
  adapter connection. [DuckDB](https://duckdb.org) (`dbt-duckdb`) compiles
  with **no external warehouse and no secrets** and is the easiest CI
  choice; cloud adapters (Snowflake, BigQuery, …) need credentials wired
  as repository secrets and a CI `profiles.yml` target.
- **A dbt profile CI can find.** dbt reads connection config from a
  `profiles.yml` whose top-level key matches `profile:` in your
  `dbt_project.yml`. Setup is adopter-specific: either commit a
  `profiles.yml` and pass `--profiles-dir`, or write a throwaway CI
  profile to `~/.dbt/profiles.yml` (dbt's default location) as the
  workflow below does. The DuckDB `:memory:` profile shown needs no
  secrets. Likewise, install dbt however your project does — `pip`,
  `uv`, or `poetry` all work; the recipe shows `pip`.
- **`fetch-depth: 0`** on `actions/checkout` — the SHA-based diff needs
  both the base and head commits present locally.
- **For the Pages preview only:** GitHub Pages enabled for the repo, publishing
  from the `gh-pages` branch (Settings → Pages → Source: *Deploy from a
  branch* → `gh-pages`). The first run creates the branch; enable Pages
  to point at it.

## 3. The shared cute-dbt invocation

Both options share the same diff + render core. The diff uses the PR
event's **base and head SHAs** (deterministic, fork-safe — it does not
depend on `origin` remote semantics or branch-name resolution), with
`--unified=0` so cute-dbt sees the exact changed hunks:

```bash
mkdir -p _site
git diff --unified=0 \
  "${{ github.event.pull_request.base.sha }}...${{ github.event.pull_request.head.sha }}" \
  > diff.patch

cute-dbt \
  --manifest dbt_project/target/manifest.json \
  --pr-diff @diff.patch \
  --project-root dbt_project \
  --out _site/report.html
```

`--pr-diff @diff.patch` reads the unified diff (the leading `@` means
"read from this file"). cute-dbt parses the diff's `+++ b/<path>` headers
to pick the in-scope set and its `@@ … @@` hunks to flag which tests the
PR actually edited (block-precise updated detection + the inline YAML diff
drawer key off the hunk spans). When a hunk changes a model's `.sql`, the
Model SQL section also shows an inline **SQL diff** of the model's raw
Jinja with a Raw ↔ Diff toggle — same diff engine, keyed off the same
hunks. Both inline diffs ignore whitespace-only edits as standard (a pure
re-indent shows the plain view, not a noisy diff). `--project-root
dbt_project` rewrites the repo-relative diff paths so they match the
manifest's project-relative `original_file_path`s (drop it if your dbt
project is at the repo root).
cute-dbt parses the diff you hand it — it never shells out to `git` or
reads `GITHUB_EVENT_PATH`, so the workflow stays in control of *how* the
diff is produced.

**Same-revision contract.** Take the diff `base...head` and compile the
manifest at `head` (the recipe below does both). Then the diff hunks line
up with the working-tree YAML, which is what makes block precision and the
inline diff trustworthy. If you feed cute-dbt a diff that no longer
matches the compiled head (revision drift), it degrades gracefully — it
keeps the file-granular "updated" mark and drops the inline diff rather
than mislabel a test.

cute-dbt writes only `report.html`; isolating it in a dedicated `_site/`
dir (hence `mkdir -p _site` + `--out _site/report.html`) keeps the Pages
publish in § 4 from pushing your **whole checkout** to the `gh-pages`
branch — `publish_dir` points at `_site/`, not `.`.

## 4. Pages preview (public repos)

Copy this into `.github/workflows/cute-dbt-pr-review.yml`. Edit
`dbt_project`, the dbt adapter, and `CUTE_DBT_REV` (see § 6 install note).

```yaml
name: cute-dbt PR review
on:
  pull_request:
    paths: ['dbt_project/**', '**/*.sql', '**/*.yml']

# Least-privilege default; the job widens what it needs.
permissions:
  contents: read

concurrency:
  group: cute-dbt-pr-review-${{ github.event.pull_request.number }}
  cancel-in-progress: true

jobs:
  review:
    runs-on: ubuntu-latest
    timeout-minutes: 20
    permissions:
      contents: write        # peaceiris/actions-gh-pages pushes to gh-pages
      pull-requests: write    # sticky comment
    steps:
      - uses: actions/checkout@34e114876b0b11c390a56381ad16ebd13914f8d5 # v4.3.1
        with:
          fetch-depth: 0           # base + head SHAs must be present
          persist-credentials: false

      # --- Compile the dbt project at the PR head. `dbt compile` (not
      #     `dbt parse`) so compiled_code is populated. Adapter, toolchain,
      #     and profile are adopter-specific — see § 2. ---
      - uses: actions/setup-python@a26af69be951a213d495a4c3e4e4022e16d87065 # v5.6.0
        with:
          python-version: '3.11'
      - run: pip install dbt-core dbt-duckdb   # edit: your adapter (uv/poetry work too)
      - name: Write a CI dbt profile
        run: |
          mkdir -p ~/.dbt
          cat > ~/.dbt/profiles.yml <<'PROFILE'
          my_dbt_profile:        # edit: must match `profile:` in dbt_project.yml
            target: ci
            outputs:
              ci:
                type: duckdb     # edit: your adapter; cloud adapters read secrets from env
                path: ':memory:'
                threads: 4
          PROFILE
      - run: dbt deps --project-dir dbt_project
      - run: dbt compile --project-dir dbt_project

      # --- Install cute-dbt (see § 6 for the binstall form once published) ---
      - uses: cargo-bins/cargo-binstall@aaa84a43aec4955a42c5ffc65d258961e39f276e # v1.19.1
      - name: Install cute-dbt
        env:
          CUTE_DBT_REV: PUT_A_CUTE_DBT_MAIN_SHA_HERE   # edit: pin a commit
        run: cargo install --locked --git https://github.com/breezy-bays-labs/cute-dbt --rev "$CUTE_DBT_REV"

      # --- Diff (SHA-based: deterministic + fork-safe) + render.
      #     `--unified=0` so cute-dbt sees the exact changed hunks
      #     (block-precise updated-test detection + the inline YAML diff
      #     drawer key off hunk spans, not just changed filenames). The
      #     diff is taken base...head and the manifest was compiled at head
      #     above, so the hunks line up with the working tree
      #     (same-revision contract). ---
      - name: Render diff-scoped report
        run: |
          set -euo pipefail
          mkdir -p _site
          git diff --unified=0 \
            "${{ github.event.pull_request.base.sha }}...${{ github.event.pull_request.head.sha }}" \
            > diff.patch
          cute-dbt \
            --manifest dbt_project/target/manifest.json \
            --pr-diff @diff.patch \
            --project-root dbt_project \
            --out _site/report.html

      # --- Publish ONLY the report dir to a per-PR Pages subdirectory.
      #     publish_dir: ./_site (NOT .) so the whole checkout never lands
      #     on the gh-pages branch. ---
      - uses: peaceiris/actions-gh-pages@84c30a85c19949d7eee79c4ff27748b70285e453 # v4.1.0
        with:
          github_token: ${{ secrets.GITHUB_TOKEN }}
          publish_dir: ./_site
          keep_files: true
          destination_dir: pr-${{ github.event.pull_request.number }}

      # --- Sticky comment with the report link ---
      - uses: marocchino/sticky-pull-request-comment@0ea0beb66eb9baf113663a64ec522f60e49231c0 # v3.0.4
        with:
          header: cute-dbt-pr-review
          message: |
            ## 📊 cute-dbt review ready

            [**Open the diff-scoped report ↗**](https://${{ github.repository_owner }}.github.io/${{ github.event.repository.name }}/pr-${{ github.event.pull_request.number }}/report.html)

            Scoped to the models + unit tests this PR changed.
```

## 5. Artifact link (private repos / Pages-less)

**Identical to the Pages preview** except: the job needs no
`contents: write`, and the
`peaceiris/actions-gh-pages` step is replaced by an artifact upload + a
download link in the comment.

```yaml
    permissions:
      contents: read
      actions: read          # for the artifacts API call below
      pull-requests: write

    # ... same checkout / dbt compile / install / render steps as above ...

      - uses: actions/upload-artifact@ea165f8d65b6e75b540449e92b4886f43607fa02 # v4.6.2
        with:
          name: cute-dbt-report
          path: _site/report.html    # render step writes here (same as above)
          retention-days: 30

      - uses: marocchino/sticky-pull-request-comment@0ea0beb66eb9baf113663a64ec522f60e49231c0 # v3.0.4
        with:
          header: cute-dbt-pr-review
          message: |
            ## 📊 cute-dbt review ready

            [**Download the diff-scoped report ↗**](${{ github.server_url }}/${{ github.repository }}/actions/runs/${{ github.run_id }})

            Download the `cute-dbt-report` artifact, unzip, open `report.html`
            in your browser. Scoped to the models + unit tests this PR changed.
```

Artifact download requires the reviewer to be logged in with repo read
access — which is exactly the auth gate that makes the artifact link the
safe choice for private data.

## 6. Trigger patterns (opt-in vs. always-on)

Teams that don't want cute-dbt on *every* PR can gate the workflow. Each
pattern **replaces only the `on:` block (and adds a job-level `if:` where
noted)** — the job body from § 4 / § 5 is unchanged.

### T1 — Always-on (the default above)

```yaml
on:
  pull_request:
    paths: ['dbt_project/**', '**/*.sql', '**/*.yml']
```

The `paths:` filter already skips PRs with zero dbt changes (cute-dbt
would render a zero-scope report anyway; this just saves CI minutes).

### T2 — Opt-in by label

A reviewer adds a `dbt-review` label to the PR:

```yaml
on:
  pull_request:
    types: [labeled, synchronize]
jobs:
  review:
    if: >
      (github.event.action == 'labeled' && github.event.label.name == 'dbt-review')
      || (github.event.action == 'synchronize' && contains(github.event.pull_request.labels.*.name, 'dbt-review'))
    # ... rest of the job ...
```

`labeled` fires the first run; `synchronize` keeps the report fresh on
later pushes while the label is present.

### T3 — Opt-in by PR comment

Anyone with write access comments `/cute-dbt`:

```yaml
on:
  issue_comment:
    types: [created]
jobs:
  review:
    if: >
      github.event.issue.pull_request
      && contains(github.event.comment.body, '/cute-dbt')
      && github.event.comment.author_association != 'NONE'
    runs-on: ubuntu-latest
    steps:
      # issue_comment runs in BASE-repo context, not on the PR head — you
      # must check out the PR ref explicitly before the dbt/render steps:
      - uses: actions/checkout@34e114876b0b11c390a56381ad16ebd13914f8d5 # v4.3.1
        with:
          fetch-depth: 0
          persist-credentials: false
      - run: gh pr checkout ${{ github.event.issue.number }}
        env:
          GH_TOKEN: ${{ secrets.GITHUB_TOKEN }}
      # ... then the dbt compile / install / render / publish steps ...
```

The `author_association != 'NONE'` check stops drive-by triggers from
non-contributors. Note the SHA-based diff in § 3 reads
`github.event.pull_request.*`, which is absent on `issue_comment` — under
T3, derive the base/head SHAs from `gh pr view ${{ github.event.issue.number }} --json baseRefOid,headRefOid` instead.

### T4 — Manual button

```yaml
on:
  workflow_dispatch:
    inputs:
      pr-number:
        description: PR number to review
        required: true
```

Read `inputs.pr-number`, `gh pr checkout` it, derive its SHAs as in T3.
Best paired with T1/T2 for the always-on path and used for one-off
re-runs.

Patterns compose — T1 + T4 on one workflow (always-on plus a manual
re-run button) is common.

## 7. Customization

- **dbt project at the repo root:** drop `--project-root` and set
  `--manifest target/manifest.json`.
- **A different dbt version / engine:** edit the `pip install` line. Both
  dbt-core 1.8+ and dbt-fusion 2.0-preview emit manifest schema v12, which
  cute-dbt reads identically — see [How it works](../how-it-works.md).
- **Custom comment body:** the `message:` is plain markdown; add a model
  count, a CHANGELOG link, whatever your reviewers want.
- **Branch protection:** make the `review` job a required status check so
  a PR can't merge until the report renders.

## 8. Cleanup on PR close (Pages preview)

The Pages `pr-<N>/` subdirectories accumulate. A small companion workflow
removes each when its PR closes:

```yaml
name: cute-dbt PR review cleanup
on:
  pull_request:
    types: [closed]
permissions:
  contents: write
jobs:
  cleanup:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@34e114876b0b11c390a56381ad16ebd13914f8d5 # v4.3.1
        with:
          ref: gh-pages
          persist-credentials: true
      - run: |
          set -euo pipefail
          git rm -rf "pr-${{ github.event.pull_request.number }}" || exit 0
          git config user.name  github-actions
          git config user.email github-actions@github.com
          git commit -m "cleanup: remove pr-${{ github.event.pull_request.number }} preview" || exit 0
          git push
```

## 9. Troubleshooting

- **`project root does not exist`** — `--project-root` is validated against
  the working directory. Make sure the path matches where `dbt_project/`
  is checked out.
- **An in-scope model fails with `NotCompiled` / "run dbt compile"** — the
  manifest was produced by `dbt parse`, not `dbt compile`. Use `dbt
  compile` (§ 2) so `compiled_code` is populated.
- **`dbt compile` fails: "Could not find profile" / credentials error** —
  the `profiles.yml` top-level key must match `profile:` in your
  `dbt_project.yml`, and cloud adapters need their credential env vars set
  (§ 2). The CI-profile step writes to `~/.dbt/profiles.yml`, dbt's default
  location, so no `--profiles-dir` is needed.
- **Empty report ("0 unit tests in scope")** — the PR changed no files
  that map to a model `.sql` or a unit-test `.yml`. Confirm `--project-root`
  matches your layout (repo-relative diff paths vs. project-relative
  manifest paths) and that `fetch-depth: 0` is set so the diff resolves.
- **Pages link 404s** — Pages isn't enabled, or it's pointed at the wrong
  branch. Enable it on `gh-pages` (§ 2), or switch to the artifact link.
- **Fork PRs:** with `pull_request` (not `pull_request_target`), forks get
  a read-only `GITHUB_TOKEN`, so the Pages-publish and sticky-comment steps
  silently no-op on fork PRs (same-repo branches are unaffected). If you
  need fork coverage, either run *only the comment step* under
  `pull_request_target` (never checking out PR code), or split the workflow
  so a [`workflow_run`](https://docs.github.com/en/actions/using-workflows/events-that-trigger-workflows#workflow_run)-triggered
  job posts the comment in base-repo context.
- **Comment trigger (T3) not firing** — check the `author_association`
  gate and that the comment contains `/cute-dbt` verbatim.

### Install note (§ 6 reference)

While cute-dbt is **unpublished**, install from a pinned commit:
`cargo install --locked --git https://github.com/breezy-bays-labs/cute-dbt --rev <SHA>`
(set `CUTE_DBT_REV` to a `main` SHA you trust — pinning is mandatory per
the [workflow-hardening convention](../release-discipline.md)). Once
cute-dbt is on crates.io, replace those two steps with the faster
`cargo binstall cute-dbt --version 0.1 --no-confirm` (the `0.1` requirement
resolves to the latest compatible `0.1.x` patch).

## 10. v0.1 fidelity limits

PR-diff scoping picks the in-scope set from the diff's **changed file
paths**; scope selection stays path-granular even though the diff's hunks
now drive block-precise updated-test detection. A few cases are out of
scope by design (use `--baseline-manifest` if you need them):

- A `packages.yml` / `dbt deps` change that alters compiled output without
  touching a `.sql`/`.yml` model file — the diff carries no path that maps
  to an affected node.
- A **renamed** model (the diff shows a deleted path + an added path; the
  deleted path maps to no current-manifest node).

A YAML **config-block** edit no longer mislabels a sibling test as
updated: block precision flags only the tests whose block a diff hunk
actually touched, so a change confined to a `config:` block won't mark the
`unit_tests:` in the same file as updated.

See [Features](../features/index.md) for the full fidelity matrix.
