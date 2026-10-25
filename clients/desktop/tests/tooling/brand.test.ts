import { expect, test } from "bun:test";
import { mkdtempSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { desktopBrand } from "../../scripts/brand.ts";

test("one identity seam binds profile changes to both packaged icon formats", () => {
  const root = mkdtempSync(join(tmpdir(), "phux-desktop-brand-"));
  for (const id of ["flat-fox", "silver-fox", "classic-prompt"]) {
    writeFileSync(join(root, "brand-profile.json"), JSON.stringify({ schema: 1, id }));
    expect(desktopBrand(root)).toEqual({
      id,
      iconPng: join(root, "AppIcon.png"),
      iconSvg: join(root, "AppIcon.svg"),
    });
  }
  writeFileSync(join(root, "brand-profile.json"), JSON.stringify({ schema: 1, id: "../outside" }));
  expect(() => desktopBrand(root)).toThrow("Invalid desktop brand profile");
});
