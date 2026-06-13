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
| `macro-lens` | The "Macro changed" section: a changed `macros/*.sql` macro's body diff, the count + collapsible directory tree of the root-project models it reaches, a per-model SQL selector with first-order call sites, and a per-arm fidelity chip. A macro edit is otherwise invisible to model/unit-test scope selection — this surfaces it. |

Experiments graduate by becoming default behavior (the id then
disappears from the vocabulary) — opting in early is how you preview
them and how the feedback loop runs. Because v0.x is explicitly
unstable, ids may appear and graduate between minor versions.

### What `project-state` gates

With the switch **off** (the default), the report behaves as if
`dbt_project.yml` did not exist — the file is not read at all and
contributes **zero bytes** to the output:

- no "Project definition changed" panel (neither the categorized nor
  the raw-diff fallback variant),
- no per-model provenance chips, vars tiers, hooks rows, or dispatch
  banner,
- **no config-tree scope widening** — models under an edited
  `models:` subtree do not join the report scope (widening is scope
  *behavior*, not presentation: widened models render full cards and
  participate in the compiled-SQL pre-flight, which is why this is a
  generation-time gate and not a display toggle),
- no standing `project_definition` metadata in the report payload —
  the *standing metadata is gated with the rest of the family*, so a
  default report is byte-identical whether or not the project root
  carries a `dbt_project.yml`.

With the switch **on**, all of the above render exactly as before the
switch existed — opting in previews precisely what graduation would
make default.

### Tuning the macro-lens inline-body cap

When `macro-lens` is on, the "Macro changed" section can server-render
each impacted model's SQL inline so a reviewer reads the call sites
without leaving the report. A *widely-used* macro can reach dozens of
models, though, and the report is a single self-contained file frozen at
generation time — inlining every body would bloat it. So the inlined
bodies are **capped**.

The model-selector always lists **every** impacted model (that list is
cheap); only the first **N** (in id order) carry a server-rendered inline
SQL panel. Past the cap, a model shows a compact "body not inlined"
affordance and the section states "showing N of M model bodies". The
default cap is **10**.

The cap is a **generation-time knob**, not a post-render toggle (the
report is static once written). Set it on either surface:

```toml
[experimental]
enable = ["macro-lens"]
# Inline up to 25 impacted-model bodies (default: 10).
macro_body_cap = 25
```

or with the CLI flag, which takes precedence over the config key:

```bash
cute-dbt report --manifest target/manifest.json \
  --pr-diff @pr.diff --macro-body-cap 25 ...
```

A cap of `0` inlines nothing (the directory tree + selector only) — the
maximally-bounded report. The cap is inert when `macro-lens` is off.
