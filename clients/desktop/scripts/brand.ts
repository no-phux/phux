import { readFileSync } from "node:fs";
import { resolve } from "node:path";

const packaging = resolve(import.meta.dir, "../packaging");

/** The generated identity is the seam for every packaged desktop brand asset. */
export function desktopBrand(directory = packaging) {
  const profile: unknown = JSON.parse(
    readFileSync(resolve(directory, "brand-profile.json"), "utf8"),
  );
  if (
    typeof profile !== "object" ||
    profile === null ||
    !("schema" in profile) ||
    profile.schema !== 1 ||
    !("id" in profile) ||
    typeof profile.id !== "string" ||
    !/^[a-z][a-z0-9-]*$/.test(profile.id)
  ) {
    throw new Error("Invalid desktop brand profile; synchronize an approved brand release");
  }
  return {
    id: profile.id,
    iconPng: resolve(directory, "AppIcon.png"),
    iconSvg: resolve(directory, "AppIcon.svg"),
  };
}
