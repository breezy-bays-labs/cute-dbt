// The Entity×View matrix — the SSOT for which views each entity exposes, the
// positional ⇧digit view keys, and the discriminated routing target. Ported
// faithfully from the cute-dbt-next prototype's `app.js` (`AVAIL`, `viewPos`,
// `viewKey`, `viewActionId`, `renderView`'s (entity, view) switch).
//
// LAYER: PURE DOMAIN — no I/O, no zustand, no React. The nav slice, the view
// router (chrome), and the dispatcher all DERIVE from this module; none of them
// re-declares the matrix or re-computes a view's positional key.
//
// Why a domain module and not a chrome const: the ⇧digit→view mapping, the
// `view-N` action id, and the "is this view available for this entity" guard
// are PURE FACTS over the matrix. Putting them in the domain lets the dispatcher
// (data layer) and the router (chrome) share ONE definition — the prototype's
// `viewPos`/`viewKey`/`viewActionId` lived in app.js where only the chrome could
// see them; here they are testable, layer-clean facts.

import type { Entity } from "./keymap";

/** Every view the matrix can route to (the union the router switches over). */
export type View =
  | "overview"
  | "lineage"
  | "files"
  | "timeline"
  | "topology"
  | "node"
  | "data"
  | "code"
  | "coverage"
  | "review"
  | "markdown";

/**
 * The availability matrix — which views each entity exposes, IN ORDER. The
 * order is load-bearing: a view's ⇧digit key and its `view-N` action id derive
 * PURELY from its position here (⇧1 = first, ⇧2 = second, …). This mirrors the
 * prototype's `AVAIL`.
 *
 * `as const satisfies Record<Entity, readonly View[]>` pins the literal tuples
 * (so positions are statically known) AND checks every entity is covered with
 * only real `View`s — the type the council's S2 row mandates.
 *
 * NOTE: Models `code` deliberately lives OUTSIDE this matrix. In the prototype
 * the Models Code surface is reachable by deep-link / action (open a comment
 * anchor, the `d`/`f` code-mode keys) but is NOT a positional tab — Topology is
 * the Models review surface. `routeTarget` still resolves `models · code`, but
 * `viewKeyFor`/`viewActionFor` return no positional key for it (it is not in the
 * tuple), exactly as in `app.js`.
 */
export const AVAIL = {
  pr: ["overview", "lineage", "files", "timeline"],
  models: ["topology", "node", "data"],
  macros: ["review"],
  seeds: ["review"],
  else: ["review"],
} as const satisfies Record<Entity, readonly View[]>;

/** The ENTITIES in number-row order (1 PR · 2 Models · 3 Macros · 4 Seeds · 5 Else). */
export const ENTITY_ORDER: readonly Entity[] = ["pr", "models", "macros", "seeds", "else"];

/** Display noun per entity (the instance-cycle chip + Select label). */
export const ENTITY_NOUN: Record<Entity, string> = {
  pr: "pr",
  models: "model",
  macros: "macro",
  seeds: "seed",
  else: "file",
};

/** Display label per entity (the Header tabs). */
export const ENTITY_LABEL: Record<Entity, string> = {
  pr: "PR",
  models: "Models",
  macros: "Macros",
  seeds: "Seeds",
  else: "Else",
};

/** Human label per view (the SubHeader view tabs). Mirrors the prototype's VIEWS + viewLabel. */
const VIEW_LABEL: Record<View, string> = {
  overview: "Overview",
  lineage: "Topology",
  files: "Files",
  timeline: "Timeline",
  topology: "Review",
  node: "Details",
  data: "Unit tests",
  code: "Code",
  coverage: "Coverage",
  review: "Review",
  markdown: "Readme",
};

/** The available view tuple for an entity (never undefined — every entity is in the matrix). */
export function viewsFor(entity: Entity): readonly View[] {
  return AVAIL[entity];
}

/** The default (first) view for an entity — the fallback when viewMap has nothing. */
export function defaultViewFor(entity: Entity): View {
  return AVAIL[entity][0];
}

/** Is `view` a positional tab of `entity` (i.e. in its AVAIL tuple)? */
export function isAvailable(entity: Entity, view: View): boolean {
  return (AVAIL[entity] as readonly View[]).includes(view);
}

