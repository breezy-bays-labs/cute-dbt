# Explore examples

The `explore` verb renders the full manifest into a **two-page** explorer —
no baseline, no scope source, the whole project at once. Both pages are
committed goldens (byte-identity gated like the reports), rendered from the
synthetic `playground-current.json` fixture, and each holds the zero-egress
property independently. See [the zero-egress page](../zero-egress.md).

> **Experimental.** The `explore` verb (and its page output) may change or
> be removed in any v0.x release while it iterates toward the design
> overhaul. See [Experimental features](../experimental.md) and the
> [Explorer external-drive contract](../explore-contract.md).

## Lineage DAG (`dag.html`)

👉 **[Open the rendered explore lineage DAG](../examples/explore/dag.html)**
([source](https://github.com/breezy-bays-labs/cute-dbt/blob/main/examples/explore/dag.html))

The full-manifest, **interactive** model-lineage DAG — every model plus
typed snapshot / seed / source / exposure nodes, edges from
`depends_on.nodes`, laid out left-to-right by the vendored Cytoscape core +
the `cytoscape-dagre` layout extension. Pan / zoom / drag, fuzzy search,
and click-to-highlight a node with its full transitive lineage (the
complement dims). Fail-open: an uncompiled model renders as a "not
compiled" node rather than raising an error.

## Unit-test index (`tests.html`)

👉 **[Open the rendered explore unit-test index](../examples/explore/tests.html)**
([source](https://github.com/breezy-bays-labs/cute-dbt/blob/main/examples/explore/tests.html))

One section per model with its unit tests, plus the full engine-agnostic
payload embedded as a JSON carrier — the same `build_payload` output the
report renders. A server-rendered static page (no Mermaid, no DataTables);
the two explore pages link to each other (`dag.html` ⇄ `tests.html`) with
same-directory anchors that load nothing until clicked.
