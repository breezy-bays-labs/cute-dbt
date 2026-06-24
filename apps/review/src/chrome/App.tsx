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
import { buildContexts, mentionCandidates } from "../domain/reshape";
import { buildDataset, type ScopeAxis } from "../domain/data/dataset";
import { buildPrOverview, buildPrFiles, buildCommentTimeline, prTimelineFeed } from "../domain/pr-page";
import { shikiName, ensureHighlighter, type AppTheme } from "../domain/highlighter";
import type { ContextData } from "../domain/context-data";
import { ENTITY_NOUN } from "../domain/matrix";
import type { Entity } from "../domain/keymap";
import { progressOf, type Verdict } from "../domain/review/review-machine";
import { Header } from "../view/Header";
import { SubHeader } from "../view/SubHeader";
import { ViewRouter } from "../view/ViewRouter";
import { Footer } from "../view/Footer";
import { WriteReview } from "../view/review/WriteReview";
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
  const dataset = useMemo(() => buildDataset(context), [context]);
  // ── S9 PR-page aggregation (pure folds; rebuilt only when the context changes) ─
  const prOverview = useMemo(() => buildPrOverview(context), [context]);
  const prFiles = useMemo(() => buildPrFiles(context), [context]);
  const prTimeline = useMemo(() => buildCommentTimeline(context), [context]);
  const prFeed = useMemo(() => prTimelineFeed(context), [context]);

  // ── store subscriptions (the chrome owns them) ───────────────────────────
  const entity = useAppStore((s) => s.entity);
  const viewMap = useAppStore((s) => s.viewMap);
  const sel = useAppStore((s) => s.sel);
  const overlays = useAppStore((s) => s.overlays);
  const settings = useAppStore((s) => s.settings);
  const keymapOverride = useAppStore((s) => s.keymapOverride);
  const prNode = useAppStore((s) => s.prNode);
  const setEntity = useAppStore((s) => s.setEntity);
  const setView = useAppStore((s) => s.setView);
  const setSel = useAppStore((s) => s.setSel);
  const setPrNode = useAppStore((s) => s.setPrNode);
  const toggleOverlay = useAppStore((s) => s.toggleOverlay);
  const openOverlay = useAppStore((s) => s.openOverlay);
  const setSetting = useAppStore((s) => s.setSetting);
  const historyBack = useAppStore((s) => s.historyBack);
  const historyForward = useAppStore((s) => s.historyForward);
  const pushHistory = useAppStore((s) => s.pushHistory);
  // ── V1 review-flow store subscriptions ────────────────────────────────────
  const review = useAppStore((s) => s.review);
  const codeMode = useAppStore((s) => s.codeMode);
  const setCodeMode = useAppStore((s) => s.setCodeMode);
  const addReviewDraft = useAppStore((s) => s.addReviewDraft);
  const markReviewedAdvance = useAppStore((s) => s.markReviewedAdvance);
  const publishReviewAction = useAppStore((s) => s.publishReview);
  const buildPayload = useAppStore((s) => s.buildPayload);
  const setSel2 = useAppStore((s) => s.setSel);
  const setView2 = useAppStore((s) => s.setView);
  const closeOverlay = useAppStore((s) => s.closeOverlay);

  const view = deriveView(viewMap, entity);

  // ── the SINGLE keydown dispatcher ────────────────────────────────────────
  useKeydown();

  // ── per-surface ui local state (lands in slices in later slices) ──────────
  const [dataMode, setDataMode] = useState<"diff" | "file">("diff");
  // the PR-scope change-axis (single-select; lands in a slice in a later slice).
  const [scopeAxis, setScopeAxis] = useState<ScopeAxis>("all");

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

  // ── Esc closes the write-review overlay (the FIXED_KEYS `esc → close any
  //    overlay` contract). The single dispatcher's modal gate suppresses every
  //    OTHER key while the overlay owns the keyboard, but Esc must still dismiss
  //    it — a capture-phase Esc handler that runs only while the overlay is open.
  useEffect(() => {
    if (!overlays.review) return;
    const onEsc = (e: KeyboardEvent): void => {
      if (e.key === "Escape") {
        e.preventDefault();
        closeOverlay("review");
      }
    };
    window.addEventListener("keydown", onEsc, { capture: true });
    return () => window.removeEventListener("keydown", onEsc, { capture: true });
  }, [overlays.review, closeOverlay]);

  // ── the in-scope MODEL set — the SINGLE source the sidebar, the SubHeader
  //    instance list, activeName, the chip total, AND the keyboard loop share
  //    (cute-dbt#495). prSelectableModels has dropped the seed/macro ids
  //    prSelectable carries; the review loop walks models only, so the sidebar +
  //    the loop + the chip can never disagree about which set is reviewable. ────
  const reviewScopeModels = dataset.prSelectableModels;
  const scopeModels = useMemo(
    () => reviewScopeModels.map((n) => context.models.find((m) => m.name === n)).filter((m): m is NonNullable<typeof m> => !!m),
    [reviewScopeModels, context],
  );

  // ── resolve the active model + context (Models surfaces) ──────────────────
  // activeName is valid only when sel.models is IN SCOPE (a reviewable model);
  // a stale/out-of-scope sel falls back to the first in-scope model — never a
  // non-model (the seed/macro sel that drove the wrong-diff bug).
  const activeName =
    (entity === "models" && sel.models && reviewScopeModels.includes(sel.models)
      ? sel.models
      : null) ??
    scopeModels[0]?.name ??
    context.models[0]?.name ??
    null;
  const model = context.models.find((m) => m.name === activeName) ?? context.models[0];
  const ctx = contexts.find((c) => c.name === activeName) ?? contexts[0];
  const shiki = shikiName(settings.theme as AppTheme);

  // ── instance list per entity (S2: the in-scope models; others stubbed) ─────
  const instances: readonly string[] = entity === "models" ? reviewScopeModels : [];

  // ── PR reviewers (the comment composer's @-mention picker source) ──────────
  // Login STRINGS only (author + reviewer logins, deduped) — the picker calls
  // `.toLowerCase()` on each, so a non-string candidate would throw.
  const reviewers = useMemo(() => mentionCandidates(context), [context]);

  const compiledSql = useMemo(() => {
    if (!model) return "";
    return model.dag.nodes
      .map((n) => model.compiled_sql[n.id])
      .filter((s): s is string => typeof s === "string")
      .join("\n\n");
  }, [model]);

  // ── V1 review-flow derivations (the REAL header chip + per-model state) ────
  // the open-thread count: live (non-outdated, line-anchored) threads across the
  // in-scope models, MINUS the ones the reviewer resolved this session. REAL —
  // computed from the dataset, never fabricated.
  const openThreads = useMemo(() => {
    let n = 0;
    for (const m of reviewScopeModels) {
      const rec = dataset.D[m];
      if (!rec) continue;
      for (const c of rec.comments) {
        if (c.line == null) continue;
        const resolvedHere = review.resolved[`${m}@${c.line}`] === true;
        if (!c.threadResolved && !resolvedHere) n++;
      }
    }
    return n;
  }, [dataset, reviewScopeModels, review.resolved]);
  const progress = useMemo(
    () => progressOf(review, reviewScopeModels, openThreads),
    [review, reviewScopeModels, openThreads],
  );
  // the active model's REAL reviewed state + pending-draft count.
  const activeReviewed = activeName != null && review.reviewed[activeName] === true;
  const activeDraftCount = activeName != null ? (review.pending[activeName]?.length ?? 0) : 0;
  const hasUnreviewed = progress.total > 0 && progress.reviewed < progress.total;

  // ── footer context (registry-fed; the flow chips read the REAL flow signals) ─
  const footerCtx: KbContext = {
    entity,
    view,
    viewCount: undefined,
    noun: ENTITY_NOUN[entity],
    codeMode,
    // the flow signals — fed REAL now (V1): the flow is mounted on Models, there
    // are unreviewed models, and a thread is focusable. The footer chips light up
    // honestly instead of degrading to inactive.
    inReview: entity === "models",
    hasUnreviewed,
    hasOpenThread: openThreads > 0,
  };

  // ── the review-flow callbacks the ViewRouter + WriteReview consume ─────────
  const onMarkReviewed = (): void => {
    if (activeName == null) return;
    const next = markReviewedAdvance(reviewScopeModels, activeName);
    if (next != null) {
      setSel2(next, "models");
      setView2("code");
      setCodeMode("diff");
    }
  };
  const onPublishReview = (verdict: Verdict, body: string): void => {
    publishReviewAction(verdict, body);
    closeOverlay("review");
  };
  // the (owner/repo, pr#) the portable payload targets — parsed from the PR url
  // in the context (honest "owner/repo" placeholder when the context lacks one).
  const repoSlug = useMemo(() => {
    const url = String(dataset.SCOPE.prUrl || "");
    const m = url.match(/github\.com\/([^/]+\/[^/]+)\/pull\/\d+/);
    return m ? m[1]! : "owner/repo";
  }, [dataset]);
  const prNumber = useMemo(() => Number(dataset.SCOPE.prNumber) || 0, [dataset]);

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
        progressLabel={`✓ ${progress.reviewed}/${progress.total}${openThreads > 0 ? ` · ${openThreads} open` : ""}`}
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
            <div className="mb-2 text-xs uppercase tracking-wide text-zinc-500">Models ({scopeModels.length})</div>
            <ul>
              {scopeModels.map((m) => (
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
            reviewers={reviewers}
            sel={sel[entity]}
            prScopeByAxis={dataset.prScopeByAxis}
            scopeAxis={scopeAxis}
            onScopeAxis={setScopeAxis}
            prNode={prNode}
            onPrNode={setPrNode}
            onOpenNode={(id, kind) => {
              // route OUT BY KIND: a seed/macro PR node jumps into its MATCHING
              // entity (seed → Seeds, macro → Macros) and selects the clicked id
              // there. NEVER misroute a non-model id onto Models with a bogus sel
              // (the false-navigation bug); reserve Models for actual models.
              // (Deleted nodes never reach this sink — they keep the PR cursor;
              // see routePrSelect.)
              const entity: Entity = kind === "seed" ? "seeds" : kind === "macro" ? "macros" : "models";
              setEntity(entity);
              setSel(id, entity);
            }}
            // ── V1 review-flow props (the Models reviewable surface) ─────────
            modelReviewed={activeReviewed}
            modelDraftCount={activeDraftCount}
            onDraft={(d) => {
              if (activeName != null) addReviewDraft(activeName, d);
            }}
            onMarkReviewed={onMarkReviewed}
            // ── S9 PR-page props (overview / files / comment timeline) ───────
            prOverview={prOverview}
            prFiles={prFiles}
            prTimeline={prTimeline}
            prFeed={prFeed}
            onOpenModel={(name) => {
              // a PR file / comment-thread row opens that model in the Models
              // review surface — jump to the Models entity, select the model, and
              // land on the code-review (diff) surface (the reviewable surface the
              // prototype's "open in code ↗" affordance targets).
              setEntity("models" as Entity);
              setSel(name, "models" as Entity);
              setView2("code");
              setCodeMode("diff");
            }}
          />
        </main>
      </div>

      {overlays.review && (
        <WriteReview
          draftCount={review.pending ? Object.values(review.pending).reduce((n, a) => n + (a?.length ?? 0), 0) : 0}
          onBuild={(verdict, body) => buildPayload({ verdict, body, repo: repoSlug, pr: prNumber })}
          onPublish={onPublishReview}
          onClose={() => closeOverlay("review")}
        />
      )}

      <Footer ctx={footerCtx} keymapOverride={keymapOverride} />
    </div>
  );
}
