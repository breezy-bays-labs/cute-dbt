# One-command review

`cute-dbt review` is the **first-contact path**: from a checked-out dbt
repo with branch changes, one command produces the PR-review report —
no flags, no flag archaeology.

```sh
cute-dbt review
```

It finds the dbt project, detects the base branch, runs *your* `dbt
compile`, diffs the working tree against the merge-base, renders the
report to `<project>/target/cute-dbt-report.html`, and — when run from an
interactive terminal — opens it in your default browser (`--no-open`
skips this). Everything `report` needs explicitly, `review` figures out
for you.

## Two verbs: porcelain and plumbing

cute-dbt's PR-review surface is split the way git splits `pull` from
`fetch`+`merge` — a convenience verb that composes a precise one.

| Verb | Role | Audience |
|------|------|----------|
| `cute-dbt review` | **Porcelain.** "Review my work." Auto-detects the base, runs your dbt, diffs, renders. Zero required flags. | Humans at a desk; agents via the [skill](./recipes/github-actions-pr-review.md#agent-skill). |
| `cute-dbt report` | **Plumbing.** Explicit inputs (`--manifest` + `--pr-diff`/`--baseline-manifest` → HTML). | CI (exact SHAs, purpose-made checkouts), scripts, power users. |

`review` *composes* `report`'s internals in-process — same render engine,
two entry depths. It is the local twin of the
[CI PR-review recipe](./recipes/github-actions-pr-review.md): `review`
auto-detects what CI already knows (the base SHA, a clean checkout), so
**CI recipes deliberately stay on `report`** — porcelain auto-detection
would be wrong where the SHAs are already exact.

## Scope: what gets reviewed

By default `review` reviews **everything on your branch vs the detected
base** — committed *plus* staged *plus* unstaged edits (the working-tree
endpoint, which is exactly what `dbt compile` just compiled, so the
inline diffs stay sound). Narrow it with one mutually-exclusive selector:

| Flag | Reviews | Diff endpoint |
|------|---------|---------------|
| *(default)* | branch work vs base | merge-base → working tree |
| `--committed-only` | committed changes only — PR-exact parity | merge-base → HEAD |
| `--staged` | only what is staged | HEAD → index (`git diff --cached`) |
| `--unstaged` | only unstaged edits | index → working tree |
| `--pr [<n>]` | the repo's open PR (see [PR anchoring](#pr-anchoring)) | merge-base → working tree |

> **Staged caveat (honest).** The manifest is always compiled from the
> working tree, so if a file you have staged *also* has unstaged edits,
> `--staged` diffs the index while the manifest reflects the working
> tree. cute-dbt detects that drift, warns naming the files, and the
> inline diffs for those files degrade gracefully to the plain view —
> the report is still correct about *which* tests changed.

Other knobs: `--base <ref>` skips base detection; `--out <path>`
relocates the report; `--project-dir <dir>` points at the dbt project
when discovery is ambiguous; `--no-compile` trusts an existing manifest
(dbt is then not needed at all); `--no-open` skips the browser;
`--config <toml>` passes report configuration through; `--force` renders
the zero-scope report even when the diff is empty.

## Base detection

With no `--base`, `review` walks a ladder and announces which rung
answered (on stderr):

1. `--base <ref>` — explicit override (always wins).
2. `git config cute-dbt.base` — a persisted answer you can set once:
   `git config cute-dbt.base origin/main`.
3. The open PR's base branch, via `gh pr view` — only when the GitHub
   CLI `gh` is installed and you are on a branch; **fail-soft**, so a
   missing or unauthenticated `gh` silently falls through.
4. The `origin/HEAD` symref (`origin/main` at clone time).
5. Probing `origin/{main,master,trunk}`, then the local heads.

If none answer, `review` stops and tells you to pass `--base`. A failed
merge-base is diagnosed: a shallow clone gets the `git fetch --unshallow`
hint, genuinely disjoint histories get the `--base` hint.

### PR anchoring

`--pr` runs the review directly off the repo's open pull request — its
base branch becomes the review base:

```sh
cute-dbt review --pr          # the current branch's open PR
cute-dbt review --pr 1234     # PR #1234 specifically
```

`--pr 1234` first checks that your current HEAD *is* PR #1234's head
branch. If it is not, cute-dbt tells you to `gh pr checkout 1234` first
and stops — **it never checks out or mutates your working tree.**

## `--dry-run`: see exactly what it will do

`review` generates the commands it would run rather than hiding them in
a maintained script. `--dry-run` prints every one — the `git diff`
invocation (with its full config-proof flag set), the `dbt compile`
plan, and the equivalent `cute-dbt report` invocation — and executes
nothing:

```sh
cute-dbt review --dry-run
```

This is the transparency affordance: the "script" is generated and
auditable, never maintained prose-in-shell. Every flag the diff carries
is there to neutralize a user git config that would otherwise silently
corrupt the patch (`diff.noprefix`, external diff drivers, textconv,
`diff.relative`, color, …).

## Privacy

`review` itself makes **zero network requests**, and the generated
report makes zero outbound requests when opened (the
[zero-egress property](./zero-egress.md)). The one caveat is the compile
step: it runs *your* dbt, which may phone home on its own — an engine
version check, anonymous usage stats. That egress belongs to dbt, not
cute-dbt. Suppress it with dbt's own switches:

```sh
DBT_DISABLE_VERSION_CHECK=1 DBT_SEND_ANONYMOUS_USAGE_STATS=false cute-dbt review
```

`review` never reads or edits your `profiles.yml`; a connection problem
surfaces dbt's own error verbatim, with an engine-uniform remediation
hint (check `~/.dbt/profiles.yml` / `--profiles-dir` / `DBT_PROFILES_DIR`,
run `dbt debug`).

## Engines

Both dbt-core (1.8+) and the dbt Fusion engine are detected from the
shape of `dbt --version` output and invoked correctly. **The exit code
is the success signal** — Fusion writes `manifest.json` even on a failed
compile, so cute-dbt never trusts mere artifact presence: a non-zero
`dbt compile` relays dbt's stderr verbatim and writes no report.

`--no-compile` skips the compile entirely and trusts the manifest
already in `target/`; a staleness check then warns (never blocks) if any
project source is newer than the manifest. The manifest location honors
`--target-path` / `DBT_TARGET_PATH`, else `<project>/target/manifest.json`.

## Failure handling

`review` fails closed with an actionable remediation on stderr at every
upstream failure — `git`/`dbt` missing, not a git repository, no
detectable base, a shallow clone, an empty diff ("nothing to review",
exit 0, no file written unless `--force`), untracked files (warned with
a `git add -N` hint), a compile failure, or a not-yet-compiled in-scope
model. Exit codes match `report`: `0` success (including the empty-scope
report), `1` a review-stage or report failure, `2` a usage error.

## Output location

The report lands at `<project>/target/cute-dbt-report.html` by default —
inside dbt's conventionally-gitignored `target/`, so the generated
report (whose inlined paths can carry your local directory layout) is
never invited into version control. Override with `--out`.

## Platform support

`review` shells out to `git` and `dbt` via `std::process::Command` — no
shell, so there is no Git Bash / WSL dependency and no PowerShell
redirection-encoding hazard. **Windows support is designed-in;
CI-verified on Linux/macOS, with Windows CI coverage tracked in
[#308](https://github.com/breezy-bays-labs/cute-dbt/issues/308).**

## See also

- [Getting started](./getting-started.md) — the manual `report` flow
  `review` automates ("what review does for you").
- [How it works](./how-it-works.md#which-scope-source) — the scope-source
  model `report` exposes directly.
- [GitHub Actions PR review](./recipes/github-actions-pr-review.md) — the
  CI recipe (on `report`) and the agent skill install one-liners.
