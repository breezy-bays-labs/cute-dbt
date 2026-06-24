// modelFacets — the per-model facet aggregation for the Models · Details and
// Models · Unit-tests views (cute-dbt#499). A PURE fold over a built ModelRecord
// that summarizes WHICH facets the payload actually carries, so each view can
// render an honest-empty state for any facet the model lacks. Every field is
// read VERBATIM from the record (the spine already did the work in
// buildModelRecord / deriveInfo / adaptTest) — nothing is recomputed or
// fabricated here. A missing facet is `false` / `0` / `""`, never a placeholder.
//
// LAYER: domain/data (pure; no view, no chrome, no I/O).

import type { ModelRecord } from "./dataset";

/** A model's facet presence summary — the never-a-false-claim spine for the two
 *  Models views: each boolean answers "does the payload carry this facet?" so a
 *  view can choose an honest table vs an honest-empty note. */
export interface ModelFacets {
  /** the model change-state, carried verbatim from the record. */
  change: "added" | "removed" | "modified";
  /** the resolved materialization (always known: incremental flag → yaml → view). */
  materialized: string;
  /** the incremental facet (drives the incremental chip + the inc-run test note). */
  isIncremental: boolean;
  /** the unique_key grain is documented (a real config fact, not unknown). */
  hasGrain: boolean;
  /** the model carries any config / governance / meta / tags facts to show. */
  hasConfig: boolean;
  /** the count of documented config rows (config + governance + meta + tags). */
  configCount: number;
  /** any unit tests at all (false ⇒ the honest no-unit-tests empty state). */
  hasTests: boolean;
  /** the total unit-test count. */
  testCount: number;
  /** the count of new-or-changed unit tests (the reviewer-worthy ones). */
  changedTestCount: number;
  /** at least one given references an external CSV fixture the payload only points
   *  at (⇒ the honest external-fixture note, never a fabricated grid). */
  hasExternalFixture: boolean;
  /** any coverage-check findings to show. */
  hasCoverage: boolean;
  /** the coverage-check count. */
  coverageCount: number;
  /** any downstream consumers (the best-available blast radius). */
  hasDownstream: boolean;
  /** the downstream-consumer count. */
  downstreamCount: number;
}

/**
 * modelFacets — aggregate a built ModelRecord into its facet-presence summary.
 * Pure + total: every input shape (a deleted model with no tests, an added model
 * with no yaml) yields a well-formed summary whose missing facets read as empty.
 */
export function modelFacets(rec: ModelRecord): ModelFacets {
  const info = rec.info;
  const configCount =
    Object.keys(info.config).length +
    Object.keys(info.gov).length +
    info.meta.length +
    (info.tags.length > 0 ? 1 : 0);
  const tests = rec.unitTests;
  const changedTestCount = tests.filter((t) => t.isNew || t.changed).length;
  const hasExternalFixture = tests.some(
    (t) => !!t.external || t.given.some((g) => !!g.external),
  );
  const downstream = rec.downstream ?? [];
  return {
    change: rec.change,
    materialized: info.materialized,
    isIncremental: info.materialized === "incremental",
    hasGrain: info.grain.known,
    hasConfig: configCount > 0,
    configCount,
    hasTests: tests.length > 0,
    testCount: tests.length,
    changedTestCount,
    hasExternalFixture,
    hasCoverage: rec.coverage.checks.length > 0,
    coverageCount: rec.coverage.checks.length,
    hasDownstream: downstream.length > 0,
    downstreamCount: downstream.length,
  };
}
