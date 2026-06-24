// The non-Models entity review surface (S8 / cute-dbt#500) — renders the Macros /
// Seeds / Sources+Tests review panes from the pure `domain/entity-views`
// aggregation. The Entity×View matrix routes `macros`/`seeds`/`else · review` here
// (the single `entity-review` route); this component switches on the entity kind.
//
// HONESTY (never-a-false-claim): every fact is READ from the reshaped data, never
// fabricated. A macro shows its REAL signature/usages; a seed its REAL columns /
// types / row count / change state; the Else pane the REAL per-column TEST
// inventory the spine attached to `manifest_nodes` AND an honest-empty SOURCES
// panel (the 440 spine carries no discrete source node — never an invented table
// identity). An entity kind with NO instances renders an honest-empty state, NOT a
// placeholder card. The change-state vocabulary matches the Models surfaces.
//
// LAYER: view (imports domain; never chrome). Server-renderable (no browser-only
// APIs) so the Vitest `renderToStaticMarkup` tests + the Playwright e2e both
// exercise it.
import React from "react";
import type {
  MacroView, SeedView, ManifestIndex, TestInventory, SourceNode,
} from "../../domain/entity-views";

// ── shared change-chip vocabulary (mirrors the Models / PR surfaces) ─────────
const CHANGE_TONE: Record<string, { bg: string; fg: string; label: string }> = {
  added: { bg: "rgba(158,206,106,0.16)", fg: "#9ece6a", label: "added" },
  new: { bg: "rgba(158,206,106,0.16)", fg: "#9ece6a", label: "new" },
  modified: { bg: "rgba(122,162,247,0.16)", fg: "#7aa2f7", label: "modified" },
  removed: { bg: "rgba(247,118,142,0.16)", fg: "#f7768e", label: "removed" },
  base: { bg: "rgba(108,112,134,0.16)", fg: "#6c7086", label: "unchanged" },
};

function ChangeChip({ change }: { change: string }): React.ReactElement {
  const t = CHANGE_TONE[change] ?? CHANGE_TONE.base!;
  return (
    <span
      data-testid="entity-change-chip"
      data-change={change}
      style={{
        fontSize: 10, textTransform: "uppercase", letterSpacing: "0.04em",
        borderRadius: 4, padding: "1px 6px", fontFamily: "ui-monospace, monospace",
        background: t.bg, color: t.fg,
      }}
    >
      {t.label}
    </span>
  );
}

function PaneHead({ icon, path, change }: { icon: string; path: string; change?: string }): React.ReactElement {
  return (
    <div style={{ display: "flex", alignItems: "center", gap: 8, marginBottom: 14 }}>
      <span style={{ fontFamily: "ui-monospace, monospace", fontSize: 12 }}>{icon}</span>
      <span data-testid="entity-path" style={{ fontFamily: "ui-monospace, monospace", fontSize: 13, fontWeight: 600, color: "#c0caf5" }}>
        {path}
      </span>
      {change && <ChangeChip change={change} />}
    </div>
  );
}

function EmptyState({ label }: { label: string }): React.ReactElement {
  return (
    <div data-testid="entity-empty" className="min-w-0 flex-1 overflow-auto p-6">
      <p style={{ fontSize: 13, color: "#6c7086" }}>{label}</p>
    </div>
  );
}

function SectionLabel({ children }: { children: React.ReactNode }): React.ReactElement {
  return (
    <div style={{ fontSize: 10, textTransform: "uppercase", letterSpacing: "0.06em", color: "#6c7086", margin: "16px 0 8px" }}>
      {children}
    </div>
  );
}

// ── MACROS ────────────────────────────────────────────────────────────────────

