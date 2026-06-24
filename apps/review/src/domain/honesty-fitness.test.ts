// ts-morph FITNESS FUNCTION — the never-a-false-claim type gate. Asserts that
// every honesty-axis field in the data contract is a STRING-LITERAL UNION, never
// a `boolean`. A future PR that downgrades any of the four axes to a bool (the
// exact trust-erosion the strategy exists to kill) FAILS this test even if the
// wire still carries strings.
//
// The four axes + the type alias each MUST resolve to:
//   PRESENCE     Presence         = "compiled_in" | "compiled_out" | "structural"
//   CONFIDENCE   ColumnConfidence = "resolved" | "opaque" | "ambiguous"
//   COVERAGE     CoverageStatus   = "covered" | "uncovered" | "unknown"
//   CELL key.t   CellKeyType      = "absent" | "null" | "number" | "str"
//
// This runs in-process via ts-morph against the real source file (the AST is the
// SSOT, not a hand-copied string). It is a per-PR gate (part of `bun run test`).
import { describe, expect, it } from "vitest";
import { Project, SyntaxKind } from "ts-morph";
import * as path from "node:path";

const CONTRACT = path.resolve(__dirname, "context-data.ts");

const project = new Project({
  tsConfigFilePath: path.resolve(__dirname, "../../tsconfig.json"),
  skipAddingFilesFromTsConfig: true,
});
const src = project.addSourceFileAtPath(CONTRACT);

/** The expected literal-union members for each honesty type alias. */
const HONESTY_ALIASES: Record<string, string[]> = {
  Presence: ["compiled_in", "compiled_out", "structural"],
  ColumnConfidence: ["resolved", "opaque", "ambiguous"],
  CoverageStatus: ["covered", "uncovered", "unknown"],
  CellKeyType: ["absent", "null", "number", "str"],
};

/** A field whose type MUST be (or be a member-typed by) an honesty alias — never bool. */
const HONESTY_FIELDS: { iface: string; field: string; alias: keyof typeof HONESTY_ALIASES }[] = [
  { iface: "RawZone", field: "presence", alias: "Presence" },
  { iface: "RawDagNode", field: "presence", alias: "Presence" },
  { iface: "ColumnEdge", field: "confidence", alias: "ColumnConfidence" },
  { iface: "RawDagEdge", field: "confidence", alias: "ColumnConfidence" },
];

describe("ts-morph honesty fitness — no honesty axis is a boolean", () => {
  it("every honesty type alias is a string-literal union with the exact members (not a boolean)", () => {
    for (const [name, members] of Object.entries(HONESTY_ALIASES)) {
      const alias = src.getTypeAlias(name);
      expect(alias, `type alias ${name} must exist in context-data.ts`).toBeDefined();
      const text = alias!.getTypeNode()!.getText();
      // structurally a union of string literals — assert no `boolean` token.
      expect(text, `${name} must not be a boolean`).not.toMatch(/\bboolean\b/);
      members.forEach((m) => expect(text, `${name} must include "${m}"`).toContain(`"${m}"`));
      // the type node IS a union (or a single literal) of string literals.
      const node = alias!.getTypeNode()!;
      const literalCount = node.getDescendantsOfKind(SyntaxKind.LiteralType).length
        + node.getDescendantsOfKind(SyntaxKind.StringLiteral).length;
      expect(literalCount, `${name} must be string literals, not a bool`).toBeGreaterThanOrEqual(members.length);
    }
  });

  it("each honesty FIELD resolves to its string-union alias, never a boolean", () => {
    for (const { iface, field, alias } of HONESTY_FIELDS) {
      const intf = src.getInterface(iface);
      expect(intf, `interface ${iface} must exist`).toBeDefined();
      const prop = intf!.getProperty(field);
      expect(prop, `${iface}.${field} must exist`).toBeDefined();
      const typeText = prop!.getTypeNode()!.getText();
      // the field's TYPE TEXT references the alias (or its members) — never `boolean`.
      expect(typeText, `${iface}.${field} must not be a boolean`).not.toMatch(/\bboolean\b/);
      const referencesAliasOrMembers = typeText.includes(alias)
        || HONESTY_ALIASES[alias]!.every((m) => typeText.includes(`"${m}"`));
      expect(referencesAliasOrMembers, `${iface}.${field} must be the ${alias} union`).toBe(true);
    }
  });

  it("the CellKey discriminator `t` is the CellKeyType union (the trichotomy), never a bool", () => {
    // CellKey is a discriminated union; each arm's `t` is a string literal.
    const cellKey = src.getTypeAlias("CellKey");
    expect(cellKey).toBeDefined();
    const text = cellKey!.getTypeNode()!.getText();
    expect(text).not.toMatch(/\bboolean\b/);
    HONESTY_ALIASES.CellKeyType!.forEach((m) => expect(text).toContain(`"${m}"`));
  });

  it("guards against a NEW honesty field sneaking in as a bool (named-suffix sweep)", () => {
    // Defense in depth: any interface property literally named presence/confidence
    // anywhere in the contract must not be typed `boolean`.
    src.getInterfaces().forEach((intf) => {
      intf.getProperties().forEach((p) => {
        const n = p.getName();
        if (n === "presence" || n === "confidence") {
          const tn = p.getTypeNode();
          expect(tn, `${intf.getName()}.${n} must have an explicit type node`).toBeDefined();
          expect(tn!.getText(), `${intf.getName()}.${n} must not be a boolean`).not.toMatch(/\bboolean\b/);
        }
      });
    });
  });
});
