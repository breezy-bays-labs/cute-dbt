// The SubHeader chrome (view layer): view Tabs (⇧digit positional) · instance
// Select + idx/total + change Badge · code/data-mode Segmented · Detail toggle.
// Ported from the prototype's second <header> bar. The ⇧digit view hints derive
// PURELY from AVAIL position (matrix.viewKeyFor) — the single source of truth.
//
// LAYER: view (may import domain + data; never chrome).
import React from "react";
import { Tabs, Segmented, Select, Badge, ChromeButton, Kbd } from "./chrome-kit";
import { viewsFor, viewKeyFor, viewLabelFor, type View } from "../domain/matrix";
import type { Entity } from "../domain/keymap";

export interface SubHeaderProps {
  entity: Entity;
  view: View;
  onView: (v: View) => void;
  noun: string;
  /** the selected instance (null for PR). */
  sel: string | null;
  instances: readonly string[];
  onSel: (id: string) => void;
  /** change tone for the instance badge (null = no badge). */
  change: "added" | "modified" | "removed" | null;
  /** code-mode segmented (only on the Code view). */
  codeMode: "diff" | "file";
  onCodeMode: (m: "diff" | "file") => void;
  /** data-mode segmented (only on the Data view of models/seeds). */
  dataMode: "diff" | "file";
  onDataMode: (m: "diff" | "file") => void;
  /** the detail/shelf toggle state. */
  detailOpen: boolean;
  onDetail: () => void;
}

export function SubHeader(p: SubHeaderProps): React.ReactElement {
  const views = viewsFor(p.entity);
  const viewTabs = views.map((v) => ({
    value: v,
    label: viewLabelFor(p.entity, v),
    hot: viewKeyFor(p.entity, v),
  }));
  // Resolve the displayed selection FIRST: when `p.sel` is null the Select shows
  // the first instance, so the idx indicator must count THAT (not -1 → a "0/16"
  // that contradicts the dropdown showing item 1 selected).
  const activeSel = p.sel ?? p.instances[0] ?? null;
  const idx = activeSel ? p.instances.indexOf(activeSel) : -1;
  const showInstance = p.entity !== "pr" && p.instances.length > 0;
  const showCodeMode = p.entity === "models" && p.view === "code";
  const showDataMode = p.view === "data" && (p.entity === "models" || p.entity === "seeds");
  const showDetail =
    (p.view === "topology" && (p.entity === "models" || p.entity === "macros" || p.entity === "seeds")) ||
    (p.entity === "models" && (p.view === "node" || p.view === "data"));

  return (
    <div
      data-testid="subheader"
      className="flex h-11 shrink-0 items-center gap-3 border-b border-zinc-800 bg-zinc-950 px-4"
    >
      {viewTabs.length > 1 ? (
        <Tabs testid="view-tabs" options={viewTabs} value={p.view} onChange={(v) => p.onView(v as View)} />
      ) : null}

      {showInstance ? (
        <>
          {viewTabs.length > 1 ? <div className="h-5 w-px bg-zinc-800" /> : null}
          <span className="text-xs uppercase tracking-wide text-zinc-500">{p.noun}</span>
          <span data-testid="instance-picker" className="inline-flex items-center gap-2">
            <Select
              testid="instance-select"
              value={activeSel ?? ""}
              onChange={p.onSel}
              options={p.instances}
              ariaLabel={`Select ${p.noun}`}
            />
            <span data-testid="instance-idx" className="font-mono text-[11px] text-zinc-500">
              {idx + 1}/{p.instances.length}
            </span>
          </span>
          {p.change ? <Badge tone={p.change}>{p.change}</Badge> : null}
        </>
      ) : null}

      {showCodeMode ? (
        <Segmented
          testid="code-mode"
          options={[
            { value: "diff", label: "Diff" },
            { value: "file", label: "File" },
          ]}
          value={p.codeMode}
          onChange={(v) => p.onCodeMode(v as "diff" | "file")}
        />
      ) : null}
      {showDataMode ? (
        <Segmented
          testid="data-mode"
          options={[
            { value: "diff", label: "Diff" },
            { value: "file", label: "File" },
          ]}
          value={p.dataMode}
          onChange={(v) => p.onDataMode(v as "diff" | "file")}
        />
      ) : null}

      <span className="flex-1" />

      {showDetail ? (
        <ChromeButton testid="btn-detail" active={p.detailOpen} onClick={p.onDetail}>
          Detail <Kbd>v</Kbd>
        </ChromeButton>
      ) : null}
    </div>
  );
}
