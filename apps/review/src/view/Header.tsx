// The Header chrome (view layer): back/fwd · entity Tabs (1-5) · Since/Full
// Segmented · a review-progress chip placeholder · Review/Sidebar/Settings
// buttons. Ported from the prototype's top <header>. It reads + drives the store
// through props the App root threads (the view layer never subscribes to the
// store directly — the chrome owns the subscription, S0 posture).
//
// LAYER: view (may import domain + data; never chrome).
import React from "react";
import { ChevronLeft, ChevronRight, PanelRight, Settings } from "lucide-react";
import { Tabs, Segmented, ChromeButton, Kbd } from "./chrome-kit";
import { ENTITY_ORDER, ENTITY_LABEL } from "../domain/matrix";
import type { Entity } from "../domain/keymap";
import { displayKey, mergeKeymap, type Keymap } from "../domain/keymap";

export interface HeaderProps {
  entity: Entity;
  onEntity: (e: Entity) => void;
  onBack: () => void;
  onForward: () => void;
  since: boolean;
  onSince: (v: boolean) => void;
  reviewOpen: boolean;
  onReview: () => void;
  sidebarOpen: boolean;
  onSidebar: () => void;
  onSettings: () => void;
  /** review-progress chip text (placeholder until the review slice — V1). */
  progressLabel: string;
  keymapOverride?: Keymap | null;
  /** experiment gate — hide the Else entity when project=false. */
  showElse: boolean;
}

export function Header(p: HeaderProps): React.ReactElement {
  const km = mergeKeymap(p.keymapOverride);
  const entityTabs = ENTITY_ORDER.filter((e) => p.showElse || e !== "else").map((e) => ({
    value: e,
    label: ENTITY_LABEL[e],
    hot: displayKey(km[`entity-${e}`]),
  }));
  return (
    <header
      data-testid="header"
      className="flex h-12 shrink-0 items-center gap-3 border-b border-zinc-800 bg-zinc-950 px-4"
    >
      {/* back / forward */}
      <div className="flex shrink-0 items-center gap-0.5">
        <button
          data-testid="nav-back"
          onClick={p.onBack}
          title="Back (⌥←)"
          className="flex h-7 items-center gap-1 rounded-md px-1.5 text-zinc-400 hover:bg-zinc-800 hover:text-zinc-100"
        >
          <ChevronLeft size={16} />
          <Kbd>⌥←</Kbd>
        </button>
        <button
          data-testid="nav-forward"
          onClick={p.onForward}
          title="Forward (⌥→)"
          className="flex h-7 items-center gap-1 rounded-md px-1.5 text-zinc-400 hover:bg-zinc-800 hover:text-zinc-100"
        >
          <Kbd>⌥→</Kbd>
          <ChevronRight size={16} />
        </button>
      </div>

      <div className="h-5 w-px bg-zinc-800" />

      <strong className="text-sm" data-testid="app-title">
        cute-dbt · review
      </strong>

      {/* entity tabs (1-5) */}
      <Tabs testid="entity-tabs" options={entityTabs} value={p.entity} onChange={(v) => p.onEntity(v as Entity)} size="sm" />

      <span className="flex-1" />

      {/* Since / Full scope */}
      <Segmented
        testid="scope-segmented"
        options={[
          { value: "full", label: "Full PR" },
          { value: "since", label: "Since review" },
        ]}
        value={p.since ? "since" : "full"}
        onChange={(v) => p.onSince(v === "since")}
      />

      {/* review-progress chip (placeholder — review slice lands the real count, V1) */}
      <span data-testid="review-progress" className="font-mono text-[11px] text-zinc-500">
        {p.progressLabel}
      </span>

      <ChromeButton testid="btn-review" active={p.reviewOpen} onClick={p.onReview} title="Write review (w)">
        <Kbd>w</Kbd> Review
      </ChromeButton>
      <ChromeButton testid="btn-sidebar" active={p.sidebarOpen} onClick={p.onSidebar} title="Review checklist (s)">
        <PanelRight size={14} /> <Kbd>s</Kbd>
      </ChromeButton>
      <ChromeButton testid="btn-settings" onClick={p.onSettings} title="Settings (,)">
        <Settings size={14} />
      </ChromeButton>
    </header>
  );
}
