/** tokens/rust/phux_tokens.rs for phux-design: plain const data, no dependencies, wasm-clean. */
import type { Resolved, Sources } from "../tokens.ts";
import { HEADER, float, modelFor, quote, rgbaHex, type Model, type Ty } from "./model.ts";

const KEYWORDS = new Set([
  "as",
  "break",
  "const",
  "continue",
  "crate",
  "else",
  "enum",
  "extern",
  "false",
  "fn",
  "for",
  "if",
  "impl",
  "in",
  "let",
  "loop",
  "match",
  "mod",
  "move",
  "mut",
  "pub",
  "ref",
  "return",
  "self",
  "Self",
  "static",
  "struct",
  "super",
  "trait",
  "true",
  "type",
  "unsafe",
  "use",
  "where",
  "while",
  "async",
  "await",
  "dyn",
  "abstract",
  "become",
  "box",
  "do",
  "final",
  "macro",
  "override",
  "priv",
  "typeof",
  "unsized",
  "virtual",
  "yield",
  "try",
  "gen",
]);
const field = (k: string) => (KEYWORDS.has(k) ? `r#${k}` : k);
/** `0x141619FF` -> `0x1416_19FF` (clippy::unreadable_literal). */
const grouped = (h: string) => `0x${h.slice(2).replace(/\B(?=(?:[0-9A-F]{4})+$)/g, "_")}`;

/** Bars the solver compares in f64 (Swift's Double), so they are not widened from f32. */
const F64_FIELDS = new Set(["Identity.host_bar", "Identity.host_ease_max"]);

/** Whether a type holds a float anywhere, so it can't derive Eq. */
function floaty(t: Ty, model: Model): boolean {
  if (t.kind === "scalar") return t.scalar === "float" || t.scalar === "double";
  if (t.kind === "array") return floaty(t.of, model);
  return model.structs.find((s) => s.name === t.name)!.fields.some((f) => floaty(f.ty, model));
}

function ty(t: Ty): string {
  if (t.kind === "struct") return t.name;
  if (t.kind === "array") return `&'static [${ty(t.of)}]`;
  return {
    color: "Rgba",
    string: "&'static str",
    bool: "bool",
    int: "u32",
    float: "f32",
    double: "f64",
    u64: "u64",
  }[t.scalar];
}

function value(t: Ty, v: any, model: Model, depth: number): string {
  const pad = "    ".repeat(depth),
    inner = "    ".repeat(depth + 1);
  if (t.kind === "scalar") {
    switch (t.scalar) {
      case "color":
        return `Rgba::hex(${grouped(rgbaHex(v))})`;
      case "string":
        return quote(v);
      case "bool":
        return String(v);
      case "int":
        return String(v);
      case "float":
      case "double":
        return float(v);
      case "u64":
        return grouped(`0x${v}`);
    }
  }
  if (t.kind === "array") {
    if (!v.length) return "&[]";
    const scalarRow = t.of.kind === "scalar";
    if (scalarRow && v.length <= 16 && t.of.kind === "scalar" && t.of.scalar !== "u64")
      return `&[${v.map((x: any) => value(t.of, x, model, depth)).join(", ")}]`;
    return `&[\n${v.map((x: any) => `${inner}${value(t.of, x, model, depth + 1)},`).join("\n")}\n${pad}]`;
  }
  const s = model.structs.find((x) => x.name === t.name)!;
  return `${t.name} {\n${s.fields.map((f) => `${inner}${field(f.key)}: ${value(f.ty, v[f.key], model, depth + 1)},`).join("\n")}\n${pad}}`;
}

export function emitRust({ R, S, sha }: { R: Resolved; S: Sources; sha: string }): string {
  const model = modelFor(R, S, sha, "desktop");
  const out = [
    `// ${HEADER}`,
    `//! phux design tokens ${S.version} (house look: ${R.doc.house}).`,
    "//!",
    "//! Plain `const` data. `phux-design` wraps it with the solver, `tone_of` and `plural`.",
    "",
    "/// An sRGB colour with straight (unmultiplied) alpha.",
    "#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]",
    "pub struct Rgba {",
    "    /// Red, 0-255.",
    "    pub r: u8,",
    "    /// Green, 0-255.",
    "    pub g: u8,",
    "    /// Blue, 0-255.",
    "    pub b: u8,",
    "    /// Alpha, 0-255; 255 is opaque.",
    "    pub a: u8,",
    "}",
    "",
    "impl Rgba {",
    "    /// The colour `0xRRGGBBAA`.",
    "    #[must_use]",
    "    pub const fn hex(rgba: u32) -> Self {",
    "        let [r, g, b, a] = rgba.to_be_bytes();",
    "        Self { r, g, b, a }",
    "    }",
    "}",
    "",
  ];
  for (const s of model.structs) {
    const eq = floaty({ kind: "struct", name: s.name }, model) ? "" : ", Eq";
    out.push(
      `/// Token group \`${s.name}\`.`,
      `#[derive(Debug, Clone, Copy, PartialEq${eq})]`,
      `pub struct ${s.name} {`,
    );
    for (const f of s.fields) {
      const type = F64_FIELDS.has(`${s.name}.${f.key}`) ? "f64" : ty(f.ty);
      out.push(`    /// \`${f.key}\`.`, `    pub ${field(f.key)}: ${type},`);
    }
    out.push("}", "");
  }
  for (const r of model.roots) {
    out.push(
      `/// ${r.doc}`,
      `pub const ${r.key.toUpperCase()}: ${ty(r.ty).replace("&'static ", "&")} = ${value(r.ty, r.value, model, 0)};`,
      "",
    );
  }
  return out.join("\n").replace(/\n+$/, "\n");
}
