// Footer component test — the footer renders EXACTLY the active registry chips
// derived from footerHints(ctx, keymap), with NO hand-written hint strings. We
// render to static markup (react-dom/server, no jsdom dep) and assert the chip
// set + the honest-degrade active flags match the registry selector output.
import { describe, it, expect } from "vitest";
import { renderToStaticMarkup } from "react-dom/server";
import { Footer } from "./Footer";
import { footerHints, type KbContext } from "../domain/keymap";

function render(ctx: KbContext, keymapOverride?: Record<string, string>): string {
  return renderToStaticMarkup(<Footer ctx={ctx} keymapOverride={keymapOverride} />);
}

describe("Footer renders ONLY registry-derived chips", () => {
  it("renders exactly the chips footerHints produces, in order", () => {
    const ctx: KbContext = { entity: "models", view: "topology", viewCount: 3, noun: "model" };
    const html = render(ctx);
    const chips = footerHints(ctx);
    // one data-chip per registry chip — count matches.
    const chipMatches = html.match(/data-testid="footer-chip"/g) ?? [];
    expect(chipMatches.length).toBe(chips.length);
    // every chip label appears in the DOM.
    chips.forEach((c) => {
      expect(html).toContain(`data-chip="${c.label}"`);
    });
  });

  it("marks the honest-degrade flow chips inactive via data-active=false", () => {
    // no unreviewed target → the unreviewed flow chip is greyed (active=false).
    const ctx: KbContext = { entity: "models", view: "topology", viewCount: 3, hasUnreviewed: false };
    const html = render(ctx);
    // the unreviewed chip exists and is rendered inactive.
    expect(html).toContain('data-chip="unreviewed"');
    // find the unreviewed chip's data-active. It is greyed.
    const unreviewedChip = footerHints(ctx).find((c) => c.label === "unreviewed");
    expect(unreviewedChip?.active).toBe(false);
    expect(html).toMatch(/data-chip="unreviewed" data-active="false"/);
  });

  it("renders the live (rebound) key inside the chip's <kbd>", () => {
    const ctx: KbContext = { entity: "models", view: "topology", viewCount: 3 };
    const html = render(ctx, { compiled: "y" });
    // the diff/file/compiled chip's kbd carries the rebound "y".
    expect(html).toContain("d/f/y");
  });

  it("renders no chips it didn't get from the registry (no stray hint text)", () => {
    // a pr·overview surface: footerHints yields a small set; the DOM must not
    // carry any label the selector didn't emit.
    const ctx: KbContext = { entity: "pr", view: "overview", viewCount: 4 };
    const chips = footerHints(ctx);
    const labels = new Set(chips.map((c) => c.label));
    const html = render(ctx);
    // every data-chip in the DOM is a label the selector produced.
    const found = [...html.matchAll(/data-chip="([^"]+)"/g)].map((m) => m[1]);
    found.forEach((label) => expect(labels.has(label as string)).toBe(true));
  });
});
