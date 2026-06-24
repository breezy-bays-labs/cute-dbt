// First-party chrome primitives (view layer) — the typed React/Tailwind ports of
// the prototype's `ui.js` Tabs / Segmented / Select / Badge / Kbd. These stay
// first-party (rather than pulling shadcn/Radix mid-slice) to match the S0
// chrome's plain-Tailwind posture and avoid a supply-chain-gate churn inside the
// app-shell slice; the shadcn/Radix migration is a deferred presentation pass
// (S11 / the later Claude Design pass). Each primitive carries the `data-kbd-*`
// hooks + `data-testid`s the keyboard chrome + e2e drive.
//
// LAYER: view (may import domain + data; never chrome).
import React from "react";

export interface TabOption {
  value: string;
  label: string;
  /** the ⇧digit hint glyph (positional view key) or entity hotkey. */
  hot?: string;
  /** disable a tab (skeleton entity not yet wired). */
  disabled?: boolean;
}

/** A horizontal tab strip (entity tabs in the Header, view tabs in the SubHeader). */
export function Tabs({
  options,
  value,
  onChange,
  size = "md",
  testid,
}: {
  options: readonly TabOption[];
  value: string;
  onChange: (v: string) => void;
  size?: "sm" | "md";
  testid?: string;
}): React.ReactElement {
  return (
    <div data-testid={testid} className="inline-flex items-center gap-0.5 rounded-md bg-zinc-900/60 p-0.5">
      {options.map((o) => {
        const active = o.value === value;
        return (
          <button
            key={o.value}
            data-testid={`tab-${o.value}`}
            data-active={active}
            disabled={o.disabled}
            onClick={() => !o.disabled && onChange(o.value)}
            className={
              "inline-flex items-center gap-1.5 rounded px-2.5 " +
              (size === "sm" ? "py-1 text-xs" : "py-1.5 text-sm") +
              " " +
              (active
                ? "bg-zinc-800 text-sky-300"
                : o.disabled
                  ? "text-zinc-600"
                  : "text-zinc-400 hover:text-zinc-200")
            }
          >
            {o.label}
            {o.hot ? <Kbd>{o.hot}</Kbd> : null}
          </button>
        );
      })}
    </div>
  );
}

/** A two-or-more option segmented control (Since/Full, diff/file). */
export function Segmented({
  options,
  value,
  onChange,
  testid,
}: {
  options: readonly { value: string; label: string }[];
  value: string;
  onChange: (v: string) => void;
  testid?: string;
}): React.ReactElement {
  return (
    <div data-testid={testid} className="inline-flex items-center rounded-md border border-zinc-800 p-0.5 text-xs">
      {options.map((o) => {
        const active = o.value === value;
        return (
          <button
            key={o.value}
            data-testid={`seg-${o.value}`}
            data-active={active}
            onClick={() => onChange(o.value)}
            className={
              "rounded px-2 py-1 " +
              (active ? "bg-zinc-800 text-zinc-100" : "text-zinc-400 hover:text-zinc-200")
            }
          >
            {o.label}
          </button>
        );
      })}
    </div>
  );
}

/** A native select styled to match the chrome (instance picker). */
export function Select({
  value,
  onChange,
  options,
  ariaLabel,
  testid,
}: {
  value: string;
  onChange: (v: string) => void;
  options: readonly string[];
  ariaLabel?: string;
  testid?: string;
}): React.ReactElement {
  return (
    <select
      data-testid={testid}
      aria-label={ariaLabel}
      value={value}
      onChange={(e) => onChange(e.target.value)}
      className="rounded bg-zinc-800 px-2 py-1 text-sm text-zinc-100"
    >
      {options.map((o) => (
        <option key={o} value={o}>
          {o}
        </option>
      ))}
    </select>
  );
}

/** A change-state badge (added · modified · removed). */
export function Badge({
  tone = "neutral",
  children,
}: {
  tone?: "added" | "modified" | "removed" | "neutral";
  children: React.ReactNode;
}): React.ReactElement {
  const cls =
    tone === "added"
      ? "border-emerald-600 text-emerald-300"
      : tone === "modified"
        ? "border-amber-600 text-amber-300"
        : tone === "removed"
          ? "border-rose-600 text-rose-300"
          : "border-zinc-700 text-zinc-400";
  return (
    <span data-testid="change-badge" data-tone={tone} className={"rounded border px-1.5 py-0.5 text-[10px] " + cls}>
      {children}
    </span>
  );
}

/** A keycap glyph. */
export function Kbd({ children }: { children: React.ReactNode }): React.ReactElement {
  return (
    <kbd className="ml-1 rounded border border-zinc-700 bg-zinc-800 px-1 text-[10px] text-zinc-300">{children}</kbd>
  );
}

/** A chrome button (header actions). */
export function ChromeButton({
  active,
  onClick,
  title,
  testid,
  children,
}: {
  active?: boolean;
  onClick: () => void;
  title?: string;
  testid?: string;
  children: React.ReactNode;
}): React.ReactElement {
  return (
    <button
      data-testid={testid}
      data-active={active}
      onClick={onClick}
      title={title}
      className={
        "inline-flex items-center gap-1 rounded-md border px-2 py-1 text-sm " +
        (active
          ? "border-sky-500 bg-sky-500/15 text-sky-200"
          : "border-zinc-800 text-zinc-300 hover:bg-zinc-800")
      }
    >
      {children}
    </button>
  );
}
