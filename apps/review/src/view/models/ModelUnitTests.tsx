// ModelUnitTests — the Models · Unit tests view (S7). A model's unit tests
// rendered from the context: a test selector + the given/expect tables with the
// cell-level diff treatment (the domain cell-diff facts, reused — never re-folded
// here). HONESTY is the spine:
//   - a model with NO unit tests → an honest no-unit-tests empty state.
//   - an external-fixture given the payload only POINTS at (g.fixture / t.external)
//     → an honest note naming the file, never a fabricated grid.
//   - in diff mode a changed test shows old→new cell diffs (CellDiffTable); the
//     new-state / file mode shows the plain FixtureTable.
// The per-test reshape is the domain adaptTest (dataset.ts) — this view is a pure
// renderer. Ported from the prototype's views.js DataView, onto Tailwind.
//
// LAYER: view (imports React + domain + domain/data; never chrome).
import React, { useEffect, useState } from "react";
import type { ModelPayload } from "../../domain/context-data";
import { adaptTest, type AdaptedTest } from "../../domain/data/dataset";
import { CellDiffTable } from "./CellDiffTable";
import { FixtureTable } from "./FixtureTable";

export interface ModelUnitTestsProps {
  model: ModelPayload;
  /** the active test index. Controlled when provided (the static-render tests pin
   *  it); uncontrolled (self-managed) when omitted — the chrome mounts it bare. */
  testIdx?: number;
  /** "diff" = old→new cell diffs; "file" = the new-state fixture grid. Controlled
   *  when provided; defaults to "diff" + a first-party Diff/File toggle otherwise. */
  dataMode?: "diff" | "file";
  /** select a different test (the test selector handler; optional in static render). */
  onTestIdx?: (i: number) => void;
}

/** the given section — diff mode renders the cell-diff per given table; file mode
 *  the plain fixture grid. An external-fixture given carries an honest badge. */
function GivenSection({ t, dataMode }: { t: AdaptedTest; dataMode: "diff" | "file" }): React.ReactElement {
  const dd = t.dataDiff;
  const givenCount = dataMode === "diff" && dd ? dd.given.length : t.given.length;
  return (
    <section data-testid="ut-given" className="space-y-2">
      <div className="text-xs uppercase tracking-wide text-zinc-500">
        given · {givenCount} input{givenCount === 1 ? "" : "s"}{" "}
        <span className="text-zinc-600">{dataMode === "diff" ? "· old → new where changed" : "· new state"}</span>
      </div>
      {dataMode === "diff" && dd
        ? dd.given.map((g, i) => (
            <div key={i} className="space-y-1">
              <div className="font-mono text-[11px] text-zinc-500">{g.input}</div>
              <CellDiffTable table={g.table} mode="diff" />
            </div>
          ))
        : t.given.map((g, i) => (
            <div key={i} className="space-y-1">
              <div className="flex items-center gap-2 font-mono text-[11px] text-zinc-500">
                {g.input}
                {g.external && (
                  <span className="rounded border border-zinc-700 bg-zinc-900 px-1.5 py-0.5 text-[10px] text-zinc-400">external csv</span>
                )}
              </div>
              <FixtureTable columns={g.columns} rows={g.rows} />
            </div>
          ))}
    </section>
  );
}

/** the expect section — the single expect table (diff or new-state). */
function ExpectSection({ t, dataMode }: { t: AdaptedTest; dataMode: "diff" | "file" }): React.ReactElement {
  const dd = t.dataDiff;
  return (
    <section data-testid="ut-expect" className="space-y-2">
      <div className="text-xs uppercase tracking-wide text-zinc-500">
        expect <span className="text-zinc-600">{dataMode === "diff" ? "· old → new where changed" : "· new state"}</span>
      </div>
      {dataMode === "diff" && dd && dd.expect ? (
        <CellDiffTable table={dd.expect} mode="diff" />
      ) : dataMode === "diff" && dd && !dd.expect ? (
        <div className="rounded-lg border border-dashed border-zinc-800 px-3 py-2 font-mono text-xs text-zinc-500">
          no expect diff — {t.incMode ? "(is_incremental() = true) " : ""}path asserts via given only
        </div>
      ) : (
        <FixtureTable columns={t.expect.columns} rows={t.expect.rows.map((r) => r.cells.map((c) => c.v))} />
      )}
    </section>
  );
}

