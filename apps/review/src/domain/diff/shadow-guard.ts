// shadow-guard (RISK#2) — the pure predicate the app's capture-phase keydown
// dispatcher consults to AVOID hijacking keystrokes that belong to the Pierre
// diff's shadow root (or to an editable field like the Composer). A keystroke
// whose composedPath crosses a `diffs-container` host (or a textarea/input/
// contentEditable) is the surface's own input — the dispatcher must let it pass.
//
// LAYER: domain (pure; std-only). The behavioral case is exercised in Playwright
// (typing inside the Pierre shadow root must not trigger entity keys).

/** Shadow-host tag names whose presence in the path means "the diff owns this key". */
export const DEFAULT_SHADOW_HOSTS: readonly string[] = ["diffs-container"];

interface PathNode {
  tagName?: string;
  nodeName?: string;
  isContentEditable?: boolean;
}

/**
 * True iff any node in `path` (a composedPath() array) is a shadow host (default
 * `diffs-container`) OR an editable element (textarea / input / contentEditable).
 * Total + safe: an empty/undefined path returns false.
 */
export function isInsideShadowOrEditable(
  path: readonly unknown[] | undefined,
  shadowHosts: readonly string[] = DEFAULT_SHADOW_HOSTS,
): boolean {
  if (!path || !path.length) return false;
  const hosts = new Set(shadowHosts.map((h) => h.toUpperCase()));
  for (const raw of path) {
    const node = raw as PathNode;
    const tag = (node.tagName ?? node.nodeName ?? "").toUpperCase();
    if (!tag) continue;
    if (hosts.has(tag)) return true;
    if (tag === "TEXTAREA" || tag === "INPUT" || tag === "SELECT") return true;
    if (node.isContentEditable === true) return true;
  }
  return false;
}