function MacroPane({ macro }: { macro: MacroView }): React.ReactElement {
  return (
    <div data-testid="entity-macro" data-macro={macro.name} className="min-w-0 flex-1 overflow-auto p-6" style={{ maxWidth: 920, margin: "0 auto" }}>
      <PaneHead icon="ƒ" path={macro.path} />

      {/* signature — the REAL macro signature line + extracted arg list. */}
      <div data-testid="macro-signature" style={{ fontFamily: "ui-monospace, monospace", fontSize: 15, fontWeight: 600, color: "#c0caf5" }}>
        {macro.name}
        <span style={{ color: "#6c7086" }}>(</span>
        <span style={{ color: "#7aa2f7" }}>{macro.args}</span>
        <span style={{ color: "#6c7086" }}>)</span>
        <span data-testid="macro-package" style={{ marginLeft: 10, fontSize: 11, fontWeight: 400, color: "#6c7086" }}>
          {macro.package}
        </span>
      </div>

      {/* description — HONEST-EMPTY when the spine emits none. */}
      {macro.description ? (
        <p style={{ fontSize: 13, color: "#a9b1d6", marginTop: 8, lineHeight: 1.5 }}>{macro.description}</p>
      ) : (
        <p data-testid="macro-no-desc" style={{ fontSize: 12, fontFamily: "ui-monospace, monospace", color: "#6c7086", marginTop: 8 }}>
          no description in this context
        </p>
      )}

      {/* source — the REAL macro body lines (verbatim). */}
      <SectionLabel>Source</SectionLabel>
      <pre data-testid="macro-body" style={{ margin: 0, padding: 12, borderRadius: 6, background: "rgba(26,27,38,0.6)", border: "1px solid #2a2b36", overflow: "auto", fontFamily: "ui-monospace, monospace", fontSize: 12, color: "#a9b1d6", lineHeight: 1.5 }}>
        {macro.bodyLines.map((l) => l.text).join("\n")}
      </pre>

      {/* call sites — the REAL impacted models, or an honest no-callers note. */}
      <SectionLabel>Call sites · {macro.impactedCount}</SectionLabel>
      {macro.usages.length > 0 ? (
        <ul style={{ margin: 0, padding: 0, listStyle: "none", display: "flex", flexDirection: "column", gap: 4 }}>
          {macro.usages.map((u) => (
            <li key={u.modelId || u.name} data-testid="macro-usage" data-model={u.name} style={{ display: "flex", alignItems: "baseline", gap: 8, fontFamily: "ui-monospace, monospace", fontSize: 12 }}>
              <span style={{ color: "#c0caf5", fontWeight: 600 }}>{u.name}</span>
              {u.path && <span style={{ color: "#6c7086", fontSize: 11 }}>{u.path}</span>}
            </li>
          ))}
        </ul>
      ) : (
        <p data-testid="macro-no-usages" style={{ fontSize: 12, fontFamily: "ui-monospace, monospace", color: "#6c7086", margin: 0 }}>
          no callers in this project
        </p>
      )}
    </div>
  );
}

// ── SEEDS ─────────────────────────────────────────────────────────────────────

