// ModelDetails — the Models · Details view (S7). The per-model identity + config
// facts the payload carries: name, change-state badge, materialization chip, tags,
// the description, a node-config table (materialized / unique_key grain / schema /
// governance / config / meta), a lineage summary (the CTE-DAG node/edge counts),
// and the documented columns (name / type / tests / description) from the column-
// lineage context. EVERY facet the payload lacks renders an honest-empty note —
// never a fabricated value (the never-a-false-claim spine).
//
// The config facts come from the domain spine (deriveInfo) + the model payload
// directly; nothing is recomputed here. Ported from the prototype's views.js
// NodeView, onto Tailwind (no htm/shadcn — S11 design pass).
//
// LAYER: view (imports React + domain + domain/data; never chrome).
import React from "react";
import type { ModelPayload, ColumnContextEntry, ColumnTest } from "../../domain/context-data";
import { deriveInfo, stateToChange } from "../../domain/data/dataset";

const STATE_TONE: Record<string, string> = {
  added: "border-emerald-500/40 bg-emerald-500/10 text-emerald-300",
  removed: "border-rose-500/40 bg-rose-500/10 text-rose-300",
  modified: "border-sky-500/40 bg-sky-500/10 text-sky-300",
};

/** A config row: a key, a value (null ⇒ honest "not configured"), an optional hint. */
interface ConfigRow {
  k: string;
  v: string | null;
  hint?: string;
}

/** the documented-column rows from the column-lineage context (the preferred,
 *  resolved source — descriptions + the COMPLETE test list). Honest-empty array
 *  when the payload carries no column lineage. */
function columnRows(m: ModelPayload): { name: string; type: string | null; tests: string[]; desc: string | null }[] {
  const ctx = m.column_lineage?.context;
  if (!ctx) return [];
  return Object.keys(ctx).map((name) => {
    const e: ColumnContextEntry = ctx[name]!;
    const tests = (e.tests ?? []).map((t) => (typeof t === "string" ? t : (t as ColumnTest).kind));
    return { name, type: e.data_type ?? null, tests, desc: e.description ?? null };
  });
}

