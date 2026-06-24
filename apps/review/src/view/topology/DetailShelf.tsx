// DetailShelf — the FIRST-PARTY resizable detail shelf (S6c). The prototype's
// `ui.js` Shelf + Segmented, ported to React WITHOUT shadcn/Radix (those are
// DEFERRED to S11, the design pass). First-party means: a pointer-drag AND
// keyboard-resizable handle (a `role="separator"` slider — accessible by
// construction), a first-party segmented control, a pinnable info panel, a
// shelf-mode segmented, and fullscreen + dock toggles, styled with Tailwind.
//
// It owns NO sync semantics — it is a pure container the TopologyPanes composes
// (the cursor-sync machine is consumed by the panes, never by the shelf). The
// resize size persists under the same versioned `cute-dbt:` localStorage keys as
// the prototype (`cute-dbt:shelfSize:side` / `:bottom`), fail-closed on read.
//
// LAYER: view (a presentational container; imports React + the icons + the
// first-party Segmented only). No domain, no chrome.
import React, { useCallback, useEffect, useRef, useState } from "react";

// ── the shelf-mode vocabulary (the source toggle the DAG follows) ────────────
export type ShelfMode = "diff" | "file" | "compiled";
export type ShelfDock = "side" | "bottom";

// ── Segmented — a first-party accessible segmented control (no shadcn) ────────
export interface SegmentedOption<V extends string> {
  value: V;
  label: string;
  disabled?: boolean;
  /** an extra data-testid on this option's button (legacy selector compat). */
  testId?: string;
  /** extra data-* attributes on this option's button (e.g. `mode` → data-mode). */
  data?: Record<string, string>;
}
export interface SegmentedProps<V extends string> {
  value: V;
  onChange: (v: V) => void;
  options: SegmentedOption<V>[];
  ariaLabel?: string;
  className?: string;
  testId?: string;
}

/** A first-party segmented control rendered as an ARIA radiogroup: arrow-key /
 *  click selectable, the active option `aria-checked`. (Replaces the prototype's
 *  `Segmented`; shadcn's lands in S11.) */
export function Segmented<V extends string>({
  value,
  onChange,
  options,
  ariaLabel,
  className,
  testId,
}: SegmentedProps<V>): React.ReactElement {
  const move = (delta: number): void => {
    const idx = options.findIndex((o) => o.value === value);
    for (let i = 1; i <= options.length; i++) {
      const next = options[(idx + delta * i + options.length * i) % options.length];
      if (next && !next.disabled) {
        onChange(next.value);
        return;
      }
    }
  };
  const onKeyDown = (e: React.KeyboardEvent): void => {
    if (e.key === "ArrowRight" || e.key === "ArrowDown") { e.preventDefault(); move(1); }
    else if (e.key === "ArrowLeft" || e.key === "ArrowUp") { e.preventDefault(); move(-1); }
  };
  return (
    <div
      role="radiogroup"
      aria-label={ariaLabel}
      data-testid={testId}
      onKeyDown={onKeyDown}
      className={"inline-flex rounded-md border border-zinc-700 bg-zinc-900 p-0.5 " + (className ?? "")}
    >
      {options.map((o) => {
        const active = o.value === value;
        const extra: Record<string, string> = {};
        if (o.data) for (const k of Object.keys(o.data)) extra["data-" + k] = o.data[k]!;
        return (
          <button
            key={o.value}
            type="button"
            role="radio"
            aria-checked={active}
            disabled={o.disabled}
            data-testid={o.testId}
            data-value={o.value}
            data-active={active ? "true" : "false"}
            tabIndex={active ? 0 : -1}
            onClick={() => !o.disabled && onChange(o.value)}
            {...extra}
            className={
              "rounded px-2.5 py-1 text-xs font-medium focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-sky-500/55 " +
              (active ? "bg-sky-500/20 text-sky-200" : "text-zinc-400 hover:text-zinc-200") +
              (o.disabled ? " cursor-not-allowed opacity-40" : "")
            }
          >
            {o.label}
          </button>
        );
      })}
    </div>
  );
}