function SeedPane({ seed }: { seed: SeedView }): React.ReactElement {
  const rowLabel =
    seed.totalRows != null && seed.shownRows != null && seed.capped
      ? `${seed.shownRows} of ${seed.totalRows} rows`
      : `${seed.totalRows ?? seed.rowCount} row${(seed.totalRows ?? seed.rowCount) === 1 ? "" : "s"}`;
  return (
    <div data-testid="entity-seed" data-seed={seed.name} className="min-w-0 flex-1 overflow-auto p-6" style={{ maxWidth: 920, margin: "0 auto" }}>
      <PaneHead icon="▦" path={seed.file} change={seed.change} />

      {seed.description && (
        <p style={{ fontSize: 13, color: "#a9b1d6", marginBottom: 8, lineHeight: 1.5 }}>{seed.description}</p>
      )}

      {/* columns + types — the REAL seed schema (honest "no type" when absent). */}
      <SectionLabel>Columns · {seed.columns.length} · <span style={{ textTransform: "none", letterSpacing: 0 }}>{rowLabel}</span></SectionLabel>
      <ul style={{ margin: 0, padding: 0, listStyle: "none", display: "flex", flexDirection: "column", gap: 4 }}>
        {seed.columns.map((c) => (
          <li key={c} data-testid="seed-column" data-column={c} style={{ display: "flex", alignItems: "baseline", gap: 8, fontFamily: "ui-monospace, monospace", fontSize: 12 }}>
            <span style={{ color: "#c0caf5", fontWeight: 600 }}>{c}</span>
            {seed.colTypes[c] ? (
              <span style={{ color: "#7aa2f7", fontSize: 11 }}>{seed.colTypes[c]}</span>
            ) : (
              <span style={{ color: "#6c7086", fontSize: 11, opacity: 0.6 }}>no type</span>
            )}
          </li>
        ))}
      </ul>

      {/* data preview — the REAL cell rows the spine carried. */}
      {seed.diffRows.length > 0 && (
        <>
          <SectionLabel>Data preview</SectionLabel>
          <div style={{ overflow: "auto", border: "1px solid #2a2b36", borderRadius: 6 }}>
            <table data-testid="seed-data" style={{ borderCollapse: "collapse", fontFamily: "ui-monospace, monospace", fontSize: 11, width: "100%" }}>
              <thead>
                <tr>
                  {seed.columns.map((c) => (
                    <th key={c} style={{ textAlign: "left", padding: "4px 10px", color: "#6c7086", borderBottom: "1px solid #2a2b36", whiteSpace: "nowrap" }}>{c}</th>
                  ))}
                </tr>
              </thead>
              <tbody>
                {seed.diffRows.slice(0, 20).map((r, ri) => (
                  <tr key={ri} data-testid="seed-data-row">
                    {r.cells.map((cell, ci) => (
                      <td key={ci} data-changed={cell.changed ? "true" : "false"} style={{ padding: "4px 10px", color: cell.changed ? "#7aa2f7" : "#a9b1d6", borderBottom: "1px solid #1f2030", whiteSpace: "nowrap" }}>
                        {cell.v}
                      </td>
                    ))}
                  </tr>
                ))}
              </tbody>
            </table>
          </div>
        </>
      )}

      {/* downstream feeds — the REAL models this seed feeds. */}
      {seed.downstream.length > 0 && (
        <>
          <SectionLabel>Feeds · {seed.downstream.length}</SectionLabel>
          <ul data-testid="seed-downstream" style={{ margin: 0, padding: 0, listStyle: "none", display: "flex", gap: 8, flexWrap: "wrap" }}>
            {seed.downstream.map((m) => (
              <li key={m} style={{ fontFamily: "ui-monospace, monospace", fontSize: 11, color: "#c0caf5", border: "1px solid #2a2b36", borderRadius: 4, padding: "2px 8px" }}>{m}</li>
            ))}
          </ul>
        </>
      )}
    </div>
  );
}

// ── ELSE — Sources + the per-column Test inventory ──────────────────────────────

function SourcesPanel({ sources }: { sources: SourceNode[] }): React.ReactElement {
  return (
    <section data-testid="sources-panel">
      <SectionLabel>Sources · {sources.length}</SectionLabel>
      {sources.length > 0 ? (
        <ul style={{ margin: 0, padding: 0, listStyle: "none", display: "flex", flexDirection: "column", gap: 4 }}>
          {sources.map((s) => (
            <li key={s.id} data-testid="source-node" data-source={s.id} style={{ display: "flex", alignItems: "baseline", gap: 8, fontFamily: "ui-monospace, monospace", fontSize: 12 }}>
              <span style={{ color: "#7aa2f7" }}>{s.sourceName}</span>
              <span style={{ color: "#6c7086" }}>.</span>
              <span style={{ color: "#c0caf5", fontWeight: 600 }}>{s.identifier || s.name}</span>
            </li>
          ))}
        </ul>
      ) : (
        <p data-testid="sources-empty" style={{ fontSize: 12, fontFamily: "ui-monospace, monospace", color: "#6c7086", margin: 0 }}>
          no sources in this context — the cute-dbt spine carries no discrete source node yet (a tracked T2 spine gap); rendered honestly empty rather than fabricated.
        </p>
      )}
    </section>
  );
}

