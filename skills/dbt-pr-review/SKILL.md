---
name: dbt-pr-review
description: Generate a self-contained HTML PR-review report for a dbt project's unit tests. Use when the user asks to review dbt changes, see what a dbt PR changed, check unit-test coverage of changed models, or produce a diff-scoped dbt report. Drives the user's own dbt + git locally; the report makes zero network requests when opened.
compatibility: Requires cute-dbt on PATH, dbt (dbt-core 1.8+ or the dbt Fusion engine), and git. Runs locally; no network access needed beyond whatever the user's own dbt does.
metadata:
  version: 0.1.0
---

# dbt PR review with cute-dbt

Produce a diff-scoped, self-contained HTML report of a dbt project's
unit tests for the change under review. `cute-dbt review` does the whole
job in one command: it finds the dbt project, detects the base branch,
runs the user's own `dbt compile`, diffs the working tree against the
merge-base, and renders the report.

## Before you start: re-ground on the installed binary

Flags and output can change between cute-dbt versions. **Always** prefer
what the installed binary reports over anything in this file:

```sh
cute-dbt --version
cute-dbt review --help
```

If a flag named below is missing or different, trust `--help`.

## Run it

From inside the user's checked-out dbt repository, on the branch with the
changes:

```sh
cute-dbt review --no-open
```

- `--no-open` is for agents and scripts — it skips the browser auto-open
  (which only happens on an interactive terminal anyway). Use it always.
- Zero required flags: the base branch is auto-detected and the dbt
  project is discovered from the working directory.
- On success, cute-dbt prints `report written to <path>` on stdout.
  Relay that path to the user; the report is a single self-contained
  HTML file they can open in any browser.

### Common variations (confirm against `--help` first)

- `--base <ref>` — diff against a specific base instead of auto-detecting.
- `--pr` / `--pr <n>` — anchor the review to the repo's open pull request
  (needs the GitHub CLI `gh`). `--pr <n>` reviews PR #n; if the current
  checkout is not that PR's head branch, cute-dbt tells the user to
  `gh pr checkout <n>` first and stops — it never checks out for them.
- `--staged` / `--unstaged` — review only staged or only unstaged edits.
- `--committed-only` — exactly what the PR would show (committed changes
  only).
- `--no-compile` — trust an already-compiled manifest instead of running
  `dbt compile` (dbt is then not needed at all).
- `--out <path>` — write the report somewhere other than
  `<project>/target/cute-dbt-report.html`.
- `--dry-run` — print every command a real run would execute, run
  nothing. Useful for explaining what review will do.

## On failure: relay the remediation verbatim

cute-dbt fails closed with an actionable remediation on stderr — for
example: `git` or `dbt` not found, no base branch detectable, a shallow
clone, a `dbt compile` error (its output is the user's own dbt, relayed
verbatim), or a not-yet-compiled manifest. **Do not paraphrase or guess
a fix.** Show the user cute-dbt's stderr exactly as printed and let them
act on it. The exit code is the signal: `0` success, `1` a review-stage
or report failure, `2` a usage error.

## What this is (and is not)

- It is local and private: cute-dbt itself makes no network requests, and
  the generated report makes zero outbound requests when opened offline.
  The compile step runs the user's own dbt, which may phone home on its
  own — suppress that with dbt's switches if needed:
  `DBT_DISABLE_VERSION_CHECK=1 DBT_SEND_ANONYMOUS_USAGE_STATS=false`.
- It does not run dbt tests, manage profiles, or install dbt. It
  visualizes what the unit tests look like for the changed models.
