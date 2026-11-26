/** Every token export, where phux commits it, and the mobile set with its manifest. */
import { createHash } from "node:crypto";
import { mkdir, readFile, writeFile } from "node:fs/promises";
import { dirname, resolve } from "node:path";
import { emitColorsets } from "./emit/colorset.ts";
import { emitCss } from "./emit/css.ts";
import { emitKotlin, emitKotlinThemes } from "./emit/kotlin.ts";
import { emitPlurals } from "./emit/plurals.ts";
import { emitRust } from "./emit/rust.ts";
import { emitStrings } from "./emit/strings.ts";
import { emitSwift, emitSwiftThemes } from "./emit/swift.ts";
import { emitXml } from "./emit/xml.ts";
import { paramsSha256 } from "./fit.ts";
import { validate } from "./schema.ts";
import { loadSources, type Sources } from "./sources.ts";
import { resolveTokens, type Resolved } from "./tokens.ts";
import { vectorSubset, vectors } from "./vectors.ts";

export function encodeJson(v: unknown, indent = 2): string {
  return JSON.stringify(v, null, indent) + "\n";
}

export interface Export {
  path: string;
  bytes: string;
}

/** The exports phux commits, at their repository paths; `design-gen --check` refuses drift in them. */
const COMMITTED: Record<string, string> = {
  "rust/phux_tokens.rs": "crates/phux-design/src/generated/tokens.rs",
  "themes.json": "crates/phux-design/tokens/themes.json",
  "derive-vectors.subset.json": "crates/phux-design/tokens/derive-vectors.subset.json",
  "css/phux-tokens.css": "clients/phux-web/assets/phux-tokens.css",
  "ios/AccentColor.colorset/Contents.json":
    "clients/rune/ios/Assets/AccentColor.colorset/Contents.json",
  ...Object.fromEntries(
    ["NeedsYou", "Working", "Failed", "Done"].map((state) => [
      `ios/Status${state}.colorset/Contents.json`,
      `clients/rune/ios/Widgets/Assets/Status${state}.colorset/Contents.json`,
    ]),
  ),
};
/** Committed exports the mobile apps also take. */
const SHARED = new Set(["ios/AccentColor.colorset/Contents.json"]);

export function tokenExports(R: Resolved, S: Sources): Export[] {
  const sha = paramsSha256(R.params);
  const model = { R, S, sha };
  const mobileThemes = R.catalog.themes.filter((t) => t.ships.includes("mobile"));
  return [
    { path: "themes.json", bytes: encodeJson(R.catalog) },
    { path: "derive-vectors.subset.json", bytes: vectorSubset(vectors(R, S)) },
    { path: "rust/phux_tokens.rs", bytes: emitRust(model) },
    { path: "swift/PhuxTokens.swift", bytes: emitSwift(model) },
    { path: "swift/PhuxThemes.swift", bytes: emitSwiftThemes(mobileThemes, R.catalog) },
    { path: "swift/Localizable.xcstrings", bytes: emitStrings(S.vocabulary) },
    { path: "kotlin/PhuxTokens.kt", bytes: emitKotlin(model) },
    { path: "kotlin/PhuxThemes.kt", bytes: emitKotlinThemes(mobileThemes, R.catalog) },
    { path: "android/values/phux_tokens.xml", bytes: emitXml(R.house.light, "light", S.version) },
    {
      path: "android/values-night/phux_tokens.xml",
      bytes: emitXml(R.house.dark, "dark", S.version),
    },
    { path: "android/values/phux_plurals.xml", bytes: emitPlurals(S.vocabulary, S.version) },
    { path: "css/phux-tokens.css", bytes: emitCss(model) },
    ...emitColorsets(R),
  ];
}

/** Resolve the sources, refuse a failing house or schema, and return every export. */
export async function renderTokens(
  root: string,
): Promise<{ R: Resolved; S: Sources; exports: Export[] }> {
  const S = await loadSources(root);
  const R = resolveTokens(S);
  const errors = Object.values(R.verify)
    .flat()
    .filter((v) => v.severity === "error");
  if (errors.length)
    throw Error(
      `house verify failed:\n${errors.map((v) => `  ${v.id} ${(v as any).mode} ${v.role} ${v.value.toFixed(3)} < ${v.bar} on ${v.surface}`).join("\n")}`,
    );
  const problems = validate(S.schema, R.doc);
  if (problems.length)
    throw Error(
      `phux.tokens.json does not match its schema:\n  ${problems.slice(0, 20).join("\n  ")}`,
    );
  return { R, S, exports: tokenExports(R, S) };
}

/** The committed exports, keyed by repository path. */
export function committed(exports: Export[]): Export[] {
  return exports.flatMap((e) =>
    e.path in COMMITTED ? [{ path: COMMITTED[e.path]!, bytes: e.bytes }] : [],
  );
}

/** The mobile exports, keyed by their path under the FFI archives' design/ directory. */
export function mobile(exports: Export[]): Export[] {
  return exports.filter((e) => !(e.path in COMMITTED) || SHARED.has(e.path));
}

/** design/manifest.json: every mobile file's sha256 and the phux commit they were generated from. */
export function manifest(files: Export[], commit: string): string {
  const sha256 = (bytes: string) => createHash("sha256").update(bytes).digest("hex");
  return encodeJson({
    schema: 1,
    phux_rev: commit,
    files: Object.fromEntries(files.map((f) => [f.path, sha256(f.bytes)])),
  });
}

/** Write exports under a directory, or with check, list the ones whose bytes differ. */
export async function writeExports(
  directory: string,
  exports: Export[],
  check = false,
): Promise<string[]> {
  const stale: string[] = [];
  for (const { path, bytes } of exports) {
    const file = resolve(directory, path),
      data = Buffer.from(bytes);
    if (check) {
      let current: Buffer | undefined;
      try {
        current = await readFile(file);
      } catch {
        current = undefined;
      }
      if (!current?.equals(data)) stale.push(path);
    } else {
      await mkdir(dirname(file), { recursive: true });
      await writeFile(file, data);
    }
  }
  return stale;
}
