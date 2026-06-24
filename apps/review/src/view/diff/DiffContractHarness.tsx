// DiffContractHarness — a TEST-ONLY surface mounting the Pierre shadow-DOM
// CONTRACT scenarios (RISK#2) so the network-denied Playwright gate can drive
// them against a real Chromium. Reachable ONLY via `?contract=diff` (main.tsx);
// it is never on the production render path. Mirrors the DiffViewer.stories.tsx
// scenarios as a live, mechanically-asserted contract.
//
// LAYER: view (imports view + domain). Test affordance, query-param-gated.
import React from "react";
import { DiffViewer } from "../DiffViewer";
import type { CtxFile } from "../../domain/reshape";
import type { RenderedThread } from "../../domain/context-data";

const PATCH = [
  "diff --git a/models/orders.sql b/models/orders.sql",
  "--- a/models/orders.sql",
  "+++ b/models/orders.sql",
  "@@ -1,4 +1,4 @@",
  " with src as (",
  "-  select id, amount",
  "+  select id, amount, currency",
  "   from raw_orders",
  " )",
].join("\n");

const leftThread: RenderedThread = {
  path: "models/orders.sql",
  line: 2,
  side: "Left", // anchors on the DELETIONS slot.
  comments: [{ author: "alice", body: "why drop amount precision?" }],
};
const rightThread: RenderedThread = {
  path: "models/orders.sql",
  line: 2,
  side: "Right", // anchors on the ADDITIONS slot.
  comments: [{ author: "bob", body: "added currency" }],
};

const file = (threads: RenderedThread[]): CtxFile => ({ path: "models/orders.sql", lang: "sql", patch: PATCH, threads });

export function DiffContractHarness({ shiki }: { shiki: string }): React.ReactElement {
  return (
    <div data-testid="diff-contract-harness" style={{ padding: 16, background: "#1a1b26", color: "#a9b1d6", minHeight: "100vh" }}>
      <h1 style={{ font: "14px system-ui" }}>Diff cluster — Pierre shadow-DOM contract (RISK#2)</h1>

      <div data-testid="contract-pierre-left" style={{ marginTop: 16 }}>
        <h2 style={{ font: "12px system-ui", opacity: 0.7 }}>Pierre · Left/deletion comment → deletions slot</h2>
        <DiffViewer file={file([leftThread])} shiki={shiki} reviewers={["alice", "bob"]} />
      </div>

      <div data-testid="contract-pierre-right" style={{ marginTop: 16 }}>
        <h2 style={{ font: "12px system-ui", opacity: 0.7 }}>Pierre · Right/addition comment → additions slot</h2>
        <DiffViewer file={file([rightThread])} shiki={shiki} reviewers={["alice", "bob"]} />
      </div>

      <div data-testid="contract-fallback" style={{ marginTop: 16 }}>
        <h2 style={{ font: "12px system-ui", opacity: 0.7 }}>First-party fallback (Pierre forced down)</h2>
        <DiffViewer file={file([leftThread, rightThread])} shiki={shiki} reviewers={["alice", "bob"]} forceFallback />
      </div>
    </div>
  );
}
