// The fixture catalog — the embedded, scrub-clean synthetic context payloads.
//
// PROVENANCE: all four files are the synthetic PR-440 dogfood (+ the
// comments-showcase golden), extracted from committed/rendered, leak-free
// sources — verified scrub-clean (zero /Users / root_path / home paths). They
// stand in for the eventual `--context-out` artifact until the Rust S3a contract
// lands. Never replace with a real-project render (would bake metadata.root_path).
import context440 from "../fixtures/context.440.json";
import contextSample from "../fixtures/context.sample.json";
import context440SinceReview from "../fixtures/context.440.since-review.json";
import { parseContext, type ParsedContextData } from "../domain/schema";

export type FixtureId = "context.440" | "context.sample" | "context.440.since-review";

const RAW: Record<FixtureId, unknown> = {
  "context.440": context440,
  "context.sample": contextSample,
  "context.440.since-review": context440SinceReview,
};

export const FIXTURE_IDS: readonly FixtureId[] = [
  "context.440",
  "context.sample",
  "context.440.since-review",
] as const;

/** Load + Zod-validate a fixture by id. Throws loudly on shape drift. */
export function loadFixture(id: FixtureId): ParsedContextData {
  return parseContext(RAW[id]);
}

/** The raw (unvalidated) fixture payload — for the schema/drift tests. */
export function rawFixture(id: FixtureId): unknown {
  return RAW[id];
}
