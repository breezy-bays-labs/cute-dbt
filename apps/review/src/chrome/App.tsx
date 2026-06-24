// The cute-dbt review app shell (S0 walking skeleton).
//
// Proves the FULL load-bearing stack wires through the bundle on a trivial-but-
// real surface: import context.440.json → Zod-validate → render static entity
// tabs + a model list from models[] + (for one selected model) ONE Pierre
// DiffViewer + ONE @xyflow/react LineageGraph laid out by the elkjs WORKER + ONE
// Shiki CodePane + Tailwind chrome + Zustand persist. Loud-fail on theme
// fallback. No parity chasing — the feature surfaces land in later slices.
import React, { useEffect, useMemo, useState } from "react";
import { Toaster } from "sonner";
import { GitPullRequest, Boxes, FlaskConical } from "lucide-react";
import { loadFixture } from "../data/fixtures";
import { useAppStore } from "../data/store";
import { buildContexts, type ReviewContext } from "../domain/reshape";
import { APP_THEMES, shikiName, ensureHighlighter, type AppTheme } from "../domain/highlighter";
import type { ContextData } from "../domain/context-data";
import { DiffViewer } from "../view/DiffViewer";
import { LineageGraph } from "../view/LineageGraph";
import { CodePane } from "../view/CodePane";

// Static entity tabs (S0 surface — only Models is wired; the rest are skeleton
// tabs proving the chrome, populated in later slices).
const ENTITY_TABS = [
  { id: "models", label: "Models", icon: Boxes, active: true },
  { id: "pr", label: "PR", icon: GitPullRequest, active: false },
  { id: "tests", label: "Tests", icon: FlaskConical, active: false },
] as const;