/**
 * Is `(entity, view)` a ROUTABLE pair — either a positional tab OR an off-matrix
 * surface `routeTarget` resolves to a real component (e.g. Models `code`, which
 * is deliberately off-matrix but reachable by deep-link / the review-flow keys)?
 * This is the predicate `deriveView` uses so a remembered off-matrix-but-routable
 * view (Models `code`, the V1 keyboard-review surface) survives, while a genuinely
 * unavailable view (Models `files`) still falls back. `not-available` ⇒ false.
 */
export function isRoutable(entity: Entity, view: View): boolean {
  return routeTarget(entity, view).kind !== "not-available";
}

/** Position of a view in the entity's matrix tuple, or -1 (e.g. Models `code`). */
export function viewPos(entity: Entity, view: View): number {
  return (AVAIL[entity] as readonly View[]).indexOf(view);
}

/** The positional ⇧digit key glyph for a view ("⇧1".."⇧4"), or "" when off-matrix. */
export function viewKeyFor(entity: Entity, view: View): string {
  const i = viewPos(entity, view);
  return i >= 0 ? "⇧" + (i + 1) : "";
}

/** The `view-N` registry action id for a view ("view-1".."view-4"), or null when off-matrix. */
export function viewActionFor(entity: Entity, view: View): string | null {
  const i = viewPos(entity, view);
  return i >= 0 ? "view-" + (i + 1) : null;
}

/** The view at a 1-based positional digit for an entity, or null if out of range. */
export function viewAtDigit(entity: Entity, digit: number): View | null {
  const views = AVAIL[entity];
  if (digit >= 1 && digit <= views.length) return views[digit - 1] ?? null;
  return null;
}

/** The human label for a view in an entity's context (the SubHeader tab text). */
export function viewLabelFor(_entity: Entity, view: View): string {
  // The prototype re-labels two Models views (topology→"Review", data→"Unit tests")
  // and macros·node→"Details"; those live in VIEW_LABEL already (topology="Review",
  // data="Unit tests"). The entity-specific override surface is otherwise empty
  // in S2 (the `_entity` param is retained for the parity hook later slices add).
  return VIEW_LABEL[view];
}

/**
 * The discriminated routing target for an (entity, view) pair — the typed
 * equivalent of the prototype's `renderView` switch. The router (chrome) maps
 * each kind to a component; here we resolve the FACT of which surface a pair
 * routes to, so routing is a pure, testable function of the matrix (not a tangle
 * of `if (entity===… && view===…)` in the JSX).
 *
 * Models `code` (off-matrix) routes to `models-code`; everything else routes by
 * its (entity, view) pair. Unknown pairs fall through to `not-available` (the
 * router renders an honest "view not available for this entity" placeholder
 * rather than crashing — matching the prototype's `EMPTY_DETAIL` safety posture).
 */
export type RouteTarget =
  | { kind: "pr-overview" }
  | { kind: "pr-lineage" }
  | { kind: "pr-files" }
  | { kind: "pr-timeline" }
  | { kind: "models-topology" }
  | { kind: "models-node" }
  | { kind: "models-data" }
  | { kind: "models-code" }
  | { kind: "entity-review"; entity: "macros" | "seeds" | "else" }
  | { kind: "not-available"; entity: Entity; view: View };

export function routeTarget(entity: Entity, view: View): RouteTarget {
  if (entity === "pr") {
    if (view === "overview") return { kind: "pr-overview" };
    if (view === "lineage") return { kind: "pr-lineage" };
    if (view === "files") return { kind: "pr-files" };
    if (view === "timeline") return { kind: "pr-timeline" };
    return { kind: "not-available", entity, view };
  }
  if (entity === "models") {
    if (view === "topology") return { kind: "models-topology" };
    if (view === "node") return { kind: "models-node" };
    if (view === "data") return { kind: "models-data" };
    if (view === "code") return { kind: "models-code" }; // off-matrix, reachable
    return { kind: "not-available", entity, view };
  }
  // macros / seeds / else share the single "review" surface.
  if (view === "review") return { kind: "entity-review", entity };
  return { kind: "not-available", entity, view };
}
