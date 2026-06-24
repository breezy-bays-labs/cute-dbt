// The zone-region overlay — RISK #3 (ts-review-app-shaping.md Council/discovery).
//
// A {% for %} / incremental-guard zone is a CLICK-THROUGH concentric-ring region
// with a per-ring legend pill. React Flow GROUPS do NOT match this contract: a
// group is a parent-CONTAINER node (children become relative to it, it clips +
// captures pointer events, it can't overlay a click-through ring with its own
// legend pinned to its top edge, and nested groups force a parent-child tree the
// zone data does not have).
//
// SPIKE — two candidates evaluated:
//   (a) a behind-nodes custom RF node-type. Rejected: an RF node is itself a
//       hit-target box; making its FILL click-through while a thin border-frame +
//       legend pill stay clickable fights RF's own node hit-testing + z-index
//       (nodes always paint above a "background" node-type, but RF still routes
//       pointer events to the topmost node under the cursor — the ring's fill
//       would swallow background pans). It also can't pin a legend to each ring's
//       own top edge across nested rings without per-node measuring.
//   (b) a viewport-synced SVG overlay. CHOSEN: a single absolutely-positioned SVG
//       layer above the RF pane, transformed by RF's live {x,y,zoom} viewport
//       (useViewport). The ring fill is pointer-events:none (click-through to the
//       nodes + the background pan underneath); only the legend pill + a fat
//       invisible stroke-frame carry pointer-events, so selection works while the
//       region stays click-through. Nested loops render as concentric rings
//       (outer = larger pad), each legend pinned to its OWN top edge — the exact
//       prototype graph.js contract, verbatim.
//
// DECISION: viewport-synced SVG overlay (b). Documented in the PR body.
//
// LAYER: view.
import React from "react";
import { useViewport } from "@xyflow/react";
import { NODE_H, NODE_W, type GraphZone, type PlacedNode } from "../../domain/graph-model";

const SIDE = 16, TOPX = 26, STEP = 24;

/** Two zones share an enclosure if they share any member (nesting detection). */
function shares(p: GraphZone, q: GraphZone): boolean {
  return p.members.some((m) => q.members.includes(m));
}

/** A zone's concentric RING index — its depth relative to the deepest zone it
 *  overlaps. Outer rings (smaller depth) get a larger pad. (graph.js ringOf.) */
export function ringOf(zones: GraphZone[], z: GraphZone): number {
  return Math.max(...zones.filter((w) => shares(w, z)).map((w) => w.depth)) - z.depth;
}

/** The computed geometry of one zone ring (pure → unit-testable). */
export interface ZoneRect { id: string; label: string; on: boolean; rx: number; ry: number; rw: number; rh: number; ring: number; }

/** Compute every visible zone's ring rectangle from the laid-out node positions.
 *  A zone with no laid-out members is dropped (honest — never an empty ring). */
export function zoneRects(
  zones: GraphZone[] | undefined,
  byId: Record<string, PlacedNode>,
  selectedZone: string | null | undefined,
): ZoneRect[] {
  const zlist = (zones ?? []).filter((z) => z.members.some((id) => byId[id]));
  if (!zlist.length) return [];
  // outer rings first (drawn behind), so nested rings paint on top.
  return zlist
    .slice()
    .sort((p, q) => ringOf(zlist, q) - ringOf(zlist, p))
    .map((z) => {
      const ring = ringOf(zlist, z);
      const padX = SIDE + ring * STEP, padT = TOPX + ring * STEP, padB = SIDE + ring * STEP;
      let x0 = Infinity, y0 = Infinity, x1 = -Infinity, y1 = -Infinity;
      for (const id of z.members) {
        const n = byId[id];
        if (!n) continue;
        x0 = Math.min(x0, n.x); y0 = Math.min(y0, n.y);
        x1 = Math.max(x1, n.x + (n.w || NODE_W)); y1 = Math.max(y1, n.y + NODE_H);
      }
      if (!isFinite(x0)) return null;
      return {
        id: z.id, label: z.label, on: selectedZone === z.id, ring,
        rx: x0 - padX, ry: y0 - padT, rw: x1 - x0 + 2 * padX, rh: y1 - y0 + padT + padB,
      } satisfies ZoneRect;
    })
    .filter((r): r is ZoneRect => r != null);
}

export interface ZoneOverlayProps {
  zones?: GraphZone[];
  byId: Record<string, PlacedNode>;
  selectedZone?: string | null;
  onSelectZone?: (id: string) => void;
}

/** The viewport-synced SVG zone overlay. Pans/zooms in lockstep with the RF pane
 *  (useViewport); the ring fill is click-through, the legend pill + stroke-frame
 *  are the click targets. */
export function ZoneOverlay({ zones, byId, selectedZone, onSelectZone }: ZoneOverlayProps): React.ReactElement | null {
  const { x, y, zoom } = useViewport();
  const rects = zoneRects(zones, byId, selectedZone);
  if (!rects.length) return null;
  const C = "var(--mat-incremental, #e69f00)";
  return (
    <svg
      data-testid="zone-overlay"
      style={{ position: "absolute", inset: 0, width: "100%", height: "100%", pointerEvents: "none", zIndex: 5 }}
    >
      <g transform={`translate(${x},${y}) scale(${zoom})`}>
        {rects.map((r) => {
          const txt = `{% ${r.label} %}`;
          const pillW = Math.round(txt.length * 6.05 + 14);
          const pick = (ev: React.MouseEvent): void => { ev.stopPropagation(); onSelectZone?.(r.id); };
          return (
            <g key={`zone-${r.id}`} data-testid="zone-ring" data-zone={r.id} data-selected={r.on ? "true" : "false"}>
              {/* the click-THROUGH fill (pointer-events:none) */}
              <rect x={r.rx} y={r.ry} width={r.rw} height={r.rh} rx={14}
                fill={C} fillOpacity={r.on ? 0.11 : 0.04}
                stroke={C} strokeOpacity={r.on ? 0.95 : 0.4} strokeWidth={r.on ? 1.8 : 1.2}
                strokeDasharray={r.on ? undefined : "5 4"} style={{ pointerEvents: "none" }} />
              {/* the fat invisible stroke-frame click target */}
              <rect x={r.rx} y={r.ry} width={r.rw} height={r.rh} rx={14} fill="none"
                stroke="transparent" strokeWidth={16}
                style={{ pointerEvents: "stroke", cursor: "pointer" }} onMouseDown={(e) => e.stopPropagation()} onClick={pick} />
              {/* the per-ring legend pill, pinned to THIS ring's top edge */}
              <g transform={`translate(${r.rx + 13},${r.ry})`} style={{ cursor: "pointer", pointerEvents: "all" }}
                onMouseDown={(e) => e.stopPropagation()} onClick={pick}>
                <rect x={0} y={-8.5} width={pillW} height={17} rx={5}
                  fill={r.on ? C : "var(--surface, #16161e)"} stroke={C} strokeOpacity={r.on ? 1 : 0.65} strokeWidth={1} />
                <text data-testid="zone-legend" x={pillW / 2} y={3.6} textAnchor="middle"
                  style={{ font: "600 10px ui-monospace, monospace", pointerEvents: "none" }} fill={r.on ? "var(--surface, #16161e)" : C}>
                  {txt}
                </text>
              </g>
            </g>
          );
        })}
      </g>
    </svg>
  );
}