export function App({ initialTheme = "tokyo" }: { initialTheme?: AppTheme }): React.ReactElement {
  // The validated context (Zod gate runs in loadFixture). context.440 = the
  // 16-model PR-440 spine.
  const context = useMemo(() => loadFixture("context.440") as unknown as ContextData, []);
  const contexts = useMemo(() => buildContexts(context), [context]);

  const theme = useAppStore((s) => s.theme);
  const setTheme = useAppStore((s) => s.setTheme);
  const selectedModel = useAppStore((s) => s.selectedModel);
  const setSelectedModel = useAppStore((s) => s.setSelectedModel);

  const [themeError, setThemeError] = useState<string | null>(null);

  // Seed the theme from the URL hook (?theme=) on first mount if the store is fresh.
  useEffect(() => {
    if (initialTheme !== "tokyo" && theme === "tokyo") setTheme(initialTheme);
  }, []);

  // Resolve selection: persisted model if still present, else the first model.
  const activeName =
    (selectedModel && context.models.some((m) => m.name === selectedModel) ? selectedModel : null) ??
    context.models[0]?.name ??
    null;

  const model = context.models.find((m) => m.name === activeName) ?? context.models[0];
  const ctx: ReviewContext | undefined =
    contexts.find((c) => c.name === activeName) ?? contexts[0];
  const shiki = shikiName(theme);

  // Compiled SQL joined in DAG order for the Shiki pane.
  const compiledSql = useMemo(() => {
    if (!model) return "";
    return model.dag.nodes
      .map((n) => model.compiled_sql[n.id])
      .filter((s): s is string => typeof s === "string")
      .join("\n\n");
  }, [model]);

  // Preload the active shiki theme before render; loud-fail on an unregistered one.
  useEffect(() => {
    let cancelled = false;
    ensureHighlighter([shiki])
      .then(() => {
        if (!cancelled) setThemeError(null);
      })
      .catch((err) => {
        if (cancelled) return;
        const msg = err instanceof Error ? err.message : String(err);
        setThemeError(msg);
        document.body.setAttribute("data-highlighter-error", msg);
      });
    return () => {
      cancelled = true;
    };
  }, [shiki]);

  return (
    <div className="min-h-screen bg-zinc-950 text-zinc-200" style={{ fontFamily: "system-ui, sans-serif" }}>
      <Toaster theme="dark" />
      {/* ── Header chrome (Tailwind v4) ── */}
      <header className="flex flex-wrap items-center gap-3 border-b border-zinc-800 px-6 py-3">
        <strong className="text-base" data-testid="app-title">
          cute-dbt · review
        </strong>
        {context.pr_ref && (
          <span data-testid="pr-ref" className="text-xs text-zinc-400">
            PR #{context.pr_ref.number}
          </span>
        )}
        <span className="flex-1" />
        <label className="text-sm text-zinc-400">
          theme{" "}
          <select
            data-testid="theme-select"
            className="rounded bg-zinc-800 px-2 py-1 text-zinc-100"
            value={theme}
            onChange={(e) => setTheme(e.target.value as AppTheme)}
          >
            {APP_THEMES.map((t) => (
              <option key={t} value={t}>
                {t}
              </option>
            ))}
          </select>
        </label>
      </header>

      {/* ── Entity tabs ── */}
      <nav data-testid="entity-tabs" className="flex gap-1 border-b border-zinc-800 px-6">
        {ENTITY_TABS.map((tab) => {
          const Icon = tab.icon;
          return (
            <button
              key={tab.id}
              data-testid={`entity-tab-${tab.id}`}
              data-active={tab.active}
              disabled={!tab.active}
              className={
                "flex items-center gap-1.5 border-b-2 px-3 py-2 text-sm " +
                (tab.active
                  ? "border-sky-400 text-sky-300"
                  : "border-transparent text-zinc-500")
              }
            >
              <Icon size={14} />
              {tab.label}
            </button>
          );
        })}
      </nav>

      {themeError && (
        <div
          data-testid="theme-error-banner"
          className="m-4 rounded border border-rose-500 bg-rose-950/40 px-4 py-2 text-sm text-rose-300"
        >
          Highlighter failed for theme "{theme}" ({shiki}): {themeError}
        </div>
      )}

      <div className="flex">
        {/* ── Model list (from models[]) ── */}
        <aside
          data-testid="model-list"
          className="w-64 shrink-0 border-r border-zinc-800 p-3"
          style={{ maxHeight: "calc(100vh - 96px)", overflow: "auto" }}
        >
          <div className="mb-2 text-xs uppercase tracking-wide text-zinc-500">
            Models ({context.models.length})
          </div>
          <ul>
            {context.models.map((m) => (
              <li key={m.name}>
                <button
                  data-testid="model-list-item"
                  data-model={m.name}
                  data-selected={m.name === activeName}
                  onClick={() => setSelectedModel(m.name)}
                  className={
                    "w-full truncate rounded px-2 py-1 text-left text-sm " +
                    (m.name === activeName
                      ? "bg-sky-500/20 text-sky-200"
                      : "text-zinc-300 hover:bg-zinc-800")
                  }
                  title={m.path ?? m.name}
                >
                  {m.name}
                </button>
              </li>
            ))}
          </ul>
        </aside>

        {/* ── The selected model's review surface ── */}
        <main className="min-w-0 flex-1 space-y-5 p-6">
          <h2 className="text-sm font-semibold" data-testid="model-heading">
            {model?.name}
            <span className="ml-2 font-normal text-zinc-500">
              ({ctx?.files.length ?? 0} changed files)
            </span>
          </h2>

          {/* ONE Pierre DiffViewer (first changed file). */}
          <section data-testid="diff-section" className="rounded-lg border border-zinc-800 bg-zinc-900/60 p-4">
            <div className="mb-2 text-xs uppercase tracking-wide text-zinc-500">Diff (Pierre)</div>
            {ctx && ctx.files[0] ? (
              <DiffViewer file={ctx.files[0]} shiki={shiki} />
            ) : (
              <p data-testid="no-diff" className="text-sm text-zinc-500">
                No changed files for this model.
              </p>
            )}
          </section>

          {/* ONE @xyflow/react LineageGraph laid out by the elkjs worker. */}
          {model?.dag && (
            <section data-testid="lineage-section" className="rounded-lg border border-zinc-800 bg-zinc-900/60 p-4">
              <div className="mb-2 text-xs uppercase tracking-wide text-zinc-500">
                CTE lineage (React Flow + elkjs worker)
              </div>
              <LineageGraph dag={model.dag} />
            </section>
          )}

          {/* ONE Shiki code pane (compiled SQL). */}
          {compiledSql && (
            <section data-testid="code-section" className="rounded-lg border border-zinc-800 bg-zinc-900/60 p-4">
              <div className="mb-2 text-xs uppercase tracking-wide text-zinc-500">Compiled SQL (Shiki)</div>
              <CodePane code={compiledSql} lang="sql" shiki={shiki} />
            </section>
          )}
        </main>
      </div>
    </div>
  );
}
