/** tokens/swift/PhuxTokens.swift (PhuxUI) and tokens/swift/PhuxThemes.swift (PhuxProtocol). */
import type { CatalogTheme, Resolved, Sources } from "../tokens.ts";
import {
  HEADER,
  float,
  lowerCamel,
  modelFor,
  quote,
  rgbaHex,
  type Model,
  type Ty,
} from "./model.ts";

const KEYWORDS = new Set([
  "associatedtype",
  "class",
  "deinit",
  "enum",
  "extension",
  "fileprivate",
  "func",
  "import",
  "init",
  "inout",
  "internal",
  "let",
  "open",
  "operator",
  "private",
  "precedencegroup",
  "protocol",
  "public",
  "rethrows",
  "static",
  "struct",
  "subscript",
  "typealias",
  "var",
  "break",
  "case",
  "catch",
  "continue",
  "default",
  "defer",
  "do",
  "else",
  "fallthrough",
  "for",
  "guard",
  "if",
  "in",
  "repeat",
  "return",
  "throw",
  "switch",
  "where",
  "while",
  "Any",
  "as",
  "await",
  "false",
  "is",
  "nil",
  "self",
  "Self",
  "super",
  "throws",
  "true",
  "try",
]);
const field = (k: string) => {
  const c = lowerCamel(k);
  return KEYWORDS.has(c) ? `\`${c}\`` : c;
};

function ty(t: Ty): string {
  if (t.kind === "struct") return t.name;
  if (t.kind === "array") return `[${ty(t.of)}]`;
  return {
    color: "PhuxRGBA",
    string: "String",
    bool: "Bool",
    int: "Int",
    float: "Double",
    double: "Double",
    u64: "UInt64",
  }[t.scalar];
}

function value(t: Ty, v: any, model: Model, depth: number): string {
  const pad = "    ".repeat(depth),
    inner = "    ".repeat(depth + 1);
  if (t.kind === "scalar") {
    switch (t.scalar) {
      case "color":
        return `PhuxRGBA(${rgbaHex(v)})`;
      case "string":
        return quote(v);
      case "bool":
      case "int":
        return String(v);
      case "float":
      case "double":
        return float(v);
      case "u64":
        return `0x${v}`;
    }
  }
  if (t.kind === "array") {
    if (!v.length) return "[]";
    if (t.of.kind === "scalar" && t.of.scalar !== "u64" && v.length <= 16)
      return `[${v.map((x: any) => value(t.of, x, model, depth)).join(", ")}]`;
    return `[\n${v.map((x: any) => `${inner}${value(t.of, x, model, depth + 1)}`).join(",\n")}\n${pad}]`;
  }
  const s = model.structs.find((x) => x.name === t.name)!;
  return `${t.name}(\n${s.fields.map((f) => `${inner}${field(f.key).replaceAll("`", "")}: ${value(f.ty, v[f.key], model, depth + 1)}`).join(",\n")}\n${pad})`;
}

export function emitSwift({ R, S, sha }: { R: Resolved; S: Sources; sha: string }): string {
  const model = modelFor(R, S, sha, "ios");
  const out = [
    `// ${HEADER}`,
    `// phux design tokens ${S.version} (house look: ${R.doc.house}).`,
    "// swiftlint:disable all",
    "",
    "/// An sRGB colour with straight (unmultiplied) alpha.",
    "public struct PhuxRGBA: Equatable, Hashable, Sendable {",
    "    public let r: UInt8",
    "    public let g: UInt8",
    "    public let b: UInt8",
    "    public let a: UInt8",
    "",
    "    /// The colour `0xRRGGBBAA`.",
    "    public init(_ rgba: UInt32) {",
    "        r = UInt8(truncatingIfNeeded: rgba >> 24)",
    "        g = UInt8(truncatingIfNeeded: rgba >> 16)",
    "        b = UInt8(truncatingIfNeeded: rgba >> 8)",
    "        a = UInt8(truncatingIfNeeded: rgba)",
    "    }",
    "}",
    "",
    "public enum PhuxTokens {",
  ];
  for (const s of model.structs) {
    out.push(`    public struct ${s.name}: Equatable, Sendable {`);
    for (const f of s.fields) out.push(`        public let ${field(f.key)}: ${ty(f.ty)}`);
    out.push("    }", "");
  }
  for (const r of model.roots)
    out.push(
      `    /// ${r.doc}`,
      `    public static let ${field(r.key)}: ${ty(r.ty)} = ${value(r.ty, r.value, model, 1)}`,
      "",
    );
  out.push("}");
  return out.join("\n") + "\n";
}

const u32 = (c: string) => `0x${c.slice(1)}`;

/** The catalog entries that ship on mobile, as plain data the app maps onto `TerminalTheme`. */
export function emitSwiftThemes(
  themes: CatalogTheme[],
  catalog: { aliases: Record<string, string>; legacy: any },
): string {
  const entry = (t: CatalogTheme) =>
    [
      "        Entry(",
      `            id: ${quote(t.id)}, name: ${quote(t.name)}, dark: ${t.dark},`,
      "            ansi16: [",
      ...[0, 4, 8, 12].map(
        (i) =>
          `                ${t.ansi
            .slice(i, i + 4)
            .map(u32)
            .join(", ")}${i < 12 ? "," : ""}`,
      ),
      "            ],",
      `            foreground: ${u32(t.fg)}, background: ${u32(t.bg)}, cursor: ${u32(t.cursor)},`,
      `            accent: ${t.accent ? u32(t.accent) : "nil"}, selectionBackground: ${t.selection_bg ? u32(t.selection_bg) : "nil"},`,
      `            icon: ${t.icon}`,
      "        )",
    ].join("\n");
  const map = (m: Record<string, string>) =>
    `[\n${Object.entries(m)
      .sort(([a], [b]) => (a < b ? -1 : 1))
      .map(([k, v]) => `        ${quote(k)}: ${quote(v)}`)
      .join(",\n")}\n    ]`;
  return [
    `// ${HEADER}`,
    "// The unified theme catalog (crates/phux-design/tokens/themes.json), entries that ship on mobile.",
    "// swiftlint:disable all",
    "",
    "public enum PhuxThemes {",
    "    /// One catalog theme: palette entries 0..15 and the defaults, each 0xRRGGBB.",
    "    public struct Entry: Equatable, Sendable {",
    "        public let id: String",
    "        public let name: String",
    "        public let dark: Bool",
    "        public let ansi16: [UInt32]",
    "        public let foreground: UInt32",
    "        public let background: UInt32",
    "        public let cursor: UInt32",
    "        public let accent: UInt32?",
    "        public let selectionBackground: UInt32?",
    "        /// Whether an alternate app icon exists for it.",
    "        public let icon: Bool",
    "    }",
    "",
    "    public static let bundled: [Entry] = [",
    themes.map(entry).join(",\n"),
    "    ]",
    "",
    "    /// Old ids kept for stored preferences, on every client.",
    `    public static let aliases: [String: String] = ${map(catalog.aliases)}`,
    "",
    "    /// Ids this app stored before the catalog was unified.",
    `    public static let legacy: [String: String] = ${map(catalog.legacy?.mobile ?? {})}`,
    "",
    "    /// The bundled theme for an id, following aliases.",
    "    public static func entry(id: String?) -> Entry? {",
    "        guard let id else { return nil }",
    "        let canonical = legacy[id] ?? aliases[id] ?? id",
    "        return bundled.first { $0.id == canonical }",
    "    }",
    "}",
    "",
  ].join("\n");
}
