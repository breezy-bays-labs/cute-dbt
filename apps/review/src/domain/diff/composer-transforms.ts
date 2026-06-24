// Pure text transforms for the comment Composer toolbar — selection-aware
// markdown edits + @-mention parsing. Each takes a TextSel (text + selection)
// and returns a fresh TextSel, so the React layer just calls these and writes
// the result back into the textarea. Verbatim port of prototype composer.js.
//
// LAYER: domain (pure; std-only).

export interface TextSel {
  text: string;
  selStart: number;
  selEnd: number;
}

export type Transform = (s: TextSel) => TextSel;

/** Split a TextSel around its selection into (before, selected, after). */
function parts(s: TextSel): [string, string, string] {
  return [s.text.slice(0, s.selStart), s.text.slice(s.selStart, s.selEnd), s.text.slice(s.selEnd)];
}

/**
 * Wrap the selection with pre/post markers (e.g. ** ** for bold). With no
 * selection, inserts the placeholder and selects it.
 */
export function wrapSelection(s: TextSel, pre: string, post: string, placeholder: string): TextSel {
  const [b, sel, a] = parts(s);
  const body = sel || placeholder;
  const text = b + pre + body + post + a;
  return { text, selStart: (b + pre).length, selEnd: (b + pre + body).length };
}

/**
 * Prefix each selected line with `mk(line, index)`. Inserts a leading newline
 * when the selection does not start at a line boundary.
 */
export function prefixLines(s: TextSel, mk: (line: string, index: number) => string): TextSel {
  const [b, sel, a] = parts(s);
  const atLineStart = b === "" || b.endsWith("\n");
  const lead = atLineStart ? "" : "\n";
  const out = (sel || "")
    .split("\n")
    .map((l, i) => mk(l, i))
    .join("\n");
  const text = b + lead + out + a;
  return { text, selStart: (b + lead).length, selEnd: (b + lead + out).length };
}

/** Insert a markdown link, selecting the `url` placeholder for quick replace. */
export function insertLink(s: TextSel): TextSel {
  const [b, sel, a] = parts(s);
  const label = sel || "text";
  const mid = b + "[" + label + "](";
  const url = "url";
  return { text: mid + url + ")" + a, selStart: mid.length, selEnd: mid.length + url.length };
}

/**
 * Open a ```suggestion block seeded with the anchored snippet (the RIGHT-side
 * source the suggestion replaces). The seeded body is selected.
 */
export function insertSuggestion(s: TextSel, seed: string | null | undefined): TextSel {
  const before = s.text.slice(0, s.selStart);
  const prefix = before.trim() ? before.replace(/\s*$/, "") + "\n\n" : "";
  const open = "```suggestion\n";
  const body = seed != null ? seed : "";
  const next = prefix + open + body + "\n```\n";
  const text = next + s.text.slice(s.selEnd);
  return { text, selStart: (prefix + open).length, selEnd: (prefix + open + body).length };
}

export interface MentionMatch {
  /** caret index of the `@` sigil that opens the active mention token. */
  from: number;
  /** the lowercased partial name typed after the `@`. */
  query: string;
}

/**
 * Detect an in-progress @-mention at the caret: an `@` preceded by the start of
 * input or whitespace, followed by word chars. `from` points AT the `@` sigil.
 * Returns null when not mentioning.
 */
export function matchMention(text: string, caret: number): MentionMatch | null {
  const upto = text.slice(0, caret);
  const m = upto.match(/(?:^|\s)@([\w-]*)$/);
  if (!m) return null;
  // m[1] is the partial name (always present when the regex matches); the `@`
  // sits one char before it.
  const name = m[1] ?? "";
  return { from: caret - name.length - 1, query: name.toLowerCase() };
}

/**
 * Replace the active @-token (the `@`+partial-name, from..caret) with the picked
 * `name` + a trailing space. The result reads `@name ` so it round-trips to the
 * PR verbatim as a GitHub mention.
 */
export function applyMention(text: string, caret: number, mention: MentionMatch, name: string): TextSel {
  const next = text.slice(0, mention.from) + "@" + name + " " + text.slice(caret);
  const pos = mention.from + 1 + name.length + 1;
  return { text: next, selStart: pos, selEnd: pos };
}
