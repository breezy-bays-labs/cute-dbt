// DetailShelf + Segmented static-render tests (S6c). The vitest env is `node`
// (no jsdom), so we render to static markup (react-dom/server) and assert the
// SYNCHRONOUS structure — the resize-drag / keyboard-resize / localStorage
// EFFECTS run only in a real DOM and are covered by the Playwright e2e
// (tests/topology-zones-shelf.spec.ts). These pin the accessible skeleton:
// the segmented control, the resize handle (a focusable slider with aria), the
// fullscreen + dock toggles, the pinnable info button, and the honest title.
//
// shadcn/Radix is DEFERRED to S11 — this shelf is FIRST-PARTY: a pointer-drag +
// keyboard-resizable handle + a first-party segmented control + Tailwind.
import { describe, it, expect } from "vitest";
import { renderToStaticMarkup } from "react-dom/server";
import { DetailShelf, Segmented, type ShelfMode } from "./DetailShelf";

const renderShelf = (props: Partial<Parameters<typeof DetailShelf>[0]> = {}): string =>
  renderToStaticMarkup(
    <DetailShelf
      title="customers"
      subtitle="modified · analytics"
      mode={"compiled" as ShelfMode}
      onMode={() => {}}
      {...props}
    >
      <div data-testid="shelf-body-content">body</div>
    </DetailShelf>,
  );

describe("Segmented — first-party accessible segmented control (no shadcn)", () => {
  it("renders a radiogroup of options with the active one aria-checked", () => {
    const html = renderToStaticMarkup(
      <Segmented<string>
        value="b"
        onChange={() => {}}
        options={[
          { value: "a", label: "A" },
          { value: "b", label: "B" },
          { value: "c", label: "C" },
        ]}
        ariaLabel="pick one"
      />,
    );
    expect(html).toContain('role="radiogroup"');
    expect(html).toContain('aria-label="pick one"');
    // three radio options.
    expect((html.match(/role="radio"/g) ?? []).length).toBe(3);
    // exactly the active one is checked.
    expect((html.match(/aria-checked="true"/g) ?? []).length).toBe(1);
    expect(html).toContain('data-value="b"');
    expect(html).toContain('data-active="true"');
  });
});

describe("DetailShelf — first-party resizable shelf skeleton", () => {
  it("renders the title + subtitle honestly", () => {
    const html = renderShelf();
    expect(html).toContain('data-testid="detail-shelf"');
    expect(html).toContain("customers");
    expect(html).toContain("modified · analytics");
    expect(html).toContain('data-testid="shelf-body-content"');
  });

  it("renders the shelf-mode segmented control (Diff/File/Compiled)", () => {
    const html = renderShelf({ modeOptions: [
      { value: "diff", label: "Diff" },
      { value: "file", label: "File" },
      { value: "compiled", label: "Compiled" },
    ] });
    expect(html).toContain('data-testid="shelf-mode"');
    expect(html).toContain('role="radiogroup"');
    expect(html).toContain("Compiled");
  });

  it("renders a FOCUSABLE, keyboard-resizable handle with slider semantics (accessible)", () => {
    const html = renderShelf();
    const handle = html.match(/data-testid="shelf-resize"[^>]*>/)?.[0] ?? "";
    expect(handle).toContain('data-testid="shelf-resize"');
    // the resize handle is reachable + operable from the keyboard (a slider).
    expect(handle).toContain('role="separator"');
    expect(handle).toContain('tabindex="0"');
    expect(handle).toContain("aria-");
    expect(handle).toContain('aria-orientation');
  });

  it("renders the fullscreen + dock toggles (first-party, keyboard-operable)", () => {
    const html = renderShelf();
    expect(html).toContain('data-testid="shelf-fullscreen"');
    expect(html).toContain('data-testid="shelf-dock"');
    // current dock state is announced on the shelf root.
    expect(html).toMatch(/data-dock="(side|bottom)"/);
  });

  it("renders the pinnable info button (pin model details)", () => {
    const html = renderShelf();
    expect(html).toContain('data-testid="shelf-pin"');
    // un-pinned by default.
    expect(html).toContain('data-pinned="false"');
  });

  it("when pinned, the info panel is shown + the pin button reflects the state", () => {
    const html = renderShelf({ pinned: true, info: <div data-testid="pinned-info">details</div> });
    expect(html).toContain('data-pinned="true"');
    expect(html).toContain('data-testid="pinned-info"');
  });

  it("in fullscreen, the resize handle is hidden (no resizing a full-bleed shelf)", () => {
    const html = renderShelf({ fullscreen: true });
    expect(html).toContain('data-fullscreen="true"');
    expect(html).not.toContain('data-testid="shelf-resize"');
  });

  it("the dock state drives the resize handle orientation (side=vertical, bottom=horizontal)", () => {
    const side = renderShelf({ dock: "side" });
    expect(side).toMatch(/data-testid="shelf-resize"[^>]*aria-orientation="vertical"/);
    const bottom = renderShelf({ dock: "bottom" });
    expect(bottom).toMatch(/data-testid="shelf-resize"[^>]*aria-orientation="horizontal"/);
  });
});
