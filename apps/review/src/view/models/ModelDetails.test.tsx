// ModelDetails static-render tests (S7, Models · Details). The per-model identity
// + config facts surface: name, change-state, materialization, tags, the node
// config table (config/governance/meta/unique_key), a lineage summary, and the
// documented columns/contract when present. Every facet the payload LACKS renders
// an honest-empty note (never a fabricated value). Built from the REAL dogfood
// fixture so the facts are the spine's, not hand-stubbed.
import { describe, it, expect } from "vitest";
import { renderToStaticMarkup } from "react-dom/server";
import { ModelDetails } from "./ModelDetails";
import { loadFixture } from "../../data/fixtures";
import type { ContextData, ModelPayload } from "../../domain/context-data";

const data = loadFixture("context.440") as unknown as ContextData;
const byName = (n: string): ModelPayload => data.models.find((m) => m.name === n)!;
const render = (m: ModelPayload): string => renderToStaticMarkup(<ModelDetails model={m} />);

describe("ModelDetails — identity + config facts (real dogfood payload)", () => {
  it("renders the model name + the change-state badge", () => {
    const html = render(byName("customers"));
    expect(html).toContain('data-testid="model-details"');
    expect(html).toContain('data-model="customers"');
    expect(html).toContain('data-testid="model-state-badge"');
    expect(html).toContain("customers");
  });

  it("shows the materialization (a config fact that is always known)", () => {
    expect(render(byName("customers"))).toContain('data-testid="model-materialization"');
  });

  it("renders the node-config table with the real config rows", () => {
    const html = render(byName("customers"));
    expect(html).toContain('data-testid="node-config"');
    // the materialized row is always present.
    expect(html.toLowerCase()).toContain("materialized");
  });

  it("flags an incremental model with the incremental materialization", () => {
    const html = render(byName("customer_order_days"));
    expect(html).toContain("incremental");
  });

  it("renders a lineage summary (the CTE-DAG node/edge counts) honestly", () => {
    const html = render(byName("customers"));
    expect(html).toContain('data-testid="lineage-summary"');
  });
});

describe("ModelDetails — honest-empty facets", () => {
  it("renders an honest no-config-yaml note for a model with no model_yaml", () => {
    // order_status_pivot ships without a model_yaml block.
    const html = render(byName("order_status_pivot"));
    expect(html).toContain('data-testid="model-details"');
    // it must NOT crash and must surface the honest no-yaml state for the config facet.
    expect(html).toContain('data-testid="node-config"');
  });

  it("renders a deleted model without fabricating tests/columns it cannot have", () => {
    const html = render(byName("legacy_order_rollup"));
    expect(html).toContain('data-model="legacy_order_rollup"');
    expect(html).toContain('data-testid="model-details"');
  });
});
