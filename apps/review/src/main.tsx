// App entry. Preloads the requested theme BEFORE first render; on failure renders
// a visible error banner + sets body[data-highlighter-error] (the loud-fail path),
// never a silent github-dark fallback. Mounts <App/>.
//
// Test hooks (via ?theme=):
//   ?theme=<appTheme>   — start on a specific registered theme (e.g. dracula).
//   ?theme=<rawShiki>   — a value NOT among the 12 app themes is treated as a raw
//                         shiki name and preloaded DIRECTLY. An UNREGISTERED name
//                         (e.g. "monokai") makes preloadHighlighter REJECT → loud
//                         fail, NO silent github-dark fallback. (negative test)
import "@xyflow/react/dist/style.css";
import "./styles.css";
import { createRoot } from "react-dom/client";
import { App } from "./chrome/App";
import { DiffContractHarness } from "./view/diff/DiffContractHarness";
import { TopologyContractHarness } from "./view/topology/TopologyContractHarness";
import { APP_THEMES, shikiName, ensureHighlighter, isAppTheme, type AppTheme } from "./domain/highlighter";

function renderError(root: ReturnType<typeof createRoot>, theme: string, message: string) {
  document.body.setAttribute("data-highlighter-error", message);
  root.render(
    <div
      data-testid="highlighter-error"
      style={{ color: "#f7768e", font: "14px system-ui", padding: 24 }}
    >
      Highlighter failed for theme "{theme}": {message}
    </div>,
  );
}

async function main() {
  const params = new URLSearchParams(location.search);
  const requested = params.get("theme");

  const root = createRoot(document.getElementById("root")!);

  const known = requested != null && isAppTheme(requested);
  const initialTheme: AppTheme = known ? (requested as AppTheme) : "tokyo";
  const preloadName =
    requested == null ? shikiName("tokyo") : known ? shikiName(requested as AppTheme) : requested;

  // Touch APP_THEMES so the static theme registrations run before preload.
  void APP_THEMES;

  try {
    await ensureHighlighter([preloadName]);
  } catch (err) {
    const message = err instanceof Error ? err.message : String(err);
    renderError(root, requested ?? "tokyo", message);
    throw err; // also surface as an uncaught rejection (loud)
  }

  // TEST-ONLY: the diff-cluster Pierre shadow-DOM contract harness (RISK#2),
  // reachable via ?contract=diff. Never on the production render path.
  if (params.get("contract") === "diff") {
    root.render(<DiffContractHarness shiki={preloadName} />);
    return;
  }

  // TEST-ONLY: the S6c zone-region / fan-out harness mounting the real looped
  // model (order_status_pivot), reachable via ?contract=topology. The looped
  // fixtures are out of the sidebar's PR scope (cute-dbt#523), so the live ring
  // click-through is asserted here. Never on the production render path.
  if (params.get("contract") === "topology") {
    root.render(<TopologyContractHarness shiki={preloadName} />);
    return;
  }

  root.render(<App initialTheme={initialTheme} />);
}

void main();
