# CI recipe: sticky PR comment with downloadable report preview

A reusable CI pattern: on every pull request, render the cute-dbt
report, upload it as a workflow artifact, and post a sticky PR comment
with a clickable download link. The comment **updates in place** on
subsequent pushes (single sticky per PR, not one comment per push).

This is what cute-dbt's own CI does for its example reports — see
[`.github/workflows/report-preview.yml`][own-workflow]. The same shape
copies cleanly into a downstream dbt project's CI; the rest of this
page walks through the adapter steps.

[own-workflow]: https://github.com/breezy-bays-labs/cute-dbt/blob/main/.github/workflows/report-preview.yml

## Quick start

Copy [`.github/workflows/examples/cute-dbt-report-preview.yml`][template]
from cute-dbt into your repo's `.github/workflows/` directory, edit the
two `path/to/manifest-*.json` placeholders to match where your CI
produces the manifest pair, push, done.

[template]: https://github.com/breezy-bays-labs/cute-dbt/blob/main/.github/workflows/examples/cute-dbt-report-preview.yml

The template assumes:

- Your CI has produced a **current** dbt manifest (the PR head's
  parsed project) and a **baseline** dbt manifest (the PR base ref's
  parsed project) somewhere on disk by the time the job runs.
- `cargo install --git https://github.com/breezy-bays-labs/cute-dbt --rev <sha>`
  works while cute-dbt is unpublished. Once cute-dbt is available on
  crates.io, prefer `cargo binstall cute4dbt` for a faster install.

If both assumptions hold, the template ships as-is. If you need to
produce the manifest pair in the same workflow, see the next section.

## Producing the manifest pair

cute-dbt diffs a "current" manifest against a "baseline" manifest. In a
PR-review flow, that's typically PR HEAD vs PR base.

`dbt parse` produces a manifest **without a warehouse connection** —
it's a pure schema-validate pass. So you can parse both refs in CI
without secrets:

```yaml
# After actions/checkout@... with fetch-depth: 0

- name: Set up Python + dbt
  uses: actions/setup-python@...   # pin to a v5+ release SHA
  with:
    python-version: '3.11'
- run: pip install dbt-core dbt-duckdb   # or your adapter

# Parse the PR HEAD (already checked out).
- name: Parse manifest at PR HEAD
  working-directory: ./your-dbt-project
  run: |
    dbt parse --project-dir . --profiles-dir .
    cp target/manifest.json /tmp/manifest-current.json

# Switch to the base ref + re-parse.
- name: Parse manifest at PR base
  run: |
    git switch --detach origin/${{ github.base_ref }}
- name: Parse manifest at base
  working-directory: ./your-dbt-project
  run: |
    dbt parse --project-dir . --profiles-dir .
    cp target/manifest.json /tmp/manifest-baseline.json

# Restore the PR head so the rest of the workflow runs against it.
- name: Restore PR head
  run: git switch -
```

Substitute your own dbt invocation if your project uses a different
adapter or layout. The output paths (`/tmp/manifest-current.json`,
`/tmp/manifest-baseline.json`) become the `MANIFEST_CURRENT` /
`MANIFEST_BASELINE` env vars in the recipe's render step.

## Why artifact + link, not inline HTML

GitHub PR comments are rendered as **markdown only** — `<script>`,
`<style>`, inline `data:` images, and most arbitrary HTML/CSS are
stripped or sanitized. Embedding cute-dbt's rendered HTML (~3 MB
single-file, with inlined CSS + JS + Mermaid bundle) in the comment
body isn't structurally available.

The artifact + link pattern works with GitHub's existing UI:

1. Reviewer clicks the link in the sticky comment.
2. GitHub authenticates them (must be a logged-in user with repo read
   access) and downloads the artifact as a zip.
3. The reviewer extracts the HTML and opens it in any browser. The
   report is fully self-contained — zero external resource requests
   (see the [zero-egress property](../zero-egress.md)) — so it works
   offline.

The artifact URL the action emits is
`https://github.com/{owner}/{repo}/actions/runs/{run_id}/artifacts/{artifact_id}`,
a stable per-artifact deep link.

## Fork PRs

GitHub issues a **read-only `GITHUB_TOKEN`** for `pull_request` events
triggered from forks regardless of the `permissions:` block on the
workflow or job. The sticky-comment step silently no-ops (or fails to
post) on fork PR runs. Same-repo branches are unaffected.

Most projects pick "no fork-PR sticky comments" until they have a
concrete need — same-repo coverage is sufficient for internal review.
If you need fork-PR coverage, two well-trodden options:

### Option A — `pull_request_target` for the comment step only

Replace `on: pull_request` with `on: pull_request_target` **only on
the comment-posting step's job** — not on any job that checks out PR
code. `pull_request_target` runs in base-repo context with write
permissions. The well-known footgun is that checking out PR code in a
`pull_request_target` workflow gives that code write access; for a
comment-only job that touches no PR-controlled inputs, the footgun
doesn't apply.

### Option B — Separate `workflow_run`-triggered comment workflow

Split the workflow in two. The first runs on `pull_request` (any
source, untrusted, just regenerates and uploads the artifact). The
second runs on `workflow_run: [Report preview], completed` — this
event always runs in base-repo context with its own writable token.
The comment workflow downloads the upstream artifact metadata via
`actions/github-script` or `gh api`, builds the comment body, posts
the sticky comment.

The canonical pattern. More YAML to maintain, but the right answer
when fork-PR comments are non-negotiable. See
[GitHub's docs on `workflow_run`][gh-workflow-run] for the full
event surface.

[gh-workflow-run]: https://docs.github.com/en/actions/using-workflows/events-that-trigger-workflows#workflow_run

## What's next

The recipe is the minimal v1 surface. Some natural extensions, none
shipping in cute-dbt today:

- **Pages preview per-PR.** Publish the rendered report to a per-PR
  subdirectory under GitHub Pages (e.g. `pr-<n>/`) and link to that
  in the sticky comment — click-to-open in the browser, no zip
  download needed. Cleanup story (deleting stale `pr-<n>/` subdirs
  when PRs close) needs its own thought. Will probably ship as part
  of a future `cute-dbt-action` composite action.
- **Reusable GitHub Action.** A `breezy-bays-labs/cute-dbt-action@v1`
  composite action that wraps install + render + upload + sticky in
  a single `uses:` line, with inputs for the manifest paths,
  artifact name, sticky header. Consumers would adopt it in ~6 lines
  of YAML.
- **dbt-fusion dogfood.** A second CI job in this repo that
  installs dbt-fusion + parses a bundled tiny dbt project +
  renders against the freshly-generated manifests, as a smoke test
  that the consumer flow stays unbroken across dbt engine updates.

Tracked in cute-dbt's open issues as the broader 🟡 CI/CLI
ergonomics checkpoint.
