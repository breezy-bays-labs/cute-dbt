// DiffViewer — the engine selector + error-boundary degrade. The Pierre PRIMARY
// path needs a real browser (it mounts a shadow-DOM web component) and is
// covered by the network-denied Playwright gate; here we assert the structural
// shell + that `forceFallback` renders the first-party path (the safety net).
import { describe, it, expect } from "vitest";
import { renderToStaticMarkup } from "react-dom/server";
import { DiffViewer } from "./DiffViewer";
import type { CtxFile } from "../domain/reshape";

const file: CtxFile = {
  path: "models/orders.sql",
  lang: "sql",
  patch: ["@@ -1,2 +1,3 @@", " with src as (", "+  select 1", " )"].join("\n"),
  threads: [{ path: "models/orders.sql", line: 2, side: "Right", comments: [{ author: "alice", body: "ok" }] }],
};

describe("DiffViewer", () => {
  it("renders the file header (path + lang)", () => {
    const html = renderToStaticMarkup(<DiffViewer file={file} shiki="tokyo-night" />);
    expect(html).toContain('data-testid="review-file"');
    expect(html).toContain("models/orders.sql");
  });

  it("forceFallback renders the FIRST-PARTY diff (the Pierre escape hatch)", () => {
    const html = renderToStaticMarkup(<DiffViewer file={file} shiki="tokyo-night" forceFallback />);
    expect(html).toContain('data-testid="fallback-diff"');
    expect(html).toContain("fallback renderer");
    // the thread still mounts on the fallback path.
    expect(html).toContain('data-testid="comment-thread"');
  });
});
