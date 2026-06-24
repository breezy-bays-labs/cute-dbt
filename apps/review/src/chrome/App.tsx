// The cute-dbt review app SHELL (S2). Composes the dispatch-state spine:
// Header + SubHeader chrome + the ViewRouter (Entity×View matrix) + the
// registry-fed Footer, mounts the SINGLE keydown dispatcher (useKeydown), drives
// the theme/style/density data-* attrs onto the document root, and pushes nav
// history on every (entity, view, sel) change. The store is the SSOT; this shell
// is the only place that subscribes to it (the view layer reads via props).
//
// LAYER: chrome (composes view + data + domain). main.tsx mounts it.
import React, { useEffect, useMemo, useState } from "react";
import { Toaster } from "sonner";
import { loadFixture } from "../data/fixtures";
import { useAppStore } from "../data/store";
import { useKeydown } from "../data/use-keydown";
import { deriveView } from "../data/nav-slice";
import { buildContexts } from "../domain/reshape";
import { shikiName, ensureHighlighter, type AppTheme } from "../domain/highlighter";
import type { ContextData } from "../domain/context-data";
import { ENTITY_NOUN } from "../domain/matrix";
import type { Entity } from "../domain/keymap";
import { Header } from "../view/Header";
import { SubHeader } from "../view/SubHeader";
import { ViewRouter } from "../view/ViewRouter";
import { Footer } from "../view/Footer";
import type { KbContext } from "../domain/keymap";
import type { View } from "../domain/matrix";

/** Map a ModelState chip to the Badge tone vocabulary (added/modified/removed). */
function changeTone(state?: string): "added" | "modified" | "removed" | null {
  if (state === "added" || state === "new") return "added";
  if (state === "modified") return "modified";
  if (state === "deleted") return "removed";
  return null;
}

