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
> does not authenticate visitors on the Free or Pro tier (private Pages
> visibility exists only on GitHub Enterprise Cloud). If your dbt
> models, fixtures, or column names are sensitive, **use the artifact
> link** (artifacts are gated behind repo-read auth). The Pages preview is
> only safe for data you would publish openly. The Pages workflow below
> includes a **privacy guard** step ([§ 4a](#4a-the-privacy-guard-step))
> that detects the private-repo → public-Pages mismatch and **fails the
> run before anything is published**.

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
git diff --unified=0 --find-renames \
  "${{ github.event.pull_request.base.sha }}...${{ github.event.pull_request.head.sha }}" \
  > diff.patch

cute-dbt report \
  --manifest dbt_project/target/manifest.json \
  --pr-diff @diff.patch \
  --project-root dbt_project \
  --out _site/report.html
```

`--pr-diff @diff.patch` reads the unified diff (the leading `@` means
"read from this file"). cute-dbt parses the diff's `+++ b/<path>` headers
to pick the in-scope set and its `@@ … @@` hunks to flag which tests the
PR actually edited (block-precise updated detection + the inline YAML diff
drawer key off the hunk spans). It also reads the `rename from`/`rename
to` headers git emits for detected renames, so a renamed model — even a
*pure* rename, which carries no hunks — scopes under its new name.
`--find-renames` just makes git's default rename detection explicit
(config-proof against a runner that sets `diff.renames = false`); a
patch produced without it still works — a rename-detection-off diff
shows the rename as a deleted + an added file, and the added path
scopes the same node. When a hunk changes a model's `.sql`, the
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
      pages: read            # the privacy guard reads the Pages visibility
    steps:
      # --- Privacy guard: refuse to publish a PRIVATE repo's report to
      #     PUBLIC GitHub Pages. Runs FIRST so a refused run spends no
      #     compile minutes and publishes nothing. Fail-closed — see § 4a
      #     for what it checks and the ALLOW_PUBLIC_PAGES opt-in. ---
      - name: Privacy guard — private repo vs public Pages
        env:
          GH_TOKEN: ${{ secrets.GITHUB_TOKEN }}
          REPO: ${{ github.repository }}
          ALLOW_PUBLIC_PAGES: ${{ vars.ALLOW_PUBLIC_PAGES }}
        run: |
          set -euo pipefail
          repo_private=$(gh api "repos/${REPO}" --jq '.private')
          if [ "$repo_private" = "false" ]; then
            echo "Privacy guard: repository is public — publishing to Pages exposes nothing new."
            exit 0
          elif [ "$repo_private" != "true" ]; then
            echo "::error title=Privacy guard::Could not reliably determine repository privacy status (received: '${repo_private}'). Failing closed."
            exit 1
          fi
          # The repo is private (or internal). Publishing is safe only when
          # the Pages site itself is private (Enterprise Cloud). A 404
          # (Pages not enabled yet) or 403 (token lacks pages: read) lands
          # in the fail-closed arm below.
          pages_visibility=$(gh api "repos/${REPO}/pages" --jq '.visibility // "unknown"' 2>/dev/null || echo "unreadable")
          if [ "$pages_visibility" = "private" ]; then
            echo "Privacy guard: private repo with private (Enterprise) Pages — safe to publish."
            exit 0
          fi
          if [ "${ALLOW_PUBLIC_PAGES:-}" = "true" ]; then
            echo "::warning title=Privacy guard overridden::ALLOW_PUBLIC_PAGES=true — publishing a PRIVATE repo's report to a Pages site with visibility '${pages_visibility}'. Anyone with the URL can read it."
            exit 0
          fi
          echo "::error title=Privacy guard::This repository is PRIVATE but its GitHub Pages site is not confirmed private (visibility: '${pages_visibility}'). Publishing would expose the report to anyone with the URL."
          echo "Remediation:"
          echo "  1. Use the artifact-link variant instead (downloads are auth-gated):"
          echo "     https://breezy-bays-labs.github.io/cute-dbt/recipes/github-actions-pr-review.html"
          echo "  2. On GitHub Enterprise Cloud, set the Pages site visibility to 'private'."
          echo "  3. To knowingly publish anyway, set the repository variable ALLOW_PUBLIC_PAGES=true."
          exit 1

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
          git diff --unified=0 --find-renames \
            "${{ github.event.pull_request.base.sha }}...${{ github.event.pull_request.head.sha }}" \
            > diff.patch
          cute-dbt report \
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

## 4a. The privacy guard step

The first step of the Pages workflow refuses to publish a **private**
repository's report to a **public** Pages site. Org/user Pages sites are
publicly URL-reachable even when the repo is private — Pages does not
authenticate visitors unless the site's visibility is `private`, which is
a [GitHub Enterprise Cloud feature](https://docs.github.com/en/enterprise-cloud@latest/pages/getting-started-with-github-pages/changing-the-visibility-of-your-github-pages-site).
A team wiring this recipe into a private dbt repo would otherwise publish
its model names, fixture data, and column names to the open web on the
first PR.

What it checks, via two GitHub API reads (both served by the default
`GITHUB_TOKEN`; the Pages read needs `pages: read` in the job
permissions, set above):

1. `gh api repos/$REPO --jq '.private'` — is the repository private (or
   internal)?
2. `gh api repos/$REPO/pages --jq '.visibility'` — is the Pages site
   itself private?

| Repo | Pages visibility | Outcome |
|---|---|---|
| public (explicit `false`) | any | ✅ pass — publishing exposes nothing new |
| private / internal | `private` (Enterprise) | ✅ pass |
| private / internal | `public` | ❌ **fail before publishing** |
| private / internal | unknown (Pages not enabled yet, or no `pages: read`) | ❌ **fail before publishing** (fail-closed) |
| anomaly (`.private` read returns neither `true` nor `false`) | any | ❌ **fail before publishing** (fail-closed, regardless of `ALLOW_PUBLIC_PAGES`) |
| private / internal | not `private`, `ALLOW_PUBLIC_PAGES=true` | ⚠️ warn + publish (explicit opt-in) |

Both "unknown" rows are deliberate. The guard bypasses only on an
**explicit `false`** from the repository-privacy read — an empty string,
`null`, or any other API anomaly fails closed rather than silently
skipping the guard. And when Pages isn't enabled yet, the first
publish creates the `gh-pages` branch and a later "enable Pages" click
defaults the site to public — so the guard refuses until the safe state
is provable.

If you understand the exposure and want the Pages preview anyway (e.g. a
private playground repo with synthetic data), set the **repository
variable** `ALLOW_PUBLIC_PAGES` to `true` (Settings → Secrets and
variables → Actions → Variables). The guard then downgrades to a warning
annotation instead of failing. Otherwise the remediation is the
[artifact-link variant](#5-artifact-link-private-repos--pages-less) below
— artifact downloads sit behind repo-read auth, so it needs no guard —
or Enterprise private Pages.

The guard never sends your data anywhere — it makes two metadata reads
against your own repo and exits; the report itself still
[makes zero external requests](../zero-egress.md).

## 5. Artifact link (private repos / Pages-less)

**Identical to the Pages preview** except: the job needs no
`contents: write` and no `pages: read`, the privacy-guard step is dropped
(nothing is published — artifact downloads sit behind repo-read auth, so
there is no mismatch to guard), and the
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
  cute-dbt reads identically — see [How it works](../how-it-works.md). To
  compile with **dbt-fusion** instead of dbt-core, see the standalone-binary
  variant in § 11 below (no pip).
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
  contents: read
# Share one group with your preview-publish (and book-deploy, if any)
# workflows so concurrent gh-pages pushes can't race + lose commits.
concurrency:
  group: gh-pages
  cancel-in-progress: false
jobs:
  cleanup:
    runs-on: ubuntu-latest
    # Fork PRs get a read-only token → the push can't succeed; skip cleanly.
    if: github.event.pull_request.head.repo.full_name == github.repository
    permissions:
      contents: write
    steps:
      - uses: actions/checkout@34e114876b0b11c390a56381ad16ebd13914f8d5 # v4.3.1
        with:
          ref: gh-pages
          persist-credentials: true   # this job pushes the cleanup commit
      - env:
          PR_NUMBER: ${{ github.event.pull_request.number }}   # via env, not inline ${{ }}
        run: |
          set -euo pipefail
          # actions/checkout sets no committer identity — set one or commit fails.
          git config user.name  "github-actions[bot]"
          git config user.email "41898282+github-actions[bot]@users.noreply.github.com"
          git rm -r --ignore-unmatch "pr-${PR_NUMBER}"
          # Commit + push only when there's a staged removal. An explicit
          # staged check (not `git commit … || exit 0`, which also masks a
          # genuine commit failure and silently leaves a stale dir).
          if git diff --cached --quiet; then
            echo "No pr-${PR_NUMBER}/ preview to remove."
            exit 0
          fi
          git commit -m "chore: clean up Pages preview for PR #${PR_NUMBER}"
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
- **`Privacy guard: This repository is PRIVATE but its GitHub Pages site
  is not confirmed private`** — working as intended (§ 4a): your repo is
  private and publishing to Pages would expose the report publicly.
  Switch to the artifact-link variant (§ 5), move to Enterprise private
  Pages, or — only if you accept the public exposure — set the repository
  variable `ALLOW_PUBLIC_PAGES=true`. The guard also fires when Pages
  isn't enabled yet or the job lacks `pages: read` (it fails closed when
  the safe state can't be verified — check the `permissions:` block
  matches § 4).
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

A **renamed** model is no longer on this list: cute-dbt reads the
`rename from`/`rename to` headers `git diff` emits (rename detection is
on by default since git 2.9) and maps both paths onto the scope match,
so even a pure rename — which carries no hunks at all — scopes the
model under its new name. The `--find-renames` flag in § 3 makes that
detection explicit, so a runner whose git config sets
`diff.renames = false` produces the same patch shape.

A YAML **config-block** edit no longer mislabels a sibling test as
updated: block precision flags only the tests whose block a diff hunk
actually touched, so a change confined to a `config:` block won't mark the
`unit_tests:` in the same file as updated.

See [Features](../features/index.md) for the full fidelity matrix.

## 11. Variant: compile with dbt-fusion (standalone binary, no pip)

[dbt-fusion](https://docs.getdbt.com/docs/fusion/about-fusion) is a
standalone Rust binary — **no Python, no pip, no virtualenv**. If your
project compiles under fusion, swap the `setup-python` + `pip install` +
`~/.dbt/profiles.yml` steps in § 4 / § 5 for the installer below. Everything
else (the diff + render core in § 3, the sticky comment, the trigger
patterns) is unchanged — fusion and dbt-core both emit manifest schema v12,
which cute-dbt reads identically.

```yaml
      # Install dbt-fusion via the official standalone-binary installer,
      # PINNED to a specific version (never floating `latest`). The
      # installer drops `dbt` into ~/.local/bin.
      - name: Install dbt-fusion
        env:
          FUSION_VERSION: 2.0.0-preview.177   # edit: pin your fusion version
        run: |
          set -euo pipefail
          # `--version VER` pins the install; `--update` only updates an
          # existing install (wrong for a fresh CI runner).
          curl -fsSL https://public.cdn.getdbt.com/fs/install/install.sh \
            | sh -s -- --version "$FUSION_VERSION"
          echo "$HOME/.local/bin" >> "$GITHUB_PATH"
          # Fail loud if the pin didn't land the intended release (PATH update
          # above applies only to later steps, so call dbt by full path here).
          "$HOME/.local/bin/dbt" --version | head -1 | grep -q "$FUSION_VERSION" \
            || { echo "::error::expected dbt-fusion $FUSION_VERSION"; exit 1; }

      # Compile. With an in-project `profiles.yml` (duckdb `:memory:`),
      # compile runs fully offline — no warehouse, no secrets, no network.
      # If your project has no dbt packages there is no `dbt deps` step.
      - name: Compile dbt project (dbt-fusion)
        working-directory: dbt_project   # edit: your project dir
        run: |
          set -euo pipefail
          dbt --version
          dbt compile --profiles-dir .   # profiles.yml committed in-project
```

Notes:

- **Pin the version.** A floating `latest` makes CI non-reproducible and
  can break on a fusion release. Pin a `2.0.0-preview.NNN` tag you have
  tested (the installer takes `--version` via `sh -s -- --version <tag>`,
  no `v` prefix).
- **In-project `profiles.yml` + `--profiles-dir .`** is the offline-friendly
  setup — `compile` only parses and renders SQL, so the duckdb `:memory:`
  target is never materialized and needs no secrets. (Alternatively keep
  the `~/.dbt/profiles.yml` approach from § 4.)
- **Deprecated test-arg format.** Fusion rejects dbt's deprecated
  generic-test argument format that dbt-core only warns about. If
  `dbt compile` errors on that, run the official autofix ephemerally (no
  venv, no pip): `uvx dbt-autofix@latest deprecations --path .`.
- This is exactly the path the cute-dbt repo's own
  [`report-preview.yml`](https://github.com/breezy-bays-labs/cute-dbt/blob/main/.github/workflows/report-preview.yml)
  uses to self-dogfood `--pr-diff` against an embedded fusion example
  project — CI recompiles an **ephemeral** manifest at the PR head and
  renders the PR's own diff into the sticky comment.

## Agent skill

The CI recipe above is for automation. For *interactive* review — "ask
your agent to review this dbt PR" — cute-dbt ships an
[Agent Skill](https://agentskills.io): a small, portable instruction
file that teaches Claude Code, Codex, Cursor, Copilot, and 30+ other
clients to drive [`cute-dbt review`](../one-command-review.md) for you.
The skill is the local-interactive counterpart to this CI recipe.

### Install it

Three channels share **one** file (`skills/dbt-pr-review/SKILL.md`), so
they never disagree:

```sh
# From the cute-dbt binary you already have — zero drift by
# construction, since the skill text ships inside the binary that
# defines the flags. Writes into the current repo.
cute-dbt skill --install                  # .claude/skills/dbt-pr-review/
cute-dbt skill --install --agent codex    # .agents/skills/... (Cursor/Codex/Copilot)

# Or from the skills ecosystem (cross-agent, auto-detects your clients):
npx skills add breezy-bays-labs/cute-dbt --skill dbt-pr-review
gh skill install breezy-bays-labs/cute-dbt dbt-pr-review
```

`cute-dbt skill --install` refuses outside a git repository (the skill
belongs to a repo). `cute-dbt skill --print` writes the skill to stdout
without touching anything — useful to inspect it, or to pipe it
somewhere yourself.

Once installed, the agent runs `cute-dbt review --no-open`, relays
cute-dbt's remediation verbatim on any failure, and re-grounds on
`cute-dbt --version` / `cute-dbt review --help` if a flag looks
unfamiliar — so a stale skill copy self-heals against version drift.

### Fallback: paste into `AGENTS.md` / `CLAUDE.md`

> **The less-capable path.** A skill loads only its name + description
> until invoked, then carries the full workflow; a pasted snippet is
> always-loaded context with no progressive disclosure and no update
> channel. Prefer `cute-dbt skill --install` (or `npx skills add`) above.
> Use this only if you have no skills tooling.

If you cannot use a skill, paste this into your repo's `AGENTS.md` (read
by Codex, Cursor, Copilot, Gemini CLI, …) or `CLAUDE.md` (Claude Code):

```markdown
## Reviewing dbt changes

To produce a PR-review report of this dbt project's unit tests, run
`cute-dbt review --no-open` from the repo root. It auto-detects the base
branch, runs `dbt compile`, diffs the working tree, and writes a
self-contained HTML report to `<project>/target/cute-dbt-report.html`.
On failure, relay cute-dbt's stderr remediation verbatim — do not guess
a fix. If a flag looks unfamiliar, re-ground via `cute-dbt --version`
and `cute-dbt review --help`.
```
