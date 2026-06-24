// The data slice — the `activeSource` field that REPLACES the prototype's
// module-global `_activeSrc` (context.js `let _activeSrc = "pr440"`). A non-Models
// surface that isn't passed the dataset as a prop reads the same source's records
// via the store's `activeSource`, rather than a hidden module global.
//
// LAYER: data (may import domain + the fixture catalog; never view/chrome).
//
// The dataset itself is built lazily + WeakMap-memoized in domain/data/dataset.ts
// (keyed on the parsed ContextData identity), so this slice carries only the
// SELECTION (which source is active); the heavy reshape is cached downstream.

import type { ContextData } from "../domain/context-data";
import { buildDataset, type Dataset } from "../domain/data/dataset";
import { loadFixture, type FixtureId } from "./fixtures";

/** The selectable context sources (the prototype's SOURCES keys, fixture-mapped). */
export type DataSource = FixtureId;

export const DATA_SOURCES: readonly DataSource[] = [
  "context.440",
  "context.sample",
  "context.440.since-review",
] as const;

/** Human labels for the source picker (mirrors the prototype's SOURCES.label). */
export const DATA_SOURCE_LABELS: Record<DataSource, string> = {
  "context.440": "PR #440 (dogfood)",
  "context.sample": "Comments showcase",
  "context.440.since-review": "PR #440 — since last review",
};

export function isDataSource(s: unknown): s is DataSource {
  return typeof s === "string" && (DATA_SOURCES as readonly string[]).includes(s);
}

/** The default active source (the prototype's `_activeSrc = "pr440"`). */
export const DATA_DEFAULTS = { activeSource: "context.440" as DataSource };

export interface DataSlice {
  /** The active context source — the explicit replacement for the module global. */
  activeSource: DataSource;
  /** Set the active source (fail-closed: a non-source is ignored). */
  setActiveSource: (src: unknown) => void;
}

export type DataSliceSet = (
  partial: Partial<{ activeSource: DataSource }> | ((s: { activeSource: DataSource }) => Partial<{ activeSource: DataSource }>),
) => void;

export function createDataSlice(set: DataSliceSet): DataSlice {
  return {
    activeSource: DATA_DEFAULTS.activeSource,
    setActiveSource: (src: unknown) => {
      if (isDataSource(src)) set({ activeSource: src });
    },
  };
}

/**
 * dataSlice — the source-keyed dataset accessor. Loads + Zod-validates the
 * fixture for the given source (fail-closed via parseContext) and builds the
 * WeakMap-memoized dataset. The selector the views call: `dataSlice(activeSource)`.
 */
export function dataSlice(src: DataSource): Dataset {
  // The Zod gate is a tolerant SUBSET pin (.passthrough() preserves the rest);
  // its inferred type widens some fields the hand-authored ContextData narrows.
  // The runtime shape is validated — narrow to the consumer contract here.
  return buildDataset(loadFixture(src) as unknown as ContextData);
}
