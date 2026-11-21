/** tokens/kotlin/PhuxTokens.kt and tokens/kotlin/PhuxThemes.kt for phux-mobile Android. */
import type { CatalogTheme, Resolved, Sources } from "../tokens.ts";
import { HEADER, argbHex, float, lowerCamel, modelFor, type Model, type Ty } from "./model.ts";

const KEYWORDS = new Set([
  "as",
  "break",
  "class",
  "continue",
  "do",
  "else",
  "false",
  "for",
  "fun",
  "if",
  "in",
  "interface",
  "is",
  "null",
  "object",
  "package",
  "return",
  "super",
  "this",
  "throw",
  "true",
  "try",
  "typealias",
  "typeof",
  "val",
  "var",
  "when",
  "while",
]);
const field = (k: string) => {
  const c = lowerCamel(k);
  return KEYWORDS.has(c) ? `\`${c}\`` : c;
};
const str = (s: string) => JSON.stringify(s).replaceAll("$", "\\$");

function ty(t: Ty): string {
  if (t.kind === "struct") return t.name;
  if (t.kind === "array") return `List<${ty(t.of)}>`;
  return {
    color: "PhuxArgb",
    string: "String",
    bool: "Boolean",
    int: "Int",
    float: "Float",
    double: "Double",
    u64: "Long",
  }[t.scalar];
}

function value(t: Ty, v: any, model: Model, depth: number): string {
  const pad = "    ".repeat(depth),
    inner = "    ".repeat(depth + 1);
  if (t.kind === "scalar") {
    switch (t.scalar) {
      case "color":
        return `PhuxArgb(${argbHex(v)}L)`;
      case "string":
        return str(v);
      case "bool":
      case "int":
        return String(v);
      case "float":
        return `${float(v)}f`;
      case "double":
        return float(v);
      case "u64":
        return `0x${v}L`;
    }
  }
  if (t.kind === "array") {
    if (!v.length) return "emptyList()";
    if (t.of.kind === "scalar" && t.of.scalar !== "u64" && v.length <= 16)
      return `listOf(${v.map((x: any) => value(t.of, x, model, depth)).join(", ")})`;
    return `listOf(\n${v.map((x: any) => `${inner}${value(t.of, x, model, depth + 1)},`).join("\n")}\n${pad})`;
  }
  const s = model.structs.find((x) => x.name === t.name)!;
  return `${t.name}(\n${s.fields.map((f) => `${inner}${field(f.key)} = ${value(f.ty, v[f.key], model, depth + 1)},`).join("\n")}\n${pad})`;
}

const SUPPRESS =
  '@file:Suppress("ktlint", "MagicNumber", "LargeClass", "LongMethod", "MaxLineLength", "TooManyFunctions")';

export function emitKotlin({ R, S, sha }: { R: Resolved; S: Sources; sha: string }): string {
  const model = modelFor(R, S, sha, "android");
  const out = [
    `// ${HEADER}`,
    `// phux design tokens ${S.version} (house look: ${R.doc.house}).`,
    SUPPRESS,
    "",
    "package dev.phux.mobile.ui.generated",
    "",
    "/** An sRGB colour with straight alpha as `0xAARRGGBB`, the shape Compose's `Color(Long)` takes. */",
    "@JvmInline",
    "value class PhuxArgb(val argb: Long) {",
    "    val alpha: Int get() = ((argb ushr 24) and 0xFF).toInt()",
    "    val red: Int get() = ((argb ushr 16) and 0xFF).toInt()",
    "    val green: Int get() = ((argb ushr 8) and 0xFF).toInt()",
    "    val blue: Int get() = (argb and 0xFF).toInt()",
    "}",
    "",
    "object PhuxTokens {",
  ];
  for (const s of model.structs) {
    out.push(`    data class ${s.name}(`);
    for (const f of s.fields) out.push(`        val ${field(f.key)}: ${ty(f.ty)},`);
    out.push("    )", "");
  }
  // One builder function per root keeps every JVM method far below the 64 KB limit.
  for (const r of model.roots)
    out.push(
      `    /** ${r.doc} */`,
      `    val ${field(r.key)}: ${ty(r.ty)} = ${lowerCamel(`build_${r.key}`)}()`,
      "",
    );
  for (const r of model.roots)
    out.push(
      `    private fun ${lowerCamel(`build_${r.key}`)}(): ${ty(r.ty)} = ${value(r.ty, r.value, model, 1)}`,
      "",
    );
  out.push(...springConversion(), "}");
  return out.join("\n") + "\n";
}

