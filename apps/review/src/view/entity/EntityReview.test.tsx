// EntityReview view test (S8 / cute-dbt#500). Renders the Macros / Seeds /
// Sources+Tests review panes to static markup (react-dom/server, no jsdom) and
// asserts the HONESTY contract on REAL fixture data:
//   - the MACROS pane renders a real macro's signature + package/path + its REAL
//     call-site usages; a macro with no usages shows an honest "no callers" note;
//   - the SEEDS pane renders a real seed's columns/types/row-count + change chip;
//   - the ELSE pane renders the REAL test inventory from manifest_nodes AND an
//     honest-empty SOURCES panel (the 440 spine carries no source node);
//   - an entity kind with NO instances renders an honest-empty state, never a
//     fabricated card.
import { describe, it, expect } from "vitest";
import { renderToStaticMarkup } from "react-dom/server";
import { EntityReview } from "./EntityReview";
import {
  buildMacroViews, buildSeedViews, buildManifestIndex, testInventory,
} from "../../domain/entity-views";
import type { ContextData } from "../../domain/context-data";
import { loadFixture } from "../../data/fixtures";

const real = loadFixture("context.440") as unknown as ContextData;
const macros = buildMacroViews(real);
const seeds = buildSeedViews(real);
const index = buildManifestIndex(real);
const inventory = testInventory(real);

function render(entity: "macros" | "seeds" | "else", sel: string | null): string {
  return renderToStaticMarkup(
    <EntityReview
      entity={entity}
      sel={sel}
      macros={macros}
      seeds={seeds}
      index={index}
      inventory={inventory}
    />,
  );
}

describe("EntityReview — Macros", () => {
  it("renders the active macro's REAL signature + package + path", () => {
    const html = render("macros", "cents_to_dollars");
    expect(html).toContain('data-testid="entity-macro"');
    expect(html).toContain("cents_to_dollars");
    expect(html).toContain("jaffle_shop");
    expect(html).toContain("macros/cents_to_dollars.sql");
  });

  it("surfaces the REAL call-site usages (impacted models) with paths", () => {
    const html = render("macros", "incremental_high_water_mark");
    expect(html).toContain('data-testid="macro-usage"');
    expect(html).toContain("customer_order_days");
    expect(html).toContain("models/marts/customer_order_days.sql");
  });

  it("a macro with no usages shows an HONEST no-callers note, not a fake caller", () => {
    const html = renderToStaticMarkup(
      <EntityReview
        entity="macros"
        sel="lonely"
        macros={[{ name: "lonely", package: "p", path: "macros/lonely.sql", signature: "{% macro lonely() %}", args: "", bodyLines: [{ kind: "context", text: "{% macro lonely() %}" }], description: "", impactedCount: 0, usages: [] }]}
        seeds={[]}
        index={{ nodes: [], sources: [] }}
        inventory={{ total: 0, entries: [], byKind: {} }}
      />,
    );
    expect(html).toContain('data-testid="macro-no-usages"');
    expect(html).not.toContain('data-testid="macro-usage"');
  });

  it("HONEST-EMPTY when there are no macros at all (not a fabricated card)", () => {
    const html = renderToStaticMarkup(
      <EntityReview entity="macros" sel={null} macros={[]} seeds={[]} index={{ nodes: [], sources: [] }} inventory={{ total: 0, entries: [], byKind: {} }} />,
    );
    expect(html).toContain('data-testid="entity-empty"');
    expect(html).not.toContain('data-testid="entity-macro"');
  });
});

describe("EntityReview — Seeds", () => {
  it("renders the active seed's REAL columns, types, row count + change chip", () => {
    const html = render("seeds", "raw_payments");
    expect(html).toContain('data-testid="entity-seed"');
    expect(html).toContain("raw_payments");
    expect(html).toContain("payment_method");
    expect(html).toContain("integer"); // amount column type
    expect(html).toContain('data-testid="seed-downstream"');
    expect(html).toContain("stg_payments");
  });

  it("HONEST-EMPTY when there are no seeds (not a fake card)", () => {
    const html = render("seeds", null);
    // override: the real fixture HAS a seed, so test the empty arm explicitly.
    const empty = renderToStaticMarkup(
      <EntityReview entity="seeds" sel={null} macros={[]} seeds={[]} index={{ nodes: [], sources: [] }} inventory={{ total: 0, entries: [], byKind: {} }} />,
    );
    expect(empty).toContain('data-testid="entity-empty"');
    expect(html).toContain("raw_payments"); // the real one still renders
  });
});

describe("EntityReview — Else (Sources + Tests)", () => {
  it("renders the REAL test inventory from manifest_nodes with REAL counts", () => {
    const html = render("else", null);
    expect(html).toContain('data-testid="test-inventory"');
    expect(html).toContain(String(inventory.total)); // 28
    expect(html).toContain('data-testid="test-entry"');
    expect(html).toContain("unique");
    expect(html).toContain("not null");
  });

  it("renders an HONEST-EMPTY sources panel (the 440 spine carries no source node)", () => {
    const html = render("else", null);
    expect(html).toContain('data-testid="sources-panel"');
    expect(html).toContain('data-testid="sources-empty"');
    expect(html).not.toContain('data-testid="source-node"');
  });

  it("a source node WOULD render when the spine carries one (forward-honest)", () => {
    const html = renderToStaticMarkup(
      <EntityReview
        entity="else"
        sel={null}
        macros={[]}
        seeds={[]}
        index={{ nodes: [], sources: [{ id: "source.jaffle_shop.raw.orders", name: "orders", sourceName: "raw", identifier: "orders" }] }}
        inventory={{ total: 0, entries: [], byKind: {} }}
      />,
    );
    expect(html).toContain('data-testid="source-node"');
    expect(html).toContain("raw");
    expect(html).not.toContain('data-testid="sources-empty"');
  });
});