export function ModelUnitTests(props: ModelUnitTestsProps): React.ReactElement {
  const { model, onTestIdx } = props;
  const tests: AdaptedTest[] = (model.tests ?? []).map(adaptTest);

  // uncontrolled-friendly: own a test cursor + a Diff/File mode when the chrome
  // mounts the view bare (the controlled props win when provided — the tests).
  const [localIdx, setLocalIdx] = useState(0);
  const [localMode, setLocalMode] = useState<"diff" | "file">("diff");
  // re-home the local cursor when the active model changes (a stale index would
  // point at the wrong model's test grid — the wrong-content false claim).
  useEffect(() => {
    setLocalIdx(0);
  }, [model.name]);
  const controlledIdx = props.testIdx !== undefined;
  const testIdx = controlledIdx ? props.testIdx! : localIdx;
  const setIdx = (i: number): void => {
    if (controlledIdx) onTestIdx?.(i);
    else setLocalIdx(i);
  };
  const controlledMode = props.dataMode !== undefined;
  const dataMode = controlledMode ? props.dataMode! : localMode;

  if (tests.length === 0) {
    return (
      <div data-testid="model-unit-tests" data-model={model.name} className="min-w-0 flex-1 overflow-auto p-6">
        <div data-testid="ut-empty" className="max-w-2xl rounded-lg border border-dashed border-zinc-800 bg-zinc-900/40 p-4">
          <div className="mb-1 text-[13px] font-medium text-zinc-200">no unit tests</div>
          <div className="font-mono text-xs text-zinc-500">
            {model.name} ships without a unit test in this PR.
            {model.state === "new" || model.state === "added" ? " New models should carry at least one." : ""}
          </div>
        </div>
      </div>
    );
  }

  const ti = Math.min(Math.max(0, testIdx), tests.length - 1);
  const t = tests[ti]!;
  const changedCount = tests.filter((x) => x.changed || x.isNew).length;
  const overrideOnly = !t.dataDiff && !!t.yamlDiff;

  return (
    <div data-testid="model-unit-tests" data-model={model.name} className="min-w-0 flex-1 space-y-4 overflow-auto p-6">
      {/* test selector + state chips */}
      <div className="flex flex-wrap items-center gap-2">
        <span className="text-xs uppercase tracking-wide text-zinc-500">unit test</span>
        <select
          data-testid="ut-test-select"
          aria-label="Select unit test"
          value={ti}
          onChange={(e) => setIdx(Number(e.target.value))}
          className="rounded-md border border-zinc-700 bg-zinc-900 px-2 py-1 font-mono text-xs text-zinc-200 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-sky-500/55"
        >
          {tests.map((x, i) => (
            <option key={x.name} value={i}>
              {x.name}
            </option>
          ))}
        </select>
        <span className="shrink-0 font-mono text-[11px] text-zinc-500">
          {ti + 1}/{tests.length}
        </span>
        {t.isNew ? (
          <span data-testid="ut-state-badge" className="rounded border border-emerald-500/40 bg-emerald-500/10 px-1.5 py-0.5 font-mono text-[10px] uppercase text-emerald-300">
            new
          </span>
        ) : t.changed ? (
          <span data-testid="ut-state-badge" className="rounded border border-sky-500/40 bg-sky-500/10 px-1.5 py-0.5 font-mono text-[10px] uppercase text-sky-300">
            changed
          </span>
        ) : null}
        {changedCount > 0 && (
          <span className="font-mono text-[10px] text-zinc-500">
            · {changedCount} of {tests.length} changed
          </span>
        )}
        <span className="flex-1" />
        {/* the Diff/File mode toggle — shown only when this view owns the mode
            (uncontrolled). When the chrome controls dataMode it owns the toggle. */}
        {!controlledMode && (
          <div role="radiogroup" aria-label="data mode" data-testid="ut-mode-toggle" className="inline-flex rounded-md border border-zinc-700 bg-zinc-900 p-0.5">
            {(["diff", "file"] as const).map((m) => (
              <button
                key={m}
                type="button"
                role="radio"
                aria-checked={dataMode === m}
                data-mode={m}
                data-active={dataMode === m ? "true" : "false"}
                onClick={() => setLocalMode(m)}
                className={
                  "rounded px-2.5 py-1 text-xs font-medium capitalize focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-sky-500/55 " +
                  (dataMode === m ? "bg-sky-500/20 text-sky-200" : "text-zinc-400 hover:text-zinc-200")
                }
              >
                {m}
              </button>
            ))}
          </div>
        )}
      </div>

      {/* affordance chips — incremental run / external fixture */}
      <div className="flex flex-wrap items-center gap-2 text-[11px]">
        {t.incMode && (
          <span className="inline-flex items-center gap-1 rounded border border-amber-500/40 bg-amber-500/10 px-1.5 py-0.5 font-mono text-[10px] uppercase tracking-wide text-amber-300">
            incremental run · is_incremental() = true
          </span>
        )}
        {t.external && (
          <span data-testid="ut-external-fixture" className="inline-flex items-center gap-1 rounded border border-zinc-700 bg-zinc-900 px-1.5 py-0.5 font-mono text-[10px] text-zinc-400">
            external fixture <span className="text-zinc-200">{t.external.file}</span>
          </span>
        )}
        {overrideOnly && (
          <span className="rounded border border-zinc-700 bg-zinc-900 px-1.5 py-0.5 font-mono text-[10px] uppercase tracking-wide text-zinc-400">
            override-only · no data change
          </span>
        )}
      </div>

      {/* description */}
      {t.description && <p className="max-w-3xl text-[13px] leading-relaxed text-zinc-300">{t.description}</p>}

      {/* given + expect grids */}
      <GivenSection t={t} dataMode={dataMode} />
      <ExpectSection t={t} dataMode={dataMode} />
    </div>
  );
}
