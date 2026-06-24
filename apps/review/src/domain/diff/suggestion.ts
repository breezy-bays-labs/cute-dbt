// GitHub-style ```suggestion fenced-block parsing. A suggestion block proposes
// replacing the lines a comment is anchored to. Splitting a comment body into
// plain-markdown + suggestion segments lets the view render the latter as a
// diff against the anchored snippet.
//
// LAYER: domain (pure; std-only). Verbatim port of prototype markdown.js.

export type CommentSegment = { type: "md"; text: string } | { type: "suggestion"; code: string };

/** True iff the body contains at least one ```suggestion fence. */
export function hasSuggestion(text: string | null | undefined): boolean {
  return /^```+\s*suggestion\s*$/im.test(String(text == null ? "" : text));
}

/**
 * Split a comment body into ordered md / suggestion segments. Whitespace-only
 * md runs are dropped; an empty suggestion (the deletion case) carries an empty
 * `code` string.
 */
export function splitSuggestions(text: string | null | undefined): CommentSegment[] {
  const lines = String(text == null ? "" : text).split("\n");
  const segs: CommentSegment[] = [];
  let buf: string[] = [];
  let i = 0;
  const flushMd = (): void => {
    if (buf.join("").trim() !== "") segs.push({ type: "md", text: buf.join("\n") });
    buf = [];
  };
  while (i < lines.length) {
    const ln = lines[i]!;
    if (/^```+\s*suggestion\s*$/i.test(ln)) {
      flushMd();
      i++;
      const body: string[] = [];
      while (i < lines.length && !/^```+\s*$/.test(lines[i]!)) {
        body.push(lines[i]!);
        i++;
      }
      i++; // closing fence
      segs.push({ type: "suggestion", code: body.join("\n") });
      continue;
    }
    buf.push(ln);
    i++;
  }
  flushMd();
  return segs;
}
