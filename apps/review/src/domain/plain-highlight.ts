// Offline regex syntax highlighter — the SYNCHRONOUS fallback used before the
// async Shiki singleton resolves (and on the Pierre-down first-party path). It
// returns {text, cls} token runs per line so the renderer never shows a flash
// of unstyled code. Supports SQL (+ Jinja) and YAML. Token classes map to the
// --tok-* CSS vars. Verbatim port of prototype highlight.js.
//
// LAYER: domain (pure; std-only).

export interface PlainToken {
  text: string;
  /** token class suffix ("kw" | "fn" | "num" | "str" | "cmt" | "jinja" | "key" | "punct" | ""). */
  cls: string;
}

const SQL_KW = new RegExp(
  "\\b(" +
    [
      "select", "from", "where", "with", "as", "join", "left", "right", "inner", "outer", "full", "cross",
      "on", "using", "group", "by", "order", "having", "limit", "offset", "and", "or", "not", "in", "is", "null",
      "case", "when", "then", "else", "end", "union", "all", "distinct", "over", "partition", "asc", "desc",
      "insert", "into", "values", "update", "set", "delete", "create", "table", "view", "temp", "exists",
      "between", "like", "ilike", "coalesce", "cast", "interval", "current_date", "current_timestamp",
      "true", "false", "unbounded", "preceding", "following", "row", "rows", "nullif", "filter", "return",
    ].join("|") +
    ")\\b",
  "gi",
);
const SQL_FN =
  /\b(sum|count|avg|min|max|round|abs|lower|upper|trim|length|concat|md5|row_number|rank|dense_rank|lag|lead|date_trunc|strftime|extract)\s*(?=\()/gi;

function pushPlain(out: PlainToken[], text: string): void {
  if (!text) return;
  const marks: [number, number, string][] = [];
  text.replace(SQL_KW, (m, _g, off: number) => {
    marks.push([off, off + m.length, "kw"]);
    return m;
  });
  text.replace(SQL_FN, (m, _g, off: number) => {
    marks.push([off, off + m.length, "fn"]);
    return m;
  });
  text.replace(/\b\d+(\.\d+)?\b/g, (m, _g, off: number) => {
    marks.push([off, off + m.length, "num"]);
    return m;
  });
  marks.sort((a, b) => a[0] - b[0]);
  let cursor = 0;
  for (const [s, e, cls] of marks) {
    if (s < cursor) continue;
    if (s > cursor) out.push({ text: text.slice(cursor, s), cls: "" });
    out.push({ text: text.slice(s, e), cls });
    cursor = e;
  }
  if (cursor < text.length) out.push({ text: text.slice(cursor), cls: "" });
}

export function highlightSQL(line: string): PlainToken[] {
  const out: PlainToken[] = [];
  const re = /(\{\{[^}]*\}\}|\{%[^%]*%\}|--[^\n]*|'(?:[^'\\]|\\.)*'|"(?:[^"\\]|\\.)*")/g;
  let last = 0;
  let m: RegExpExecArray | null;
  while ((m = re.exec(line))) {
    if (m.index > last) pushPlain(out, line.slice(last, m.index));
    const t = m[0];
    const cls = t.startsWith("--") ? "cmt" : t.startsWith("{{") || t.startsWith("{%") ? "jinja" : "str";
    out.push({ text: t, cls });
    last = m.index + t.length;
  }
  if (last < line.length) pushPlain(out, line.slice(last));
  return out.length ? out : [{ text: line, cls: "" }];
}

export function highlightYAML(line: string): PlainToken[] {
  const out: PlainToken[] = [];
  const comment = line.indexOf("#");
  let body = line;
  let tail = "";
  if (comment >= 0) {
    body = line.slice(0, comment);
    tail = line.slice(comment);
  }
  const kv = body.match(/^(\s*-?\s*)([\w.-]+)(:)(.*)$/);
  if (kv) {
    out.push({ text: kv[1] ?? "", cls: "" });
    out.push({ text: kv[2] ?? "", cls: "key" });
    out.push({ text: kv[3] ?? "", cls: "punct" });
    if (kv[4]) {
      const v = kv[4];
      if (/^\s*['"]/.test(v)) out.push({ text: v, cls: "str" });
      else if (/^\s*\d+(\.\d+)?\s*$/.test(v)) out.push({ text: v, cls: "num" });
      else if (/\b(true|false|null)\b/.test(v)) out.push({ text: v, cls: "kw" });
      else out.push({ text: v, cls: "" });
    }
  } else {
    out.push({ text: body, cls: "" });
  }
  if (tail) out.push({ text: tail, cls: "cmt" });
  return out.length ? out : [{ text: line, cls: "" }];
}

export type PlainLang = "sql" | "yaml" | "jinja" | "dbt" | "yml" | string;

export function highlightPlain(line: string, lang?: PlainLang): PlainToken[] {
  const s = line == null ? "" : String(line);
  if (lang === "yaml" || lang === "yml") return highlightYAML(s);
  const sqlish = !lang || lang === "sql" || lang === "jinja" || lang === "dbt";
  if (!sqlish) return [{ text: s, cls: "" }];
  return highlightSQL(s);
}