function TestInventoryPanel({ inventory }: { inventory: TestInventory }): React.ReactElement {
  const kinds = Object.entries(inventory.byKind).sort((a, b) => b[1] - a[1]);
  return (
    <section data-testid="test-inventory" data-total={inventory.total}>
      <SectionLabel>Tests · {inventory.total}</SectionLabel>
      {inventory.total > 0 ? (
        <>
          {/* per-kind rollup — the REAL test-kind counts. */}
          <div style={{ display: "flex", gap: 8, flexWrap: "wrap", marginBottom: 10 }}>
            {kinds.map(([kind, n]) => (
              <span key={kind} data-testid="test-kind" data-kind={kind} data-count={n} style={{ fontFamily: "ui-monospace, monospace", fontSize: 11, color: "#a9b1d6", border: "1px solid #2a2b36", borderRadius: 4, padding: "2px 8px" }}>
                {kind} · {n}
              </span>
            ))}
          </div>
          {/* the REAL (model, column, test) triples. */}
          <ul style={{ margin: 0, padding: 0, listStyle: "none", display: "flex", flexDirection: "column", gap: 3 }}>
            {inventory.entries.map((e, i) => (
              <li key={`${e.model}.${e.column}.${e.test}.${i}`} data-testid="test-entry" style={{ display: "flex", alignItems: "baseline", gap: 8, fontFamily: "ui-monospace, monospace", fontSize: 12 }}>
                <span style={{ color: "#9ece6a" }}>{e.test}</span>
                <span style={{ color: "#6c7086" }}>on</span>
                <span style={{ color: "#c0caf5" }}>{e.model}</span>
                <span style={{ color: "#6c7086" }}>.</span>
                <span style={{ color: "#a9b1d6" }}>{e.column}</span>
              </li>
            ))}
          </ul>
        </>
      ) : (
        <p data-testid="tests-empty" style={{ fontSize: 12, fontFamily: "ui-monospace, monospace", color: "#6c7086", margin: 0 }}>
          no column tests in this context.
        </p>
      )}
    </section>
  );
}

function ElsePane({ index, inventory }: { index: ManifestIndex; inventory: TestInventory }): React.ReactElement {
  return (
    <div data-testid="entity-else" className="min-w-0 flex-1 overflow-auto p-6" style={{ maxWidth: 920, margin: "0 auto" }}>
      <h2 style={{ fontSize: 13, fontWeight: 600, color: "#c0caf5", margin: 0 }}>Project · sources &amp; tests</h2>
      <p style={{ fontSize: 12, color: "#6c7086", margin: "4px 0 0" }}>
        the REAL manifest-node facts the context carries — {index.nodes.length} node{index.nodes.length === 1 ? "" : "s"} indexed.
      </p>
      <SourcesPanel sources={index.sources} />
      <TestInventoryPanel inventory={inventory} />
    </div>
  );
}

// ── the entity-review dispatcher ────────────────────────────────────────────────

export interface EntityReviewProps {
  entity: "macros" | "seeds" | "else";
  /** the active instance id (the SubHeader instance selector). */
  sel: string | null;
  macros: MacroView[];
  seeds: SeedView[];
  index: ManifestIndex;
  inventory: TestInventory;
}

/** Resolve the active instance: the selected one, else the first (mirrors the
 *  SubHeader's `sel ?? instances[0]` resolution). */
function active<T extends { name: string }>(list: T[], sel: string | null): T | undefined {
  return (sel != null ? list.find((x) => x.name === sel) : undefined) ?? list[0];
}

export function EntityReview(p: EntityReviewProps): React.ReactElement {
  if (p.entity === "macros") {
    const macro = active(p.macros, p.sel);
    if (!macro) return <EmptyState label="No macros in this context." />;
    return <MacroPane macro={macro} />;
  }
  if (p.entity === "seeds") {
    const seed = active(p.seeds, p.sel);
    if (!seed) return <EmptyState label="No seeds in this context." />;
    return <SeedPane seed={seed} />;
  }
  // else — the project Sources + Tests surface (always renders, honest-empty inside).
  return <ElsePane index={p.index} inventory={p.inventory} />;
}