/** Compose takes (dampingRatio, stiffness); spec §1.8 fixes the conversion from (duration, bounce). */
function springConversion(): string[] {
  return [
    "    /** Compose `spring(dampingRatio, stiffness)` for a SwiftUI-style (duration, bounce) spring, mass 1. */",
    "    fun dampingRatio(spring: Spring): Float = if (spring.bounce >= 0f) 1f - spring.bounce else 1f / (1f + spring.bounce)",
    "",
    "    fun stiffness(spring: Spring): Float {",
    "        val omega = 2.0 * Math.PI / spring.duration",
    "        return (omega * omega).toFloat()",
    "    }",
  ];
}

const rgb = (c: string) => `0x${c.slice(1)}`;

export function emitKotlinThemes(
  themes: CatalogTheme[],
  catalog: { aliases: Record<string, string>; legacy: any },
): string {
  const entry = (t: CatalogTheme) =>
    [
      "        Entry(",
      `            id = ${str(t.id)}, name = ${str(t.name)}, dark = ${t.dark},`,
      "            ansi16 = intArrayOf(",
      ...[0, 4, 8, 12].map(
        (i) =>
          `                ${t.ansi
            .slice(i, i + 4)
            .map(rgb)
            .join(", ")},`,
      ),
      "            ),",
      `            foreground = ${rgb(t.fg)}, background = ${rgb(t.bg)}, cursor = ${rgb(t.cursor)},`,
      `            accent = ${t.accent ? rgb(t.accent) : "null"}, selectionBackground = ${t.selection_bg ? rgb(t.selection_bg) : "null"},`,
      `            icon = ${t.icon},`,
      "        ),",
    ].join("\n");
  const map = (m: Record<string, string>) =>
    `mapOf(\n${Object.entries(m)
      .sort(([a], [b]) => (a < b ? -1 : 1))
      .map(([k, v]) => `        ${str(k)} to ${str(v)},`)
      .join("\n")}\n    )`;
  return [
    `// ${HEADER}`,
    "// The unified theme catalog (crates/phux-design/tokens/themes.json), entries that ship on mobile.",
    SUPPRESS,
    "",
    "package dev.phux.mobile.terminal.rendering.generated",
    "",
    "object PhuxThemes {",
    "    /** One catalog theme: palette entries 0..15 and the defaults, each 0xRRGGBB. */",
    "    class Entry(",
    "        val id: String,",
    "        val name: String,",
    "        val dark: Boolean,",
    "        val ansi16: IntArray,",
    "        val foreground: Int,",
    "        val background: Int,",
    "        val cursor: Int,",
    "        val accent: Int?,",
    "        val selectionBackground: Int?,",
    "        /** Whether an alternate app icon exists for it. */",
    "        val icon: Boolean,",
    "    )",
    "",
    "    val bundled: List<Entry> = listOf(",
    themes.map(entry).join("\n"),
    "    )",
    "",
    "    /** Old ids kept for stored preferences, on every client. */",
    `    val aliases: Map<String, String> = ${map(catalog.aliases)}`,
    "",
    "    /** Ids this app stored before the catalog was unified. */",
    `    val legacy: Map<String, String> = ${map(catalog.legacy?.mobile ?? {})}`,
    "",
    "    /** The bundled theme for an id, following aliases. */",
    "    fun entry(id: String?): Entry? {",
    "        if (id == null) return null",
    "        val canonical = legacy[id] ?: aliases[id] ?: id",
    "        return bundled.firstOrNull { it.id == canonical }",
    "    }",
    "}",
    "",
  ].join("\n");
}
