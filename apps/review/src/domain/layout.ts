// First-party LEFT-TO-RIGHT fallback layout for the React Flow DAGs (synchronous;
// used as the initial positioning before the elkjs worker returns a refined
// layered layout). depth = longest path from a root (no incoming edge);
// x = depth*220, y = index-within-depth * 90.
//
// elkjs (the bundled worker, src/worker/elk.worker.ts) provides the production
// layout asynchronously; this pure first-party pass keeps the graph laid out on
// first paint + is the deterministic, unit-testable fallback.

interface MinNode { id: string }
interface MinEdge { source: string; target: string }

export interface Positioned { position: { x: number; y: number } }

export function layoutLR<N extends MinNode, E extends MinEdge>(
  nodes: N[],
  edges: E[],
): (N & Positioned)[] {
  const incoming = new Map<string, string[]>();
  const outgoing = new Map<string, string[]>();
  for (const n of nodes) {
    incoming.set(n.id, []);
    outgoing.set(n.id, []);
  }
  for (const e of edges) {
    if (!incoming.has(e.target) || !outgoing.has(e.source)) continue;
    incoming.get(e.target)!.push(e.source);
    outgoing.get(e.source)!.push(e.target);
  }

  const depthCache = new Map<string, number>();
  const visiting = new Set<string>();
  function depth(id: string): number {
    if (depthCache.has(id)) return depthCache.get(id)!;
    if (visiting.has(id)) return 0; // cycle guard — should not happen on a DAG
    visiting.add(id);
    const parents = incoming.get(id) ?? [];
    const d = parents.length === 0 ? 0 : Math.max(...parents.map((p) => depth(p) + 1));
    visiting.delete(id);
    depthCache.set(id, d);
    return d;
  }

  const byDepth = new Map<number, N[]>();
  for (const n of nodes) {
    const d = depth(n.id);
    const arr = byDepth.get(d) ?? [];
    arr.push(n);
    byDepth.set(d, arr);
  }

  const positioned: (N & Positioned)[] = [];
  for (const [d, arr] of byDepth) {
    arr.forEach((n, i) => {
      positioned.push({ ...n, position: { x: d * 220, y: i * 90 } });
    });
  }
  const order = new Map(nodes.map((n, i) => [n.id, i]));
  positioned.sort((a, b) => order.get(a.id)! - order.get(b.id)!);
  return positioned;
}
