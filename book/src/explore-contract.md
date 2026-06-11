# Explorer external-drive contract

The explorer's `dag.html` is not just a page you click around — it is a
surface other tools can **drive** and **observe**. An IDE extension, a
terminal browser pane, or any embedding host can push focus into the
lineage view and react when *you* deliberately select a model. This
page documents that contract; the in-tree VS Code extension consumes
exactly this surface.

Everything here holds the [zero-egress property](./zero-egress.md):
`postMessage` to an embedding host is in-process message passing, never
a network request, and on plain `file://` the bridge half is completely
inert — zero behavior change.

## Contract version

The current contract version is **`1`**. It is readable two ways:

- **DOM attribute** (no JS execution needed — for attribute-only
  observers): `<body data-cute-dbt-contract="1">` on `dag.html`.
- **JS global**: `window.cuteDbtContract` —

  ```js
  {
    version: "1",
    commitEventType: "cute-dbt/commit",
    attribute: "data-selected-model",
    hooks: ["focusModel", "setView"],
    views: ["lineage", "cte"]
  }
  ```

The attribute is the single source; the global reads it back, so the
two can never disagree. The version covers the four named surfaces
below — the forward hooks, the commit event schema, the DOM attribute,
and the payload-paths shape — and bumps **only** on a breaking change
to any of them. Versioning is folded into the
[release discipline](./release-discipline.md): a contract bump is a
v0.x **minor** (v1.0+ **major**) event, exactly like a CLI-surface
break. There is no separate versioning system.

## Forward hooks (host → page)

### `window.focusModel(id)`

Highlights the model (emphasize its full upstream + downstream lineage,
dim the rest, open the detail card) and centers it. `id` is the full
manifest node id (`model.<package>.<name>`). Returns `true`, or `false`
for an unknown id (a fail-open no-op).

**The no-echo rule:** `focusModel` never writes `data-selected-model`
and never posts a commit event. A host pushing editor-sync focus must
not hear its own push back as a selection — otherwise host and page
chase each other in a feedback loop.

### `window.setView(kind)`

Switches the active view programmatically: `"lineage"` or `"cte"` (the
same vocabulary the on-page toggle uses). The CTE arm requires a
highlighted model (highlight one with `focusModel` first); switching
without one is a fail-open no-op. Returns `true` when the requested
view is active afterwards, `false` otherwise.

Both hooks exist on every rendered page (inert on an empty manifest) —
calling them never throws.

## The commit signal (page → host), dual-bound

A selection becomes a **commit** only on the deliberate
<kbd>Space</kbd> keypress — never on hover, click, search-select, or
`focusModel`. One commit, two bindings:

1. **DOM attribute** (always): `document.body.dataset.selectedModel`
   is set to the committed model's full node id. This is the
   standalone-`file://` binding — a terminal/browser host observes the
   attribute (e.g. via `MutationObserver` or polling).
2. **Host bridge** (when registered): a versioned commit event via
   `postMessage`:

   ```js
   {
     type: "cute-dbt/commit",
     contractVersion: "1",
     modelId: "model.jaffle_shop.customers",
     view: "lineage",            // the active view at commit time
     paths: { /* the committed node's paths block, see below */ }
   }
   ```

### Host-bridge registration

Detection-based, at page boot, presence checks only:

- a VS Code webview exposes `acquireVsCodeApi` — it is called once and
  the returned API's `postMessage` is used;
- any other host may inject `window.cuteDbtHostBridge` (an object with
  a `postMessage` function) **before** the page's scripts run.

If neither exists the bridge stays unregistered and the page behaves
exactly as before — the attribute binding alone.

## Payload file paths

Every node in the `explore-dag-data` lineage payload carries a `paths`
block so hosts can open files. All values are **project-relative**
manifest facts (dbt emits `original_file_path` / `patch_path` relative
by design — never an absolute path):

```json
{
  "sql": "models/marts/core/dim_payers.sql",
  "schema_yaml": "models/marts/core/_core__models.yml",
  "unit_tests": [
    {
      "name": "test_dim_payers_injects_unknown_sentinel",
      "yaml": "models/marts/core/_core__models.yml",
      "fixtures": ["tests/fixtures/payers_given.csv"]
    }
  ]
}
```

- `sql` — the model's source file (`nodes.<id>.original_file_path`).
- `schema_yaml` — the schema-properties YAML that patches the model
  (`nodes.<id>.patch_path`; both engines serialize it as a
  `<package>://path` package URI, which cute-dbt strips on ingestion).
- `unit_tests[].yaml` — the declaring YAML, from the unit-test node's
  own `original_file_path`.
- `unit_tests[].fixtures` — external `fixture:` file references, in
  given order then the expect's, **verbatim** as the manifest emits
  them. dbt-fusion resolves them to project-relative paths
  (`tests/fixtures/<name>.csv`); dbt-core may emit a bare fixture name,
  which hosts resolve via the same `tests/fixtures/<name>.csv`
  convention cute-dbt's own renderer applies.

Absence is explicit: `null` for a missing path, `[]` for no unit tests
— keys are never omitted. The same paths render read-only in the
model-detail card's **files** section, so the contract surface is
human-visible too.

## Change context (`--pr-diff`): the `changed` node key

When the page was rendered with `explore --pr-diff` (PR-diff change
context), each lineage-payload node whose source file the diff touched
carries an additional key:

```json
{ "id": "model.jaffle_shop.customers", "changed": true, "...": "..." }
```

This key is the **one deliberate exception** to the explicit-absence
posture: it is serialized **only when `true`**. An unchanged node — and
*every* node on a page rendered without `--pr-diff` — carries no
`changed` key at all, so a no-context payload is byte-identical to the
pre-context shape. Hosts should treat a missing key as "not changed /
no context". The key is additive and informational; it is not one of
the four versioned contract surfaces, and shipping it did not bump the
contract version.

Change context never narrows scope: the payload's `nodes` array spans
the full manifest with or without a diff.

## What is deliberately *not* in the contract

- Hover, click, and search are in-page exploration; they never fire
  the external signal in either binding.
- `tests.html` is not a drivable surface (no hooks, no attribute).
- The explorer takes no baseline manifest, ever — change context
  arrives via `--pr-diff` (the git diff signal) and only decorates;
  the commit event schema is unchanged by it.
