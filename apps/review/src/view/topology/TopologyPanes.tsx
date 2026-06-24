// TopologyPanes — the S6b container that wires the CTE⇄code panes to the PURE S6a
// cursor-sync machine (domain/cursor-sync). It COMPOSES three merged surfaces and
// owns NO sync semantics of its own:
//   • the shared DAG engine (view/graph/LineageGraph) for the compiled OR raw CTE
//     DAG (the toggle FOLLOWS the shelf mode),
//   • the CompiledView pane (this slice) reflecting the cursor/span/scroll,
//   • the S6a reducers (selectNode/selectZone/syncForward/syncFromCursor) driven
//     over the SyncMaps that buildSyncMaps (this slice) lifts from code_map.
//
// THE BIDIRECTIONAL SYNC (consumed, never modified):
//   FORWARD  — a DAG node click → selectNode → syncForward bumps scrollNonce →
//              CompiledView DIRECT-scrolls + ring-flashes the node's span.
//   REVERSE  — a cursor move in the pane → syncFromCursor resolves the innermost
//              node/zone and highlights it in the DAG.
// The machine's `===`-identity anti-loop bail means a forward→reverse round-trip
// re-renders nothing extra; this container just applies whatever state it returns.
//
// HONEST-EMPTY: a model with NO code_map (buildSyncMaps === null) renders the
// CompiledView empty state + an honest DAG — never a fabricated span or sync.
//
// LAYER: view (composes graph + domain; never chrome).
import React, { useCallback, useEffect, useMemo, useReducer, useRef, useState } from "react";
import { LineageGraph } from "../graph/LineageGraph";
import { CompiledView } from "./CompiledView";
import {
  initialSyncState, selectNode, selectZone, syncForward, syncFromCursor, spanForNode,
  type SyncMaps, type SyncState,
} from "../../domain/cursor-sync";
import { buildSyncMaps } from "../../domain/sync-maps";
import { cteDagToGraph, rawGraphToGraphData } from "../../domain/topology-graphs";
import { rawDagToGraph, ensureMainNode } from "../../domain/data/raw-spans";
import type { GraphData } from "../../domain/graph-model";
import type { ModelPayload } from "../../domain/context-data";

type Shelf = "compiled" | "raw";

/** The sync-machine actions this container dispatches (each delegates to a pure
 *  S6a reducer — no extra logic lives in the reducer). */
type SyncAction =
  | { type: "selectNode"; id: string | null; side: "compiled" | "raw"; maps: SyncMaps }
  | { type: "selectZone"; id: string | null }
  | { type: "cursor"; line: number | null; side: "compiled" | "raw"; maps: SyncMaps };

function syncReducer(state: SyncState, action: SyncAction): SyncState {
  switch (action.type) {
    case "selectNode":
      // forward: select then run the SIDE-aware forward sync (cursor snap + scroll
      // nonce). The side resolves the span against the active shelf's table, so a
      // raw-only `zone:N`/`(final select)` node scrolls + flashes the raw pane
      // instead of silently no-op'ing (cute-dbt#497 finding 1).
      return syncForward(selectNode(state, action.id), action.maps, action.side);
    case "selectZone":
      return selectZone(state, action.id);
    case "cursor":
      // reverse: move the cursor → resolve the innermost node/zone.
      return syncFromCursor(state, action.maps, action.line, action.side);
  }
}

/** The span (in the active pane's coords) for the current selection. Compiled:
 *  the node's compiled span. Raw: the node's raw span OR the selected zone's
 *  region — both already in `maps`, so no recompute, no fabrication. */
function activeSpan(maps: SyncMaps | null, state: SyncState, side: "compiled" | "raw") {
  if (!maps) return null;
  if (side === "compiled") return spanForNode(maps, state.node, "compiled");
  // raw side
  if (state.zone) {
    const z = (maps.zones ?? []).find((zz) => zz.id === state.zone);
    return z ? { start: { line: z.startLine }, end: { line: z.endLine } } : null;
  }
  // a node on the raw shelf → its RAW span (the same table the forward sync now
  // resolves over), so a raw-only `zone:N`/`(final select)` node tints too.
  return spanForNode(maps, state.node, "raw");
}

