// Linear notes must come from the tagged changelog section, not a SHA stub.
import { expect, test } from "bun:test";
import { mkdtempSync, readFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { show } from "../lib/git.ts";
import { extractSection, versionFromTag, writeSection } from "./changelog-section.ts";

const CHANGELOG = `# Changelog

## [0.36.0](https://example/compare/v0.35.0...v0.36.0) (2026-09-13)

### Features

* fleet inbox\r

## [0.35.0](https://example/compare/v0.34.0...v0.35.0) (2026-09-13)

* quic streams
`;

test("the version from root and component tags", () => {
  expect(["v0.36.0", "desktop-v0.2.0", "opencode-plugin-v0.3.0"].map(versionFromTag)).toEqual([
    "0.36.0",
    "0.2.0",
    "0.3.0",
  ]);
  expect(() => versionFromTag("next")).toThrow("unsupported release tag");
});

test("one section, up to the next heading; a missing one fails closed", () => {
  expect(extractSection(CHANGELOG, "0.36.0")).toBe(
    "## [0.36.0](https://example/compare/v0.35.0...v0.36.0) (2026-09-13)\n\n### Features\n\n* fleet inbox\n",
  );
  expect(() => extractSection(CHANGELOG, "9.9.9")).toThrow("no changelog heading");
});

test("the CLI writes the section from the changelog at a revision", () => {
  const output = join(mkdtempSync(join(tmpdir(), "changelog-")), "notes.md");
  const version = /^## \[([^\]]+)\]/m.exec(show("HEAD", "CHANGELOG.md") ?? "")?.[1] ?? "";
  expect(writeSection(`v${version}`, "CHANGELOG.md", "HEAD", output)).toBe(0);
  expect(readFileSync(output, "utf8")).toStartWith(`## [${version}]`);
  expect(() => writeSection("v9.9.9", "CHANGELOG.md", "HEAD")).toThrow("no changelog heading");
  expect(() => writeSection("v1.0.0", "CHANGELOG.md", "no-such-ref")).toThrow("cannot read");
});