// ── the resize bounds (the prototype's 260..cap clamp) ───────────────────────
const MIN_SIZE = 260;
const KEY_STEP = 24; // arrow-key resize increment

function clampSize(next: number, dock: ShelfDock): number {
  const cap =
    dock === "bottom"
      ? (typeof window !== "undefined" ? window.innerHeight : 800) * 0.85
      : (typeof window !== "undefined" ? window.innerWidth : 1280) * 0.8;
  return Math.max(MIN_SIZE, Math.min(cap, next));
}

function defaultSize(dock: ShelfDock): number {
  if (typeof window === "undefined") return 480;
  return Math.round((dock === "bottom" ? window.innerHeight : window.innerWidth) * 0.5);
}

/** Read the persisted size (fail-closed: any read error → the default). */
function loadSize(dock: ShelfDock): number {
  const key = "cute-dbt:shelfSize:" + dock;
  try {
    const v = Number(typeof localStorage !== "undefined" ? localStorage.getItem(key) : null);
    return v && v > MIN_SIZE ? v : defaultSize(dock);
  } catch {
    return defaultSize(dock);
  }
}

function saveSize(dock: ShelfDock, size: number): void {
  try {
    if (typeof localStorage !== "undefined") localStorage.setItem("cute-dbt:shelfSize:" + dock, String(Math.round(size)));
  } catch {
    /* fail-closed: a write error is non-fatal */
  }
}

// ── icons (inline, no @lucide dep in the shelf chrome — keep it self-contained) ─
function FullscreenIcon({ on }: { on: boolean }): React.ReactElement {
  return (
    <svg width="16" height="16" viewBox="0 0 16 16" fill="none" stroke="currentColor" strokeWidth="1.5" aria-hidden="true">
      {on ? (
        <path d="M6 2v4H2M10 2v4h4M6 14v-4H2M10 14v-4h4" strokeLinecap="round" strokeLinejoin="round" />
      ) : (
        <path d="M2 6V2h4M14 6V2h-4M2 10v4h4M14 10v4h-4" strokeLinecap="round" strokeLinejoin="round" />
      )}
    </svg>
  );
}
function DockIcon({ dock }: { dock: ShelfDock }): React.ReactElement {
  return (
    <svg width="16" height="16" viewBox="0 0 16 16" fill="none" stroke="currentColor" strokeWidth="1.4" aria-hidden="true">
      <rect x="1.5" y="2.5" width="13" height="11" rx="1.5" />
      {dock === "side" ? <path d="M10 2.5v11" /> : <path d="M1.5 10h13" />}
    </svg>
  );
}

export interface DetailShelfProps {
  title: string;
  subtitle?: string;
  /** the shelf-mode segmented value + handler (Diff/File/Compiled). */
  mode: ShelfMode;
  onMode: (m: ShelfMode) => void;
  /** the mode options (a model with no raw side disables File). */
  modeOptions?: SegmentedOption<ShelfMode>[];
  /** the dock side (controlled; defaults to "side"). */
  dock?: ShelfDock;
  onDock?: (d: ShelfDock) => void;
  /** fullscreen (controlled; hides the resize handle when on). */
  fullscreen?: boolean;
  onFullscreen?: (full: boolean) => void;
  /** pinnable model-info panel (controlled). */
  pinned?: boolean;
  onPin?: (pinned: boolean) => void;
  info?: React.ReactNode;
  children: React.ReactNode;
}

const DEFAULT_MODE_OPTIONS: SegmentedOption<ShelfMode>[] = [
  { value: "diff", label: "Diff" },
  { value: "file", label: "File" },
  { value: "compiled", label: "Compiled" },
];

/**
 * DetailShelf — the first-party resizable shelf. The size is local state seeded
 * from localStorage; a pointer-drag on the handle OR arrow keys on the focused
 * `role="separator"` slider resize it (keyboard-accessible by construction). The
 * handle is hidden in fullscreen (nothing to resize when full-bleed).
 */
