// ZonePresence treatment tests (S6c) — the honest 3-state presence rendering.
// The vitest env is `node` (no jsdom), so we render to static markup
// (react-dom/server) and assert the SYNCHRONOUS structure — the same posture as
// CompiledView.test.tsx / GraphNode.test.tsx. The honesty decision (compiled_in
// / compiled_out / structural) is the DOMAIN's (zone-presence.ts); this layer
// only PROJECTS it, so these tests pin that each state renders its DISTINCT,
// honest treatment and the compiled_out (incremental-only) explainer is never a
// fabricated body.
import { describe, it, expect } from "vitest";
import { renderToStaticMarkup } from "react-dom/server";
import { ZonePresence, ZonePresenceList } from "./ZonePresence";
import type { ZonePresenceTreatment } from "../../domain/data/zone-presence";

const treatment = (over: Partial<ZonePresenceTreatment>): ZonePresenceTreatment => ({
  zi: 0,
  zoneId: "z0",
  nodeId: "zone:0",
  kind: "for_loop",
  presence: "compiled_in",
  generated: true,
  genCount: 2,
  genIds: ["completed_orders", "pending_orders"],
  incrementalOnly: false,
  template: "{{ status }}_orders",
  loop: "for status in statuses",
  explainer: "expands into the templated CTEs below",
  ...over,
});

const renderOne = (t: ZonePresenceTreatment): string =>
  renderToStaticMarkup(<ZonePresence treatment={t} />);

describe("ZonePresence — the 3-state honest treatment", () => {
  it("compiled_in: the TEMPLATE treatment names the fan-out CTE count + ids (never the incremental explainer)", () => {
    const html = renderOne(treatment({ presence: "compiled_in" }));
    expect(html).toContain('data-testid="zone-presence"');
    expect(html).toContain('data-presence="compiled_in"');
    expect(html).toContain('data-incremental-only="false"');
    // names the fan-out it genuinely expanded into.
    expect(html).toMatch(/2 CTEs|completed_orders/);
    // it is NOT the incremental-only treatment.
    expect(html).not.toContain('data-testid="incremental-only-explainer"');
  });

  it("compiled_out: the INCREMENTAL-ONLY explainer (honest 3-state) names is_incremental, never a fake body", () => {
    const html = renderOne(
      treatment({
        presence: "compiled_out",
        incrementalOnly: true,
        generated: false,
        genCount: 0,
        genIds: [],
        explainer:
          "Declared inside {% if is_incremental() %} — this loop generates CTEs only when dbt runs the model incrementally, so a fresh dbt compile strips it and the compiled DAG never shows them.",
      }),
    );
    expect(html).toContain('data-presence="compiled_out"');
    expect(html).toContain('data-incremental-only="true"');
    // the load-bearing honest explainer is present + names is_incremental().
    expect(html).toContain('data-testid="incremental-only-explainer"');
    expect(html).toMatch(/is_incremental/i);
    expect(html).toMatch(/compile|strip/i);
    // an incremental-only chip labels the state.
    expect(html).toContain('data-testid="presence-chip"');
    // NEVER a fabricated CTE list (it generated nothing this build).
    expect(html).not.toContain("completed_orders");
  });

  it("structural: the WRAPPER treatment is present but emits no CTE of its own (never incremental-only)", () => {
    const html = renderOne(
      treatment({
        presence: "structural",
        incrementalOnly: false,
        generated: false,
        genCount: 0,
        genIds: [],
        loop: "for region in regions",
        template: null,
        explainer: "A {% for %} wrapper region — it templates the loops inside it; it emits no CTE of its own.",
      }),
    );
    expect(html).toContain('data-presence="structural"');
    expect(html).toContain('data-incremental-only="false"');
    // it must NOT borrow the incremental-only explainer (never-a-false-claim).
    expect(html).not.toContain('data-testid="incremental-only-explainer"');
    expect(html).toMatch(/wrapper|region/i);
  });

  it("compiled_in WITHOUT generation (0-CTE wrapper) renders the wrapper chip + body — NEVER 'templated · 0 CTEs' (never-a-false-claim)", () => {
    // order_status_pivot zone:1 — presence "compiled_in" carried verbatim, but the
    // `for region` wrapper generated NO CTE of its own (node_map.raw["zone:1"] = []).
    // The chip MUST agree with the body's else-branch (wrapper region), not claim a
    // purple "templated · 0 CTEs" fan-out next to "emits no CTE of its own".
    const html = renderOne(
      treatment({
        presence: "compiled_in",
        generated: false,
        genCount: 0,
        genIds: [],
        loop: "for region in regions",
        template: null,
        explainer: "A {% for %} wrapper region — it templates the loops inside it; it emits no CTE of its own.",
      }),
    );
    expect(html).toContain('data-presence="compiled_in"');
    expect(html).toContain('data-incremental-only="false"');
    // the contradiction the slice must prevent: a 0-CTE compiled_in must NOT claim
    // the purple "templated · 0 CTEs" fan-out chip.
    expect(html).not.toMatch(/templated · 0 CTEs/);
    expect(html).not.toMatch(/0 CTEs/);
    // it renders the honest wrapper-region treatment instead.
    expect(html).toMatch(/wrapper region/i);
    // and never a fabricated incremental explainer either.
    expect(html).not.toContain('data-testid="incremental-only-explainer"');
  });

  it("compiled_in WITH generation still names the real fan-out CTE count (regression guard for the chip fix)", () => {
    const html = renderOne(treatment({ presence: "compiled_in", generated: true, genCount: 2 }));
    expect(html).toMatch(/2 CTEs/);
    expect(html).toContain('data-testid="fanout-ctes"');
  });

  it("every state carries the zone id + loop header (selectable cross-ref to the ring)", () => {
    const html = renderOne(treatment({ zoneId: "z2", loop: "for status in statuses" }));
    expect(html).toContain('data-zone="z2"');
    expect(html).toMatch(/for status in statuses/);
  });
});

describe("ZonePresenceList — every zone's treatment, honest-empty when none", () => {
  it("renders a card per treatment", () => {
    const html = renderToStaticMarkup(
      <ZonePresenceList
        treatments={[
          treatment({ zi: 0, zoneId: "z0", presence: "compiled_in" }),
          treatment({ zi: 1, zoneId: "z1", presence: "compiled_out", incrementalOnly: true }),
        ]}
      />,
    );
    const cards = html.match(/data-testid="zone-presence"/g) ?? [];
    expect(cards.length).toBe(2);
    expect(html).toContain('data-testid="zone-presence-list"');
  });

  it("renders nothing for an empty treatment list (honest-empty, never a fabricated zone)", () => {
    const html = renderToStaticMarkup(<ZonePresenceList treatments={[]} />);
    expect(html).not.toContain('data-testid="zone-presence"');
    expect(html).not.toContain('data-testid="zone-presence-list"');
  });
});
