# Selecting & suppressing checks

cute-dbt's coverage checks (see [Checks](./checks/index.md)) are tuned
through the `[checks]` section of the optional `--config <PATH>` TOML
file, plus an inline SQL pragma for model-adjacent acknowledgements.

Everything on this page is **display-layer only**. Disabled and
suppressed checks still evaluate and still participate in supersedes
resolution — disabling or suppressing a more-specific check can never
resurrect the finding it superseded. Selection *removes* findings from
the report; suppression *keeps them and marks them*, carrying your
reason into the report payload. The report itself stays stateless: no
browser-local state, ever — what the team accepted (and why) lives in
the repo, code-reviewed by construction.

## Selection: opt-out and opt-in modes

Two modes, sqlfluff-style. The default is `opt-out`: every registered
check is displayed unless you disable it.

```toml
[checks]
# mode = "opt-out" is the default
disable = ["grain.unique-key-unbacked"]
```

If you would rather start from nothing and pick the checks you want —
without authoring a huge opt-out list — use `opt-in`:

```toml
[checks]
mode = "opt-in"
enable = ["grain.*"]
```

Rules:

- `disable` is only legal in `opt-out` mode; `enable` is only legal —
  and is required — in `opt-in` mode (`enable = []` is a valid,
  explicit "display no checks").
- Entries are **exact check ids** (`grain.unique-key-unbacked`) or
  **group globs** of the form `<group>.*` (`grain.*`). No other
  pattern shapes are supported.
- Resolution is **fail-closed**: an unknown check id, an unknown group,
  or an unsupported pattern is a usage error (exit 2) before any report
  is written, with remediation text naming the registry's known checks
  and groups. The registry ledger is
  [`heuristics/registry.toml`](https://github.com/breezy-bays-labs/cute-dbt/blob/main/heuristics/registry.toml).

Disabling a check that supersedes another does **not** re-enable the
superseded one on constructs the disabled check fired on — resolution
already happened. That is deliberate: the superseding check exists
because it reads those constructs better.

## Targeted suppression: `[[checks.suppress]]`

Suppression is for *"we know and we don't care"* — a real finding the
team has consciously accepted. It is **never** for tool misreads: if a
check is wrong about a construct, that is a detector bug or a missing
supersedes edge — [file an issue](https://github.com/breezy-bays-labs/cute-dbt/issues)
instead of suppressing.

```toml
[[checks.suppress]]
check = "grain.unique-key-unbacked"
model = "fct_encounters_monthly"
reason = "monthly grain duplicates accepted during the 2026 backfill"
```

- `check` takes an **exact** check id — no globs. Suppression is a
  precise statement; to hide a whole group, use `disable`.
- `model` is the bare model name (`fct_encounters_monthly`) or the full
  node id (`model.healthcare_analytics.fct_encounters_monthly`).
- `reason` is **required** and must be non-empty. A config entry lives
  far from the code it silences, so it must carry its own
  justification. The reason rides into the report payload with the
  finding, marked `suppressed` with `source = "config"`.

A suppress entry whose `model` matches nothing in the current scope is
inert (the model may simply not be in this report's scope); an unknown
`check` id is a fail-closed usage error like any other `[checks]`
entry.

## The inline pragma

For an acknowledgement that should live next to the code it concerns,
put a pragma comment in the model's SQL file:

```sql
-- cute-dbt: ignore(grain.unique-key-unbacked, "known dupes during backfill")
select ...
```

Grammar:

```text
-- cute-dbt: ignore(<check-id>)
-- cute-dbt: ignore(<check-id>, "<reason>")
```

- The pragma may appear on **any line** of the model file — a full-line
  comment or trailing after code (the sqlfluff `-- noqa` placement) —
  and applies **model-wide** (file-level granularity). Construct-level
  placement is not supported.
- `<check-id>` is an exact check id; whitespace inside the parentheses
  is tolerated.
- The `reason` is **optional** here (unlike `[[checks.suppress]]`): the
  pragma sits beside the code it silences, so the surrounding source
  and its review history are the justification surface. When present it
  must be double-quoted, and it rides into the payload exactly like a
  config reason (`source = "pragma"`).
- cute-dbt reads the pragma from the manifest's `raw_code` (the
  verbatim authored model file), so it works in both scope modes with
  no `--project-root` required.
- A pragma naming an **unknown check id is not an error** — it warns on
  stderr and has no effect. Source text warns; config fails closed.

## What suppressed findings look like

A suppressed finding keeps its verdict (`covered` / `uncovered` /
`unknown`), its evidence, and its recommendation — it is simply marked:

```json
{
  "check": "grain.unique-key-unbacked",
  "verdict": { "status": "uncovered" },
  "suppressed": {
    "source": "config",
    "reason": "monthly grain duplicates accepted during the 2026 backfill"
  }
}
```

The report's findings surface renders suppressed findings as a
collapsed, acknowledged set rather than active recommendations.
