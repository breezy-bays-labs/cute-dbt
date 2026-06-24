// DiffContractHarness — a TEST-ONLY surface mounting the Pierre shadow-DOM
// CONTRACT scenarios (RISK#2) so the network-denied Playwright gate can drive
// them against a real Chromium. Reachable ONLY via `?contract=diff` (main.tsx);
// it is never on the production render path. Mirrors the DiffViewer.stories.tsx
// scenarios as a live, mechanically-asserted contract.
//
// It mounts the REAL single capture-phase keydown dispatcher (`useKeydown`) — the
// same one `<App>` mounts — and renders an entity-REACTIVE probe (the live
// `store.entity`). That makes the RISK#2 shadow-keyboard guard testable IN
// CONTEXT: a real dispatcher exists that WOULD route an entity key, and the probe
// visibly changes when it does. The Playwright gate proves the guard by asserting
// a shadow-root "3" leaves the probe unchanged while a light-DOM "3" flips it —
// so the test fails if `isInsideShadowOrEditable` (the guard) is removed.
//
// LAYER: view (imports view + domain + the data dispatcher). Test affordance,
// query-param-gated.
import React from "react";
import { DiffViewer } from "../DiffViewer";
import { useAppStore } from "../../data/store";
import { useKeydown } from "../../data/use-keydown";
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
  // Mount the REAL single dispatcher — the very listener `<App>` mounts. Without
  // it the shadow-keyboard guard test would be vacuous (no listener to hijack the
  // key). With it, an entity key dispatched in the LIGHT DOM flips `store.entity`;
  // the guard's job is to ensure the SAME key dispatched from INSIDE the Pierre
  // shadow root does NOT. The probe below makes that side-effect observable.
  useKeydown();
  // Entity-reactive probe: re-renders on every `store.entity` change, so the gate
  // can read the live entity before/after a keystroke and assert the guard held.
  const entity = useAppStore((s) => s.entity);

  return (
    <div data-testid="diff-contract-harness" style={{ padding: 16, background: "#1a1b26", color: "#a9b1d6", minHeight: "100vh" }}>
      <h1 style={{ font: "14px system-ui" }}>Diff cluster — Pierre shadow-DOM contract (RISK#2)</h1>

      {/* The live store.entity — the dispatcher's observable side-effect. A bare
          "3" routes to set-entity("macros"); the guard must suppress it when the
          keystroke originates inside the Pierre shadow root. */}
      <div data-testid="contract-entity-probe" data-entity={entity} style={{ font: "12px system-ui", opacity: 0.7 }}>
        active entity: {entity}
      </div>

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
