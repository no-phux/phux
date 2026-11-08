import { afterEach, expect, test } from "bun:test";
import { mkdirSync, mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { canonicalLicenses, type Crate, renderNotices } from "../../desktop/scripts/notices.ts";
import { runeNoticeReader } from "./notice-texts.ts";

const dirs: string[] = [];
afterEach(() => dirs.splice(0).forEach((dir) => rmSync(dir, { recursive: true, force: true })));

function fixture() {
  const root = mkdtempSync(join(tmpdir(), "rune-notices-"));
  dirs.push(root);
  const crate = (name: string, dir = join(root, "crates", name)): Crate => {
    mkdirSync(dir, { recursive: true });
    return {
      name, dir, version: "0.0.1", license: "MIT", repository: "", authors: [],
      firstParty: false, buildOnly: false,
    };
  };
  return { root, crate };
}

test("all pinned Rune crates retain the workspace copyright and local notices", () => {
  const { root, crate } = fixture();
  const license = canonicalLicenses().MIT.replace("{holders}", "2026 Anomaly");
  writeFileSync(join(root, "LICENSE"), license);
  const crates = [crate("rune"), crate("rune-motion"), crate("rune-notify")];
  writeFileSync(join(crates[1]!.dir, "NOTICE"), "A crate-local notice");
  const read = runeNoticeReader(crates, canonicalLicenses());
  for (const entry of crates) expect(read(entry).get("workspace/LICENSE")).toBe(license);
  expect(read(crates[1]!).get("NOTICE")).toBe("A crate-local notice");
  const rendered = renderNotices(crates, read, "Phux Rune");
  expect(rendered.split(license)).toHaveLength(2);
  expect(rendered).not.toContain("The rune authors");
});

test("a same-named crate outside the pinned checkout keeps its own license", () => {
  const { root, crate } = fixture();
  writeFileSync(join(root, "LICENSE"), "Rune workspace license");
  const rune = crate("rune");
  const other = crate("rune-motion", join(root, "other", "rune-motion"));
  writeFileSync(join(other.dir, "LICENSE"), "Another license");
  expect([...runeNoticeReader([rune, other], canonicalLicenses())(other).values()])
    .toEqual(["Another license"]);
});

test("missing Rune or workspace license fails closed", () => {
  expect(() => runeNoticeReader([], canonicalLicenses())).toThrow("missing from the shipping graph");
  const { crate } = fixture();
  expect(() => runeNoticeReader([crate("rune")], canonicalLicenses())).toThrow("LICENSE is missing");
});

test("unrelated crates without license files still use the canonical fallback", () => {
  const { root, crate } = fixture();
  writeFileSync(join(root, "LICENSE"), "Rune workspace license");
  const rune = crate("rune");
  const other = crate("other", join(root, "registry", "other"));
  const texts = runeNoticeReader([rune, other], canonicalLicenses())(other);
  expect(texts.get("MIT, canonical text")).toContain("Copyright (c) The other authors");
  expect(texts.has("workspace/LICENSE")).toBe(false);
});
