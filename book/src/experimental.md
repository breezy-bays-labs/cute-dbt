# Experimental features

Some cute-dbt surfaces keep shipping on `main` before they are stable
enough for the default experience. They sit behind **one named
switch** with two equivalent opt-in surfaces:

- the `[experimental]` table of the optional `--config <PATH>` TOML
  file, and
- the `CUTE_DBT_EXPERIMENTAL` environment variable.

Opting in on **either** surface enables an experiment — the resolved
set is the **union** of the two (neither surface can disable what the
other enabled). With no opt-in at all, no experimental surface is
enabled.

The switch applies to `cute-dbt report`. The `explore` verb is itself
experimental as a whole and ships **ungated** — it stays runnable with
no opt-in, and `CUTE_DBT_EXPERIMENTAL` has no effect on it.

## The `[experimental]` table

One key: `enable`, a list of **exact experiment ids**.

```toml
[experimental]
enable = ["project-state"]
```

No globs and no `"all"` shorthand here — authored config names
experiments precisely.

## The `CUTE_DBT_EXPERIMENTAL` environment variable

Suited to CI, where exporting a variable on a step is cheaper than
authoring a config file:

```yaml
- name: Render the report
  env:
    CUTE_DBT_EXPERIMENTAL: "1"
  run: cute-dbt report --manifest target/manifest.json ...
```

Accepted values:

- `1` or `all` — enable every registered experiment,
- a comma-separated list of exact experiment ids
  (`project-state`),
- an empty value — enables nothing (a no-op, not an error).

## Fail-closed vocabulary

The experiment vocabulary is **closed and validated up front**: an
unknown id on either surface is a usage error (exit 2) before any
manifest is read, with remediation text naming the registered ids —
the same posture as the `[checks]` section (see
[Selecting & suppressing checks](./check-selection.md)).

## Registered experiments

| id | covers |
|----|--------|
| `project-state` | The project-state surfaces: the "Project definition changed" panel, per-model config attributions, vars/hooks/dispatch change rows, and `dbt_project.yml` config-tree scope widening. |

Experiments graduate by becoming default behavior (the id then
disappears from the vocabulary) — opting in early is how you preview
them and how the feedback loop runs. Because v0.x is explicitly
unstable, ids may appear and graduate between minor versions.
