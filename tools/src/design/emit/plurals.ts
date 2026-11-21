/** tokens/android/values/phux_plurals.xml: the status words and ICU plurals as Android resources. */
import { HEADER } from "./model.ts";

const esc = (s: string) =>
  s
    .replaceAll("&", "&amp;")
    .replaceAll("<", "&lt;")
    .replaceAll(">", "&gt;")
    .replaceAll("'", "\\'")
    .replaceAll('"', '\\"');

/** `{count, plural, one {# x} other {# y}}` -> [["one", "%d x"], ["other", "%d y"]]. */
export function icuForms(message: string, placeholder: string): [string, string][] {
  const m = /^\{count, plural, (.*)\}$/.exec(message);
  if (!m) throw Error(`not an ICU plural: ${message}`);
  return [...m[1]!.matchAll(/(\w+) \{([^}]*)\}/g)].map((x) => [
    x[1]!,
    x[2]!.replaceAll("#", placeholder),
  ]);
}

export const resourceName = (key: string) => `phux_${key.replaceAll(".", "_")}`;

export function emitPlurals(
  vocabulary: { words: Record<string, string>; plurals: Record<string, string> },
  version: string,
): string {
  const lines: string[] = [];
  for (const [k, v] of Object.entries(vocabulary.words))
    lines.push(`    <string name="${resourceName(`status.${k}`)}">${esc(v)}</string>`);
  for (const [k, v] of Object.entries(vocabulary.plurals)) {
    lines.push(`    <plurals name="${resourceName(k)}">`);
    for (const [form, text] of icuForms(v, "%d"))
      lines.push(`        <item quantity="${form}">${esc(text)}</item>`);
    lines.push("    </plurals>");
  }
  return [
    '<?xml version="1.0" encoding="utf-8"?>',
    `<!-- ${HEADER} -->`,
    `<!-- phux design tokens ${version}: status words and count plurals (en). -->`,
    "<resources>",
    ...lines,
    "</resources>",
    "",
  ].join("\n");
}
