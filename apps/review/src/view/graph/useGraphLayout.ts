// useGraphLayout — lays out a shared GraphData with elkjs `layered` in the
// BUNDLED worker (the same elk-api ELK class + workerFactory pattern as
// useElkLayout, NOT a direct elk.bundled import into the worker, NOT a CDN worker
// URL — local-first by construction). Feeds elk positions + the grow-to-fit
// per-node width into PlacedNode[].
//
// First paint uses the synchronous first-party LR fallback (domain/layout) so the
// graph is laid out before the worker returns; the layered result swaps in when
// it lands. Zones widen the elk spacing (rings + legends need breathing room).
//
// LAYER: view.
import { useEffect, useRef, useState } from "react";
import ELKConstructor from "elkjs/lib/elk-api.js";
import type { ELK, ElkNode } from "elkjs/lib/elk-api.js";
import { NODE_H, nodeWidth, type GraphData, type GraphEdge, type PlacedNode } from "../../domain/graph-model";
import { layoutLR } from "../../domain/layout";

function makeElk(): ELK {
  return new ELKConstructor({
    workerFactory: () =>
      new Worker(new URL("../../worker/elk.worker.ts", import.meta.url), { type: "module" }),
  });
}

/** The first-party fallback placement (deterministic; pre-elk first paint). */
export function fallbackPlace(data: GraphData): PlacedNode[] {
  const laid = layoutLR(
    data.nodes.map((n) => ({ id: n.id })),
    data.edges.map(([source, target]) => ({ source, target })),
  );
  const posById = new Map(laid.map((l) => [l.id, l.position]));
  return data.nodes.map((n) => {
    const p = posById.get(n.id) ?? { x: 0, y: 0 };
    return { ...n, x: p.x, y: p.y, w: nodeWidth(n) };
  });
}

const RIGHT: "RIGHT" | "LEFT" = "RIGHT";

export interface UseGraphLayoutOpts {
  /** layout direction (RIGHT default; LEFT for mirrored lineage). */
  direction?: "RIGHT" | "LEFT";
}

export function useGraphLayout(data: GraphData, opts: UseGraphLayoutOpts = {}): PlacedNode[] {
  const [placed, setPlaced] = useState<PlacedNode[]>(() => fallbackPlace(data));
  const elkRef = useRef<ELK | null>(null);
  const direction = opts.direction ?? RIGHT;

  // nodeKey hashes the id PLUS every width-affecting fact (label/sub/mat/
  // incrementalOnly/templated — the exact inputs `nodeWidth` reads). The effect
  // reads `data` directly (not in its deps), so a node whose width-affecting facts
  // change IN PLACE while its id stays the same must still re-key here, else the
  // layout keeps a stale `nodeWidth` and elk never re-runs. (Id alone would miss
  // an in-place fact change.)
  const nodeKey = JSON.stringify(
    data.nodes.map((n) => [n.id, n.label, n.sub, n.mat, n.incrementalOnly, n.templated]),
  );
  const edgeKey = JSON.stringify(data.edges.map((e) => [e[0], e[1]]));
  const hasZones = !!(data.zones && data.zones.length);

  useEffect(() => {
    setPlaced(fallbackPlace(data));
    if (data.nodes.length === 0) return;

    let cancelled = false;
    if (!elkRef.current) elkRef.current = makeElk();
    const elk = elkRef.current;

    const widthById = new Map(data.nodes.map((n) => [n.id, nodeWidth(n)]));
    const graph: ElkNode = {
      id: "root",
      layoutOptions: {
        "elk.algorithm": "layered",
        "elk.direction": direction,
        // zones need horizontal/vertical breathing room for rings + legends.
        "elk.layered.spacing.nodeNodeBetweenLayers": hasZones ? "150" : "82",
        "elk.spacing.nodeNode": hasZones ? "68" : "34",
      },
      children: data.nodes.map((n) => ({ id: n.id, width: widthById.get(n.id), height: NODE_H })),
      edges: data.edges.map((e: GraphEdge, i) => ({ id: "e" + i, sources: [e[0]], targets: [e[1]] })),
    };
    elk
      .layout(graph)
      .then((laid) => {
        if (cancelled) return;
        const pos = new Map((laid.children ?? []).map((c) => [c.id, { x: c.x ?? 0, y: c.y ?? 0, w: c.width }]));
        setPlaced((prev) =>
          prev.map((n) => {
            const p = pos.get(n.id);
            return p ? { ...n, x: p.x, y: p.y, w: p.w ?? n.w } : n;
          }),
        );
      })
      .catch(() => {
        // loud-but-graceful: keep the first-party fallback positions.
      });

    // Re-layout (axis toggle, subgraph switch, direction/zone change) only
    // cancels the in-flight result — it NEVER terminates the worker. Spawning a
    // Worker is expensive; tearing it down + respawning on every dependency
    // change caused UI lag. The ONE elkRef worker stays alive and services the
    // next elk.layout() call; only the unmount effect below disposes it.
    return () => {
      cancelled = true;
    };
  }, [nodeKey, edgeKey, direction, hasZones]);

  // Dedicated unmount-only disposal (empty deps): elkjs never disposes its
  // bundled worker itself, so without this the live Worker leaks when the
  // component unmounts. Guarded — only call when the instance + method exist
  // (elkjs 0.11.1 exposes ELK#terminateWorker). Clear the ref so a remount
  // constructs a fresh ELK + worker.
  useEffect(() => {
    return () => {
      const elkToDispose = elkRef.current;
      if (elkToDispose && typeof elkToDispose.terminateWorker === "function") {
        elkToDispose.terminateWorker();
        elkRef.current = null;
      }
    };
  }, []);

  return placed;
}
