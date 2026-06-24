// FixtureTable — the plain given/expect grid (S7, file mode / new state, no diff).
// A columns header + one row per fixture row; an absent cell renders an explicit
// em-dash (never a silent blank). Honest-empty when a fixture carries no columns.
// The AdaptedTest's given/expect rows are already string|undefined cells from the
// spine (dataset.adaptTest) — this is a pure presentational grid.
//
// LAYER: view (imports React only; never chrome).
import React from "react";

export interface FixtureTableProps {
  columns: string[];
  rows: (string | undefined)[][];
}

export function FixtureTable({ columns, rows }: FixtureTableProps): React.ReactElement {
  if (columns.length === 0) {
    return (
      <div
        data-testid="fixture-empty"
        className="rounded-lg border border-dashed border-zinc-800 px-3 py-2 font-mono text-xs text-zinc-500"
      >
        no columns — treated as empty
      </div>
    );
  }
  return (
    <div data-testid="fixture-table" className="overflow-x-auto rounded-lg border border-zinc-800 bg-zinc-900/60">
      <table className="w-full font-mono text-xs">
        <thead>
          <tr className="border-b border-zinc-800 text-left uppercase tracking-wide text-zinc-500">
            {columns.map((c) => (
              <th key={c} className="px-3 py-2 font-medium align-bottom" style={{ maxWidth: "12rem" }}>
                {c}
              </th>
            ))}
          </tr>
        </thead>
        <tbody>
          {rows.map((r, i) => (
            <tr key={i} data-testid="fixture-row" className="border-b border-zinc-800/70 last:border-0">
              {columns.map((_, j) => {
                const v = r[j];
                return (
                  <td key={j} className="px-3 py-1.5 align-top text-zinc-200">
                    {v == null || v === "" ? <span className="text-zinc-600">—</span> : v}
                  </td>
                );
              })}
            </tr>
          ))}
        </tbody>
      </table>
    </div>
  );
}
