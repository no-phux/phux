/**
 * tokens/android/values{,-night}/phux_tokens.xml: Android colour resources for one mode, so the
 * pre-Compose window (themes.xml) follows system dark mode. Resources cannot blend, so translucent
 * roles ship only as their flat forms (`phux_flat_<layer>_<group>_<name>`).
 */
import { LAYERS, parse, type Roles } from "../derive.ts";
import { HEADER } from "./model.ts";

const argb = (c: string) => {
  const [r, g, b] = parse(c);
  return `#FF${[r, g, b].map((v) => v.toString(16).toUpperCase().padStart(2, "0")).join("")}`;
};

export function emitXml(r: Roles, mode: "dark" | "light", version: string): string {
  const lines: string[] = [];
  const add = (name: string, c: string) =>
    lines.push(`    <color name="phux_${name}">${argb(c)}</color>`);
  for (const group of [
    "layer",
    "text",
    "border",
    "accent",
    "status",
    "selection",
    "match",
    "diff",
  ] as const) {
    for (const [k, v] of Object.entries(r[group] as Record<string, string>))
      if (parse(v)[3] === 255) add(`${group}_${k}`, v);
  }
  r.syntax.forEach((c, i) => add(`syntax_${i}`, c));
  for (const l of LAYERS)
    for (const [group, values] of Object.entries(r.flat[l]))
      for (const [k, v] of Object.entries(values)) add(`flat_${l}_${group}_${k}`, v);
  return [
    '<?xml version="1.0" encoding="utf-8"?>',
    `<!-- ${HEADER} -->`,
    `<!-- phux design tokens ${version}, ${mode} mode. Opaque roles and flat forms only: Android resources cannot blend. -->`,
    "<resources>",
    ...lines,
    "</resources>",
    "",
  ].join("\n");
}