export function ModelDetails({ model }: { model: ModelPayload }): React.ReactElement {
  const info = deriveInfo(model);
  const isIncremental = info.materialized === "incremental";
  const cols = columnRows(model);
  const change = stateToChange(model.state);

  // the node-config rows — the prototype's NodeView config table, sourced from the
  // domain-derived info. A row with a null value renders an honest "not configured".
  const rows: ConfigRow[] = [
    { k: "materialized", v: info.materialized },
    {
      k: "unique_key (grain)",
      v: info.grain.known ? info.grain.value : null,
      hint: info.grain.known ? "via " + info.grain.source : undefined,
    },
    ...Object.keys(info.gov).map((k) => ({ k, v: info.gov[k] ?? null })),
    ...Object.keys(info.config).map((k) => ({ k, v: info.config[k] ?? null })),
    { k: "tags", v: info.tags.length ? info.tags.join(", ") : null },
    ...info.meta.map((m) => ({ k: m.key, v: m.value })),
  ];
  const configured = rows.filter((r) => r.v != null);

  return (
    <div data-testid="model-details" data-model={model.name} className="min-w-0 flex-1 space-y-5 overflow-auto p-6">
      {/* identity */}
      <div className="flex flex-wrap items-center gap-2">
        <h2 className="font-mono text-base font-semibold text-zinc-100">{model.name}</h2>
        <span
          data-testid="model-state-badge"
          data-state={change}
          className={"rounded border px-1.5 py-0.5 font-mono text-[10px] uppercase tracking-wide " + (STATE_TONE[change] ?? "border-zinc-700 text-zinc-400")}
        >
          {change}
        </span>
        <span
          data-testid="model-materialization"
          className={
            "rounded border px-1.5 py-0.5 font-mono text-[10px] uppercase tracking-wide " +
            (isIncremental ? "border-amber-500/40 bg-amber-500/10 text-amber-300" : "border-zinc-700 bg-zinc-900 text-zinc-400")
          }
        >
          {info.materialized}
        </span>
        {info.tags.map((t) => (
          <span key={t} className="rounded border border-zinc-700 bg-zinc-900 px-1.5 py-0.5 font-mono text-[10px] text-zinc-400">
            #{t}
          </span>
        ))}
      </div>

      {/* description */}
      {model.description ? (
        <p className="max-w-3xl text-[13px] leading-relaxed text-zinc-300">{model.description}</p>
      ) : (
        <p className="text-[13px] italic text-zinc-600">no description configured</p>
      )}

      {/* node config */}
      <section>
        <div className="mb-1.5 text-xs uppercase tracking-wide text-zinc-500">node config</div>
        <div data-testid="node-config" className="divide-y divide-zinc-800 overflow-hidden rounded-lg border border-zinc-800 bg-zinc-900/60">
          {!model.model_yaml && configured.length <= 1 && (
            <div data-testid="config-no-yaml" className="px-3 py-2 font-mono text-xs text-zinc-500">
              no schema YAML for this model — only the materialization is known
            </div>
          )}
          {configured.map((r) => (
            <div key={r.k} className="flex items-center gap-3 px-3 py-1.5 font-mono text-xs">
              <span className="h-2 w-2 shrink-0 rounded-sm bg-sky-500" title="configured" />
              <span className="w-44 shrink-0 text-zinc-500">{r.k}</span>
              <span className="text-zinc-200">{r.v}</span>
              {r.hint && <span className="ml-1.5 text-[11px] text-zinc-600">({r.hint})</span>}
            </div>
          ))}
        </div>
      </section>

      {/* lineage summary — the CTE-DAG shape (honest counts, never a fabricated graph) */}
      <section>
        <div className="mb-1.5 text-xs uppercase tracking-wide text-zinc-500">lineage</div>
        <div data-testid="lineage-summary" className="flex flex-wrap items-center gap-4 rounded-lg border border-zinc-800 bg-zinc-900/60 px-3 py-2 font-mono text-xs text-zinc-300">
          <span>
            <span className="text-zinc-500">CTE nodes</span> {model.dag.nodes.length}
          </span>
          <span>
            <span className="text-zinc-500">edges</span> {model.dag.edges.length}
          </span>
          {model.is_incremental != null && (
            <span>
              <span className="text-zinc-500">incremental</span> {model.is_incremental ? "yes" : "no"}
            </span>
          )}
        </div>
      </section>

      {/* documented columns / contract */}
      <section>
        <div className="mb-1.5 text-xs uppercase tracking-wide text-zinc-500">columns · {cols.length}</div>
        {cols.length === 0 ? (
          <div data-testid="columns-empty" className="rounded-lg border border-dashed border-zinc-800 px-3 py-2 font-mono text-xs text-zinc-500">
            no columns documented{model.model_yaml?.path ? " in " + model.model_yaml.path : ""}
          </div>
        ) : (
          <div data-testid="columns-table" className="overflow-x-auto rounded-lg border border-zinc-800 bg-zinc-900/60">
            <table className="w-full font-mono text-xs">
              <thead>
                <tr className="border-b border-zinc-800 text-left uppercase tracking-wide text-zinc-500">
                  <th className="px-3 py-2 font-medium">column</th>
                  <th className="px-3 py-2 font-medium">type</th>
                  <th className="px-3 py-2 font-medium">tests</th>
                  <th className="px-3 py-2 font-medium">description</th>
                </tr>
              </thead>
              <tbody>
                {cols.map((c) => (
                  <tr key={c.name} data-testid="column-row" className="border-b border-zinc-800/70 last:border-0">
                    <td className="px-3 py-1.5 font-semibold text-zinc-100">{c.name}</td>
                    <td className="px-3 py-1.5 text-zinc-400">{c.type ?? <span className="text-zinc-600">—</span>}</td>
                    <td className="px-3 py-1.5">
                      <div className="flex flex-wrap gap-1">
                        {c.tests.length ? (
                          c.tests.map((t, i) => (
                            <span key={i} className="rounded bg-zinc-800 px-1.5 py-0.5 text-[10px] text-zinc-400">
                              {t}
                            </span>
                          ))
                        ) : (
                          <span className="text-zinc-600">—</span>
                        )}
                      </div>
                    </td>
                    <td className="px-3 py-1.5 text-zinc-400">{c.desc ?? <span className="text-zinc-600">—</span>}</td>
                  </tr>
                ))}
              </tbody>
            </table>
          </div>
        )}
      </section>
    </div>
  );
}
