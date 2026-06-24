// ZonePresence — the honest 3-state zone-presence treatment (S6c). Renders ONE
// {% for %} / incremental zone's classification (compiled_in / compiled_out /
// structural) as a distinct, honest card in the detail shelf. This is where the
// never-a-false-claim presence axis becomes visible to the reviewer:
//
//   • compiled_in  — the loop genuinely expanded into the templated CTEs; the
//                    card names the fan-out (its real generated CTE ids).
//   • compiled_out — the INCREMENTAL-ONLY explainer: is_incremental() stripped
//                    the loop this build, so it generated nothing. An honest
//                    explainer — NEVER a fabricated CTE list — DISTINCT from a
//                    compiled_in TEMPLATE collapse (S6b's own purple treatment).
//   • structural   — a wrapper region that templates the loops inside it; it
//                    emits no CTE of its own. Present, but not incremental-only.
//
// The render layer NEVER recomputes the classification — it projects the domain
// `ZonePresenceTreatment` (zone-presence.ts). The compiled_out branch shows the
// honest explainer text + the loop header; it must not list generated CTEs (it
// generated none), which the domain already guarantees (`genIds === []`).
//
// LAYER: view (projects domain facts; never recomputes them).
import React from "react";
import type { ZonePresenceTreatment } from "../../domain/data/zone-presence";

const AMBER = "var(--mat-incremental, #e69f00)";
const PURPLE = "var(--legend-6, #8250df)";
const MUTED = "var(--text-muted, #6c7086)";

/** The chip color + label per presence state — presentation only; the honesty
 *  fact (`presence`/`incrementalOnly`) is the data attribute the gate reads.
 *
 *  The chip MUST agree with the body's tri-branch below (never-a-false-claim): a
 *  PURPLE "templated · N CTEs" fan-out chip is shown ONLY when the loop actually
 *  generated CTEs (`presence === "compiled_in" && generated`). A `compiled_in`
 *  zone that generated NONE (a 0-CTE wrapper, e.g. order_status_pivot's outer
 *  `for region` loop) is structurally a wrapper region — it must render the MUTED
 *  wrapper chip + the wrapper body, never a purple "templated · 0 CTEs" claim. */
function chipStyle(t: ZonePresenceTreatment): { color: string; label: string } {
  if (t.presence === "compiled_out") return { color: AMBER, label: "incremental-only" };
  if (t.presence === "compiled_in" && t.generated)
    return { color: PURPLE, label: "templated · " + t.genCount + " CTEs" };
  return { color: MUTED, label: "wrapper region" };
}

export interface ZonePresenceProps {
  treatment: ZonePresenceTreatment;
}

/** One zone's honest presence treatment card. */
export function ZonePresence({ treatment: t }: ZonePresenceProps): React.ReactElement {
  const chip = chipStyle(t);
  const header = t.loop ? "{% " + t.loop + " %}" : t.template ?? "{% for %}";
  return (
    <div
      data-testid="zone-presence"
      data-zone={t.zoneId}
      data-presence={t.presence}
      data-incremental-only={t.incrementalOnly ? "true" : "false"}
      className="rounded-md border p-3 text-[13px] leading-relaxed"
      style={{
        borderColor: t.incrementalOnly ? AMBER : "var(--border, #2a2b36)",
        background: t.incrementalOnly ? "color-mix(in srgb, var(--mat-incremental, #e69f00) 8%, transparent)" : "transparent",
      }}
    >
      <div className="mb-1.5 flex items-center gap-2">
        <span
          data-testid="presence-chip"
          className="rounded px-1.5 py-0.5 text-[10px] font-mono uppercase tracking-wide"
          style={{ color: chip.color, border: `1px solid ${chip.color}` }}
        >
          {chip.label}
        </span>
        <span className="font-mono text-[11px]" style={{ color: MUTED }}>
          {header}
        </span>
      </div>

      {t.incrementalOnly ? (
        // The honest INCREMENTAL-ONLY explainer (compiled_out) — names
        // is_incremental(), never a fabricated body. The load-bearing 3-state.
        <p data-testid="incremental-only-explainer" style={{ color: "var(--text, #cdd6f4)" }}>
          {t.explainer}
        </p>
      ) : t.presence === "compiled_in" && t.generated ? (
        // compiled_in: name the real fan-out it expanded into (the templated CTEs).
        <div>
          <p style={{ color: "var(--text, #cdd6f4)" }}>{t.explainer}</p>
          <ul data-testid="fanout-ctes" className="mt-1.5 flex flex-wrap gap-1.5">
            {t.genIds.map((id) => (
              <li
                key={id}
                className="rounded px-1.5 py-0.5 font-mono text-[10.5px]"
                style={{ color: PURPLE, border: `1px solid ${PURPLE}`, listStyle: "none" }}
              >
                {id}
              </li>
            ))}
          </ul>
        </div>
      ) : (
        // structural: a wrapper region — present, but emits no CTE of its own.
        <p style={{ color: MUTED }}>{t.explainer}</p>
      )}
    </div>
  );
}

export interface ZonePresenceListProps {
  treatments: ZonePresenceTreatment[];
}

/** Every zone's honest presence treatment. Honest-empty: renders nothing (not
 *  even the wrapper) when there are no zones — never a fabricated section. */
export function ZonePresenceList({ treatments }: ZonePresenceListProps): React.ReactElement | null {
  if (!treatments.length) return null;
  return (
    <div data-testid="zone-presence-list" className="space-y-2">
      <div className="text-[10px] uppercase tracking-wide" style={{ color: MUTED }}>
        Jinja zones
      </div>
      {treatments.map((t) => (
        <ZonePresence key={t.zoneId} treatment={t} />
      ))}
    </div>
  );
}