export function App({ initialTheme = "tokyo" }: { initialTheme?: AppTheme }): React.ReactElement {
  const context = useMemo(() => loadFixture("context.440") as unknown as ContextData, []);
  const contexts = useMemo(() => buildContexts(context), [context]);

  // ── store subscriptions (the chrome owns them) ───────────────────────────
  const entity = useAppStore((s) => s.entity);
  const viewMap = useAppStore((s) => s.viewMap);
  const sel = useAppStore((s) => s.sel);
  const overlays = useAppStore((s) => s.overlays);
  const settings = useAppStore((s) => s.settings);
  const keymapOverride = useAppStore((s) => s.keymapOverride);
  const setEntity = useAppStore((s) => s.setEntity);
  const setView = useAppStore((s) => s.setView);
  const setSel = useAppStore((s) => s.setSel);
  const toggleOverlay = useAppStore((s) => s.toggleOverlay);
  const openOverlay = useAppStore((s) => s.openOverlay);
  const setSetting = useAppStore((s) => s.setSetting);
  const historyBack = useAppStore((s) => s.historyBack);
  const historyForward = useAppStore((s) => s.historyForward);
  const pushHistory = useAppStore((s) => s.pushHistory);

  const view = deriveView(viewMap, entity);

  // ── the SINGLE keydown dispatcher ────────────────────────────────────────
  useKeydown();

  // ── per-surface ui local state (lands in slices in later slices) ──────────
  const [codeMode, setCodeMode] = useState<"diff" | "file">("diff");
  const [dataMode, setDataMode] = useState<"diff" | "file">("diff");

  const [themeError, setThemeError] = useState<string | null>(null);

  // Seed the theme from the URL hook (?theme=) on first mount if the store is fresh.
  useEffect(() => {
    if (initialTheme !== "tokyo" && settings.theme === "tokyo") setSetting("theme", initialTheme);
  }, []);

  // ── drive theme/style/density onto the document root (data-* attrs) ───────
  useEffect(() => {
    const el = document.documentElement;
    el.setAttribute("data-theme", settings.theme);
    el.setAttribute("data-style", settings.style);
    el.setAttribute("data-density", settings.density);
    if (settings.accent && settings.accent !== "theme") el.setAttribute("data-accent", settings.accent);
    else el.removeAttribute("data-accent");
  }, [settings.theme, settings.style, settings.density, settings.accent]);

  // ── push nav history on every (entity, view, sel) change ──────────────────
  useEffect(() => {
    pushHistory();
  }, [entity, view, JSON.stringify(sel)]);

  // ── resolve the active model + context (Models surfaces) ──────────────────
  const activeName =
    (entity === "models" && sel.models && context.models.some((m) => m.name === sel.models)
      ? sel.models
      : null) ??
    context.models[0]?.name ??
    null;
  const model = context.models.find((m) => m.name === activeName) ?? context.models[0];
  const ctx = contexts.find((c) => c.name === activeName) ?? contexts[0];
  const shiki = shikiName(settings.theme as AppTheme);

  // ── instance list per entity (S2: models from the fixture; others stubbed) ─
  const modelNames = useMemo(() => context.models.map((m) => m.name), [context]);
  const instances: readonly string[] = entity === "models" ? modelNames : [];

  const compiledSql = useMemo(() => {
    if (!model) return "";
    return model.dag.nodes
      .map((n) => model.compiled_sql[n.id])
      .filter((s): s is string => typeof s === "string")
      .join("\n\n");
  }, [model]);

  // ── footer context (registry-fed; the flow chips degrade honestly) ────────
  const footerCtx: KbContext = {
    entity,
    view,
    viewCount: undefined,
    noun: ENTITY_NOUN[entity],
  };

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

  const change = entity === "models" ? changeTone(model?.state) : null;
  const detailOpen = overlays.shelf;

  return (
    <div className="flex min-h-screen flex-col bg-zinc-950 text-zinc-200" style={{ fontFamily: "system-ui, sans-serif" }}>
      <Toaster theme="dark" />

      <Header
        entity={entity}
        onEntity={setEntity}
        onBack={historyBack}
        onForward={historyForward}
        since={settings.dataSource === "pr440-since"}
        onSince={(v) => setSetting("dataSource", v ? "pr440-since" : "pr440")}
        reviewOpen={overlays.review}
        onReview={() => openOverlay("review")}
        sidebarOpen={overlays.sidebar}
        onSidebar={() => toggleOverlay("sidebar")}
        onSettings={() => toggleOverlay("settings")}
        progressLabel={`✓ 0/${modelNames.length}`}
        keymapOverride={keymapOverride}
        showElse={settings.project}
      />

      <SubHeader
        entity={entity}
        view={view}
        onView={(v: View) => setView(v)}
        noun={ENTITY_NOUN[entity]}
        sel={sel[entity]}
        instances={instances}
        onSel={(id) => setSel(id)}
        change={change}
        codeMode={codeMode}
        onCodeMode={setCodeMode}
        dataMode={dataMode}
        onDataMode={setDataMode}
        detailOpen={detailOpen}
        onDetail={() => toggleOverlay("shelf")}
      />

      {themeError && (
        <div
          data-testid="theme-error-banner"
          className="m-4 rounded border border-rose-500 bg-rose-950/40 px-4 py-2 text-sm text-rose-300"
        >
          Highlighter failed for theme "{settings.theme}" ({shiki}): {themeError}
        </div>
      )}

      <div className="flex min-h-0 flex-1">
        {entity === "models" && (
          <aside
            data-testid="model-list"
            className="h-full w-64 shrink-0 overflow-auto border-r border-zinc-800 p-3"
          >
            <div className="mb-2 text-xs uppercase tracking-wide text-zinc-500">Models ({modelNames.length})</div>
            <ul>
              {context.models.map((m) => (
                <li key={m.name}>
                  <button
                    data-testid="model-list-item"
                    data-model={m.name}
                    data-selected={m.name === activeName}
                    onClick={() => setSel(m.name, "models" as Entity)}
                    className={
                      "w-full truncate rounded px-2 py-1 text-left text-sm " +
                      (m.name === activeName ? "bg-sky-500/20 text-sky-200" : "text-zinc-300 hover:bg-zinc-800")
                    }
                    title={m.path ?? m.name}
                  >
                    {m.name}
                  </button>
                </li>
              ))}
            </ul>
          </aside>
        )}

        <main className="flex min-w-0 flex-1 flex-col overflow-hidden">
          <ViewRouter
            entity={entity}
            view={view}
            model={model}
            ctx={ctx}
            compiledSql={compiledSql}
            shiki={shiki}
            sel={sel[entity]}
          />
        </main>
      </div>

      <Footer ctx={footerCtx} keymapOverride={keymapOverride} />
    </div>
  );
}
