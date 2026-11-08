/** Rune's crates inherit the license at the root of their pinned checkout. */
import { resolve } from "node:path";
import {
  type CanonicalLicenses,
  type Crate,
  licenseTexts,
  noticeTexts,
} from "./third-party.ts";

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
