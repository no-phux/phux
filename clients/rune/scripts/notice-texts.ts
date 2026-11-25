/** Rune's crates inherit the license at the root of their pinned checkout. */
import { resolve } from "node:path";
import { readFileSync } from "node:fs";
import {
  type CanonicalLicenses,
  type Crate,
  licenseTexts,
  noticeTexts,
} from "./third-party.ts";

/** Fixed, compiled-in identity assets ship their actual license on every platform. */
export function identityNotices(
  licensePath = resolve(import.meta.dir, "../assets/identity/LICENSE"),
): string {
  const license = readFileSync(licensePath, "utf8").trim();
  if (!license) throw new Error("Identity asset LICENSE is empty");
  return `\n@lobehub/icons — Claude, Codex and OpenCode identity marks\nhttps://github.com/lobehub/lobe-icons\n\n${license}\n`;
}

export function runeNoticeReader(crates: Crate[], canonical: CanonicalLicenses) {
  const rune = crates.find((crate) => crate.name === "rune");
  if (rune === undefined) throw new Error("Rune is missing from the shipping graph");
  const root = resolve(rune.dir, "../..");
  const workspace = licenseTexts(root);
  if (!workspace.has("LICENSE")) throw new Error("Rune's workspace LICENSE is missing");
  return (crate: Crate): Map<string, string> => {
    if (crate.dir !== resolve(root, "crates", crate.name)) return noticeTexts(crate, canonical);
    // Keep crate-local notices too; don't replace actual attribution with a canonical fallback.
    return new Map([
      ...[...workspace].map(([name, body]): [string, string] => [`workspace/${name}`, body]),
      ...licenseTexts(crate.dir),
    ]);
  };
}