export function DetailShelf(props: DetailShelfProps): React.ReactElement {
  const {
    title, subtitle, mode, onMode, modeOptions = DEFAULT_MODE_OPTIONS,
    dock = "side", onDock, fullscreen = false, onFullscreen, pinned = false, onPin, info, children,
  } = props;
  const bottom = dock === "bottom";

  const [size, setSize] = useState<number>(() => loadSize(dock));
  const dragRef = useRef<{ x0: number; y0: number; s0: number } | null>(null);

  // re-seed the size when the dock side flips (the two docks persist separately).
  const lastDock = useRef(dock);
  useEffect(() => {
    if (lastDock.current !== dock) {
      lastDock.current = dock;
      setSize(loadSize(dock));
    }
  }, [dock]);

  // the global drag listeners — bound only while a drag is live (the prototype's
  // window mousemove/mouseup). Persists the final size on release.
  useEffect(() => {
    function onMove(e: MouseEvent): void {
      const d = dragRef.current;
      if (!d) return;
      const next = bottom ? d.s0 + (d.y0 - e.clientY) : d.s0 + (d.x0 - e.clientX);
      setSize(clampSize(next, dock));
    }
    function onUp(): void {
      if (dragRef.current) {
        dragRef.current = null;
        document.body.style.userSelect = "";
        setSize((s) => { saveSize(dock, s); return s; });
      }
    }
    window.addEventListener("mousemove", onMove);
    window.addEventListener("mouseup", onUp);
    return () => {
      window.removeEventListener("mousemove", onMove);
      window.removeEventListener("mouseup", onUp);
    };
  }, [bottom, dock]);

  const onHandleMouseDown = useCallback(
    (e: React.MouseEvent): void => {
      e.preventDefault();
      dragRef.current = { x0: e.clientX, y0: e.clientY, s0: size };
      document.body.style.userSelect = "none";
    },
    [size],
  );

  // keyboard-resize: ←/→ (side) or ↑/↓ (bottom) grow/shrink; Home/End jump.
  const onHandleKeyDown = useCallback(
    (e: React.KeyboardEvent): void => {
      const grow = bottom ? "ArrowUp" : "ArrowLeft"; // toward MORE shelf
      const shrink = bottom ? "ArrowDown" : "ArrowRight";
      let delta = 0;
      if (e.key === grow) delta = KEY_STEP;
      else if (e.key === shrink) delta = -KEY_STEP;
      else if (e.key === "Home") { e.preventDefault(); const v = clampSize(MIN_SIZE, dock); setSize(v); saveSize(dock, v); return; }
      else if (e.key === "End") { e.preventDefault(); const v = clampSize(defaultSize(dock) * 1.6, dock); setSize(v); saveSize(dock, v); return; }
      else return;
      e.preventDefault();
      setSize((s) => { const v = clampSize(s + delta, dock); saveSize(dock, v); return v; });
    },
    [bottom, dock],
  );

  const sizeStyle: React.CSSProperties = fullscreen
    ? { flex: "1 1 0%", minWidth: 0, minHeight: 0 }
    : bottom
      ? { height: size }
      : { width: size };

  return (
    <aside
      data-testid="detail-shelf"
      data-dock={dock}
      data-fullscreen={fullscreen ? "true" : "false"}
      data-pinned={pinned ? "true" : "false"}
      data-size={Math.round(size)}
      className={
        "relative flex flex-col border-zinc-800 bg-zinc-950 " +
        (fullscreen ? "" : "shrink-0 ") +
        (bottom ? "w-full border-t" : "h-full border-l")
      }
      style={sizeStyle}
    >
      {/* the FIRST-PARTY resize handle — a focusable role="separator" slider:
          pointer-drag OR arrow-key resizable (accessible by construction). Hidden
          in fullscreen (nothing to resize). */}
      {!fullscreen && (
        <div
          data-testid="shelf-resize"
          role="separator"
          tabIndex={0}
          aria-orientation={bottom ? "horizontal" : "vertical"}
          aria-label="Resize the detail shelf"
          aria-valuemin={MIN_SIZE}
          aria-valuenow={Math.round(size)}
          onMouseDown={onHandleMouseDown}
          onKeyDown={onHandleKeyDown}
          className={
            "group absolute z-30 flex items-center justify-center focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-sky-500 " +
            (bottom ? "left-0 right-0 top-0 h-2 cursor-row-resize" : "top-0 bottom-0 left-0 w-2 cursor-col-resize")
          }
        >
          <div
            className={
              "rounded bg-zinc-700 transition-colors group-hover:bg-sky-500 group-focus-visible:bg-sky-500 " +
              (bottom ? "h-[3px] w-10" : "h-10 w-[3px]")
            }
          />
        </div>
      )}

      {/* header — title/subtitle + the pin/dock/fullscreen chrome + shelf-mode */}
      <header className="flex shrink-0 items-center justify-between gap-2 border-b border-zinc-800 px-4 py-2">
        <div className="min-w-0">
          <div className="truncate font-mono text-sm font-semibold text-zinc-100">{title}</div>
          {subtitle && <div className="text-[10px] uppercase tracking-wide text-zinc-500">{subtitle}</div>}
        </div>
        <div className="flex shrink-0 items-center gap-1.5">
          <button
            type="button"
            data-testid="shelf-pin"
            data-pinned={pinned ? "true" : "false"}
            aria-pressed={pinned}
            aria-label={pinned ? "Unpin model details" : "Pin model details"}
            onClick={() => onPin?.(!pinned)}
            className={
              "inline-flex h-7 w-7 items-center justify-center rounded-md border text-[12px] focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-sky-500/55 " +
              (pinned ? "border-sky-500 bg-sky-500/10 text-sky-300" : "border-zinc-700 text-zinc-400 hover:bg-zinc-800 hover:text-zinc-200")
            }
            title="model details (pin)"
          >
            ⓘ
          </button>
          <button
            type="button"
            data-testid="shelf-dock"
            data-dock={dock}
            aria-label={dock === "side" ? "Dock to bottom" : "Dock to side"}
            onClick={() => onDock?.(dock === "side" ? "bottom" : "side")}
            className="inline-flex h-7 w-7 items-center justify-center rounded-md border border-zinc-700 text-zinc-400 hover:bg-zinc-800 hover:text-zinc-200 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-sky-500/55"
            title={dock === "side" ? "Dock to bottom" : "Dock to side"}
          >
            <DockIcon dock={dock} />
          </button>
          <button
            type="button"
            data-testid="shelf-fullscreen"
            data-fullscreen={fullscreen ? "true" : "false"}
            aria-pressed={fullscreen}
            aria-label={fullscreen ? "Exit fullscreen" : "Fullscreen the detail shelf"}
            onClick={() => onFullscreen?.(!fullscreen)}
            className="inline-flex h-7 w-7 items-center justify-center rounded-md border border-zinc-700 text-zinc-400 hover:bg-zinc-800 hover:text-zinc-200 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-sky-500/55"
            title={fullscreen ? "Exit fullscreen" : "Fullscreen"}
          >
            <FullscreenIcon on={fullscreen} />
          </button>
          <Segmented<ShelfMode>
            testId="shelf-mode"
            ariaLabel="shelf source mode"
            value={mode}
            onChange={onMode}
            options={modeOptions}
          />
        </div>
      </header>

      {/* the pinned model-info panel (controlled; shown only when pinned) */}
      {pinned && info && (
        <div data-testid="shelf-info" className="shrink-0 border-b border-zinc-800 bg-zinc-900/60 px-4 py-3 text-[12px] text-zinc-300">
          {info}
        </div>
      )}

      {/* the shelf body (the code pane + the zone-presence treatments) */}
      <div data-testid="shelf-body" className="flex min-h-0 flex-1 flex-col overflow-auto">
        {children}
      </div>
    </aside>
  );
}