export interface TopologyPanesProps {
  model: ModelPayload;
  shiki: string;
}

export function TopologyPanes({ model, shiki }: TopologyPanesProps): React.ReactElement {
  const [shelf, setShelf] = useState<Shelf>("compiled");
  const [state, dispatch] = useReducer(syncReducer, undefined, initialSyncState);
  // scrollKey mirrors the machine's scrollNonce into a prop the CompiledView
  // effect keys on (a node pick → one DIRECT-scroll + ring-flash).
  const scrollKey = state.scrollNonce;

  // ── the SyncMaps the machine resolves over (null ⇒ honest-empty) ────────────
  const maps = useMemo(() => buildSyncMaps(model), [model]);

  // ── the compiled + raw graphs (built once per model) ────────────────────────
  const compiledGraph: GraphData = useMemo(() => cteDagToGraph(model.dag), [model]);
  const rawGraph: GraphData = useMemo(() => {
    const fileName = (model.path ?? model.name).split("/").pop() ?? model.name;
    const raw = ensureMainNode(rawDagToGraph(model.dag, model.code_map), fileName);
    return rawGraphToGraphData(raw);
  }, [model]);
  const hasRaw = rawGraph.nodes.length > 0 && !!model.code_map?.raw_dag;

  // The DAG source FOLLOWS the shelf: raw DAG while viewing the raw source, the
  // compiled DAG while viewing the compiled pane. Falls to compiled when there's
  // no raw_dag (honest — never a fabricated raw graph).
  const dagMode: "compiled" | "raw" = shelf === "raw" && hasRaw ? "raw" : "compiled";
  const activeGraph = dagMode === "raw" ? rawGraph : compiledGraph;

  // ── the pane text (compiled coords ← code_map.compiled; raw coords ← raw_sql) ─
  // HONEST-EMPTY (cute-dbt#497 finding 2): a model with NO code_map (maps === null)
  // has no source-map spine — the File/raw shelf must NOT render a bare raw_sql
  // listing under a "File" label that contradicts the honest-empty pane. Gate the
  // raw text on the spine, not on raw-text presence: with no maps there is no raw
  // pane text and the File toggle disables.
  const compiledText = model.code_map?.compiled ?? "";
  const rawText = maps ? model.raw_sql ?? model.code_map?.compiled ?? "" : "";
  const paneSide: "compiled" | "raw" = shelf === "compiled" ? "compiled" : "raw";
  const paneText = paneSide === "compiled" ? compiledText : rawText;

  // reset the machine + shelf when the model changes (drop stale cursors/spans).
  const modelKey = model.name;
  const lastModel = useRef(modelKey);
  useEffect(() => {
    if (lastModel.current !== modelKey) {
      lastModel.current = modelKey;
      setShelf("compiled");
      // reset always lands on the compiled shelf → compiled side.
      dispatch({ type: "selectNode", id: null, side: "compiled", maps: maps ?? { nodeSpans: {} } });
    }
  }, [modelKey, maps]);

  // ── FORWARD: a DAG node click → select + scroll the pane to its span ────────
  // the forward sync resolves the span against the ACTIVE shelf's table (compiled
  // `nodeSpans` vs raw `rawNodeSpans`), so a raw-only `zone:N`/`(final select)`
  // node scrolls + flashes the raw pane (cute-dbt#497 finding 1).
  const onSelectNode = useCallback(
    (id: string) => {
      const side: "compiled" | "raw" = shelf === "raw" ? "raw" : "compiled";
      dispatch({ type: "selectNode", id, side, maps: maps ?? { nodeSpans: {} } });
    },
    [maps, shelf],
  );
  const onSelectZone = useCallback((id: string) => {
    dispatch({ type: "selectZone", id });
  }, []);

  // ── REVERSE: a cursor move in the pane → highlight the resolved DAG node ─────
  const onCursorLine = useCallback(
    (line: number | null) => {
      dispatch({ type: "cursor", line, side: paneSide, maps: maps ?? { nodeSpans: {} } });
    },
    [maps, paneSide],
  );

  const span = activeSpan(maps, state, paneSide);
  // the selected zone id → the DAG ring highlight (raw side only).
  const selectedZone = dagMode === "raw" ? state.zone : null;

  return (
    <div data-testid="topology-panes" data-dag-mode={dagMode} data-shelf={shelf} className="flex min-h-0 flex-1 flex-col gap-3 lg:flex-row">
      {/* ── the DAG (compiled CTE | raw), following the shelf ── */}
      <section data-testid="topology-dag" className="min-w-0 flex-1">
        <div className="mb-2 flex items-center gap-2 text-xs uppercase tracking-wide text-zinc-500">
          <span>{dagMode === "raw" ? "Raw DAG" : "Compiled CTE DAG"}</span>
        </div>
        <LineageGraph
          key={"topo-" + model.name + "-" + dagMode}
          data={activeGraph}
          selected={state.node}
          selectedZone={selectedZone}
          onSelect={onSelectNode}
          onSelectZone={onSelectZone}
          recenter={false}
          height={340}
        />
      </section>

      {/* ── the code pane (compiled | file/raw), reflecting the sync ── */}
      <section data-testid="topology-shelf" className="flex min-w-0 flex-1 flex-col">
        <div className="mb-2 flex items-center gap-1 text-xs">
          <span className="mr-2 uppercase tracking-wide text-zinc-500">source</span>
          {(["compiled", "raw"] as const).map((m) => (
            <button
              key={m}
              type="button"
              data-testid="shelf-toggle"
              data-mode={m}
              data-active={shelf === m}
              disabled={m === "raw" && !rawText}
              onClick={() => setShelf(m)}
              className={
                "rounded px-2 py-0.5 font-mono text-[11px] " +
                (shelf === m ? "bg-sky-500/20 text-sky-200" : "text-zinc-400 hover:bg-zinc-800") +
                (m === "raw" && !rawText ? " cursor-not-allowed opacity-40" : "")
              }
            >
              {m === "compiled" ? "Compiled" : "File"}
            </button>
          ))}
        </div>
        <SyncedCompiledView
          text={paneText}
          shiki={shiki}
          span={span}
          cursorLine={state.cursor}
          scrollKey={scrollKey}
          side={paneSide}
          maps={maps}
          onCursorLine={onCursorLine}
        />
      </section>
    </div>
  );
}

