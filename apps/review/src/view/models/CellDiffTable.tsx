// CellDiffTable — the cell-level given/expect diff grid (S7). old→new per changed
// cell, +/− row marks, and the NULL / absent / empty trichotomy rendered
// EXPLICITLY: a SQL NULL ("NULL" token) ≠ a missing cell ("·") ≠ an empty-but-
// present value ("∅"). The honesty fold itself (diffSide → NormSide) lives in
// domain/data/cell-diff.ts (the 100%-kill module); this view renders each state
// distinctly so a reviewer can always tell them apart. Ported from the prototype's
// views.js CellDiffTable / cellSide, onto Tailwind (no htm, no shadcn — S11).
//
// LAYER: view (imports React + the domain cell-diff TYPES only; never chrome).
import React from "react";
import type { NormDiffTable, NormSide } from "../../domain/data/cell-diff";

/** Render one trichotomy-folded side: absent → "·", NULL → the NULL token,
 *  present → the value ("" → "∅", the empty-but-present glyph). `cls` tints a
 *  present value (added/removed/changed). Never a blank that hides the state. */
function CellSide({ side, cls }: { side: NormSide; cls?: string }): React.ReactElement {
  if (side.absent) return <span className="select-none text-zinc-700">·</span>;
  if (side.null)
    return (
      <span className="rounded border border-zinc-700 bg-zinc-800 px-1 text-[11px] italic tracking-wide text-zinc-400">
        NULL
      </span>
    );
  return <span className={cls}>{side.text === "" ? "∅" : side.text}</span>;
}

const ROW_MARK: Record<string, { s: string; cls: string }> = {
  added: { s: "+", cls: "text-emerald-400" },
  removed: { s: "−", cls: "text-rose-400" },
  modified: { s: "~", cls: "text-sky-400" },
  unchanged: { s: "", cls: "text-zinc-700" },
};
const COL_TINT: Record<string, string> = {
  added: "text-emerald-400",
  removed: "text-rose-400 line-through",
};

export interface CellDiffTableProps {
  table: NormDiffTable | null;
  /** "diff" = old→new where changed + row +/− treatment; "file" = new state only. */
  mode?: "diff" | "file";
}

/**
 * CellDiffTable — render a normalized cell-diff table. Honest-empty (a dashed
 * note, never a fabricated row) when the table is null or columnless. In diff
 * mode each changed cell shows old→new (both sides through the trichotomy); in
 * file mode only the new (present) state.
 */
export function CellDiffTable({ table, mode = "diff" }: CellDiffTableProps): React.ReactElement {
  if (!table || table.columns.length === 0) {
    return (
      <div
        data-testid="cell-diff-empty"
        className="rounded-lg border border-dashed border-zinc-800 px-3 py-2 font-mono text-xs text-zinc-500"
      >
        no rows — treated as empty
      </div>
    );
  }
  const allNew = table.columns.every((c) => c.status === "added");
  return (
    <div className="overflow-x-auto rounded-lg border border-zinc-800 bg-zinc-900/60">
      <table className="w-full font-mono text-xs">
        <thead>
          <tr className="border-b border-zinc-800 text-left uppercase tracking-wide text-zinc-500">
            {mode === "diff" && <th className="w-6 px-1 py-2" />}
            {table.columns.map((c) => (
              <th
                key={c.name}
                className={"px-3 py-2 font-medium align-bottom " + (mode === "diff" ? (COL_TINT[c.status] ?? "") : "")}
                style={{ maxWidth: "12rem" }}
              >
                {c.name}
                {mode === "diff" && c.status === "added" && !allNew && (
                  <span className="ml-1 text-[9px]">new</span>
                )}
              </th>
            ))}
          </tr>
        </thead>
        <tbody>
          {table.rows.map((r, i) => {
            const mk = ROW_MARK[r.kind] ?? ROW_MARK.modified!;
            const rowBg =
              mode === "diff"
                ? r.kind === "added"
                  ? "bg-emerald-500/10"
                  : r.kind === "removed"
                    ? "bg-rose-500/10"
                    : r.kind === "unchanged"
                      ? "opacity-60"
                      : ""
                : "";
            return (
              <tr key={i} data-testid="cell-diff-row" data-kind={r.kind} className={"border-b border-zinc-800/70 last:border-0 " + rowBg}>
                {mode === "diff" && (
                  <td className={"select-none text-center font-bold " + mk.cls}>{mk.s}</td>
                )}
                {r.cells.map((cell, j) => {
                  let body: React.ReactNode;
                  if (mode === "file") {
                    const s = cell.new.absent ? cell.old : cell.new;
                    body = <CellSide side={s} cls={cell.changed ? "rounded bg-emerald-500/15 px-1 text-emerald-300" : "text-zinc-200"} />;
                  } else if (r.kind === "added") {
                    body = <CellSide side={cell.new} cls="text-emerald-400" />;
                  } else if (r.kind === "removed") {
                    body = <CellSide side={cell.old} cls="text-rose-400/80 line-through" />;
                  } else if (cell.changed) {
                    body = (
                      <span className="inline-flex items-center gap-1">
                        <CellSide side={cell.old} cls="rounded bg-rose-500/15 px-1 text-rose-400/80 line-through" />
                        <span className="text-zinc-600">→</span>
                        <CellSide side={cell.new} cls="rounded bg-emerald-500/15 px-1 text-emerald-300" />
                      </span>
                    );
                  } else {
                    body = <CellSide side={cell.new.absent ? cell.old : cell.new} cls="text-zinc-200" />;
                  }
                  return (
                    <td key={j} className="px-3 py-1.5 align-top">
                      {body}
                    </td>
                  );
                })}
              </tr>
            );
          })}
        </tbody>
      </table>
    </div>
  );
}
