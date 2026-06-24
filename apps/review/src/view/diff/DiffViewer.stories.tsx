// DiffViewer stories — the documented Pierre shadow-DOM CONTRACT (RISK#2), the
// Storybook half of the council's "encode it as a Storybook story + a Playwright
// assertion." These stories are the canonical, type-checked specification of the
// load-bearing Pierre invariants; the MECHANICAL enforcement is the
// network-denied Playwright gate (tests/diff-cluster.spec.ts), which drives the
// real built app and asserts each property against a live Chromium.
//
// (The project ships no Storybook RUNNER yet — adding the heavy SB toolchain is
// out of S5's scope and would carry its own gates. These stories type-check as
// part of the normal `tsc` gate and document the contract for the eventual SB
// adoption; the Playwright spec is the always-green CI enforcer today.)
//
// LAYER: view (story file; imports view + domain).
import React from "react";
import { DiffViewer } from "../DiffViewer";
import type { CtxFile } from "../../domain/reshape";
import type { RenderedThread } from "../../domain/context-data";

// A diff with BOTH a removed and an added line so a Left (deletion) comment has a
// real old-side row to anchor to.
const PATCH = [
  "diff --git a/models/orders.sql b/models/orders.sql",
  "--- a/models/orders.sql",
  "+++ b/models/orders.sql",
  "@@ -1,4 +1,4 @@",
  " with src as (",
  "-  select id, amount",
  "+  select id, amount, currency",
  "   from {{ ref('raw_orders') }}",
  " )",
].join("\n");

const leftThread: RenderedThread = {
  path: "models/orders.sql",
  line: 2, // the OLD-side (deletion) line.
  side: "Left", // ← anchors on the DELETIONS slot (the RISK#2 contract).
  comments: [{ author: "alice", body: "why drop `amount` precision here?" }],
};

const rightThread: RenderedThread = {
  path: "models/orders.sql",
  line: 2, // the NEW-side (addition) line.
  side: "Right",
  comments: [{ author: "bob", body: "added `currency` — `looks good`" }],
};

const file = (threads: RenderedThread[]): CtxFile => ({ path: "models/orders.sql", lang: "sql", patch: PATCH, threads });

const meta = {
  title: "diff/DiffViewer",
  component: DiffViewer,
};
export default meta;

/** PRIMARY Pierre engine — a Left/deletion comment anchors on the deletions slot. */
export const PierreLeftDeletion = (): React.ReactElement => (
  <DiffViewer file={file([leftThread])} shiki="tokyo-night" reviewers={["alice", "bob"]} />
);

/** PRIMARY Pierre engine — a Right/addition comment anchors on the additions slot. */
export const PierreRightAddition = (): React.ReactElement => (
  <DiffViewer file={file([rightThread])} shiki="tokyo-night" reviewers={["alice", "bob"]} />
);

/** The FIRST-PARTY fallback — a Pierre breakage degrades here, never a blank. */
export const FirstPartyFallback = (): React.ReactElement => (
  <DiffViewer file={file([leftThread, rightThread])} shiki="tokyo-night" reviewers={["alice", "bob"]} forceFallback />
);

/** A ```suggestion comment renders as a first-party Suggested-change diff. */
export const SuggestionComment = (): React.ReactElement => (
  <DiffViewer
    file={file([
      {
        path: "models/orders.sql",
        line: 2,
        side: "Right",
        comments: [{ author: "carol", body: "```suggestion\n  select id, amount, currency, region\n```" }],
      },
    ])}
    shiki="tokyo-night"
    reviewers={["carol"]}
  />
);