/**
 * SyncedCompiledView — the CompiledView + a thin REVERSE-sync click bridge. The
 * pure machine has no DOM; the reverse end (a code line → a DAG node) needs a real
 * click target. A click on a code row reports its 1-based line to `onCursorLine`,
 * which the container feeds to `syncFromCursor`. (Keyboard ↑↓ cursor nav lands in
 * S6c with the shelf; this slice proves the click reverse path + the forward path.)
 */
function SyncedCompiledView({
  text,
  shiki,
  span,
  cursorLine,
  scrollKey,
  side,
  maps,
  onCursorLine,
}: {
  text: string;
  shiki: string;
  span: { start: { line: number }; end: { line: number } } | null;
  cursorLine: number | null;
  scrollKey: number;
  side: "compiled" | "raw";
  maps: SyncMaps | null;
  onCursorLine: (line: number | null) => void;
}): React.ReactElement {
  // a click anywhere in the pane → find the nearest code-line row → reverse-sync.
  const onClick = useCallback(
    (e: React.MouseEvent<HTMLDivElement>) => {
      if (!maps) return;
      const row = (e.target as HTMLElement).closest<HTMLElement>('[data-testid="code-line"]');
      if (!row) return;
      const n = Number(row.getAttribute("data-line"));
      if (Number.isFinite(n) && n > 0) onCursorLine(n);
    },
    [maps, onCursorLine],
  );
  return (
    <div data-testid="synced-pane" data-side={side} onClick={onClick} className="min-h-0 flex-1">
      <CompiledView text={text} lang="sql" shiki={shiki} span={span} cursorLine={cursorLine} scrollKey={scrollKey} flash />
    </div>
  );
}
