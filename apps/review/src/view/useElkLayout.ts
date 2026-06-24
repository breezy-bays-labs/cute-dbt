// useElkLayout — lays out React Flow nodes with elkjs running in a BUNDLED
// worker. First paint uses the synchronous first-party LR layout
// (domain/layout); when elk's layered result returns, positions swap in.
//
// elkjs is driven via its `ELK` api class (elk-api.js — pure class, no worker
// code) with a `workerFactory` that constructs OUR bundled worker
// (src/worker/elk.worker.ts → elkjs's elk-worker.js). The worker is built with
// `new Worker(new URL(...), { type: "module" })` so Vite emits it as a
// SAME-ORIGIN asset — never a CDN worker URL. Zero network egress.
import { useEffect, useRef, useState } from "react";
import ELKConstructor from "elkjs/lib/elk-api.js";
import type { ELK, ElkNode } from "elkjs/lib/elk-api.js";
import type { FlowNode, FlowEdge } from "../domain/reshape";
import { layoutLR } from "../domain/layout";

const NODE_W = 150;
const NODE_H = 40;

function makeElk(): ELK {
  return new ELKConstructor({
    workerFactory: () =>
      new Worker(new URL("../worker/elk.worker.ts", import.meta.url), { type: "module" }),
  });
}

export function useElkLayout(nodes: FlowNode[], edges: FlowEdge[]): FlowNode[] {
  const [positioned, setPositioned] = useState<FlowNode[]>(() =>
    layoutLR(nodes, edges.map((e) => ({ ...e }))),
  );
  const elkRef = useRef<ELK | null>(null);

  const nodeKey = JSON.stringify(nodes.map((n) => n.id));
  const edgeKey = JSON.stringify(edges.map((e) => e.id));

  useEffect(() => {
    // Re-seed deterministic fallback positions whenever the graph changes.
    setPositioned(layoutLR(nodes, edges.map((e) => ({ ...e }))));
    if (nodes.length === 0) return;

    let cancelled = false;
    if (!elkRef.current) elkRef.current = makeElk();
    const elk = elkRef.current;

    const graph: ElkNode = {
      id: "root",
      layoutOptions: {
        "elk.algorithm": "layered",
        "elk.direction": "RIGHT",
        "elk.spacing.nodeNode": "40",
        "elk.layered.spacing.nodeNodeBetweenLayers": "80",
      },
      children: nodes.map((n) => ({ id: n.id, width: NODE_W, height: NODE_H })),
      edges: edges.map((e) => ({ id: e.id, sources: [e.source], targets: [e.target] })),
    };
    elk
      .layout(graph)
      .then((laid) => {
        if (cancelled) return;
        const pos = new Map(
          (laid.children ?? []).map((c) => [c.id, { x: c.x ?? 0, y: c.y ?? 0 }]),
        );
        setPositioned((prev) =>
          prev.map((n) => {
            const p = pos.get(n.id);
            return p ? { ...n, position: { x: p.x, y: p.y } } : n;
          }),
        );
      })
      .catch(() => {
        // Loud-but-graceful: keep the first-party fallback positions.
      });

    return () => {
      cancelled = true;
    };
  }, [nodeKey, edgeKey]);

  return positioned;
}
