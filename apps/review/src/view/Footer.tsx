// The status-bar Footer (view layer) — renders ONLY registry-derived chips. No
// hand-written hint strings: every chip comes from footerHints(ctx, keymap),
// which folds the prototype's hand-written dirHint motions into typed registry
// data and degrades the review-FLOW chips honestly (greyed until their
// when-context is met). The live context is a minimal placeholder here (S1); S2
// wires the real entity×view context from the nav slice.
//
// LAYER: view (may import domain + data; never chrome). It reads the keymap slice
// state via props (the chrome shell owns the store subscription).
import React from "react";
import { footerHints, type KbContext, type Keymap } from "../domain/keymap";

export interface FooterProps {
  /** the live page context (entity·view·codeMode + review-flow signals). */
  ctx: KbContext;
  /** the sparse keymap override (so chip keys reflect rebindings). */
  keymapOverride?: Keymap | null;
}

/**
 * The footer chip strip. Each chip is `<kbd>key/key</kbd> label`, tinted greyed
 * when its registry action is inactive in the current context (honest degrade).
 */
export function Footer({ ctx, keymapOverride }: FooterProps): React.ReactElement {
  const chips = footerHints(ctx, keymapOverride);
  return (
    <footer
      data-testid="footer"
      className="flex h-9 shrink-0 items-center gap-3 overflow-hidden whitespace-nowrap border-t border-zinc-800 bg-zinc-900/60 px-4 font-mono text-[11px] text-zinc-400"
    >
      {chips.map((chip, i) => (
        <span
          key={`${chip.label}-${i}`}
          data-testid="footer-chip"
          data-chip={chip.label}
          data-active={chip.active}
          className={
            "inline-flex items-center gap-1 " + (chip.active ? "text-zinc-300" : "text-zinc-600")
          }
        >
          <kbd className="rounded border border-zinc-700 bg-zinc-800 px-1 text-[10px]">
            {chip.keys.join("/")}
          </kbd>{" "}
          {chip.label}
        </span>
      ))}
    </footer>
  );
}
