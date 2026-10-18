// Linear notes must come from the tagged changelog section, not a SHA stub.
import { describe, expect, test } from "bun:test";
import { mkdtempSync, readFileSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { repoPath } from "../lib/repo.ts";
import { extractSection, versionFromTag, writeSection } from "./changelog-section.ts";

const CHANGELOG = `# Changelog

## [0.36.0](https://example/compare/v0.35.0...v0.36.0) (2026-09-13)

### Features

* fleet inbox

## [0.35.0](https://example/compare/v0.34.0...v0.35.0) (2026-09-13)

### Features

* quic streams
`;

describe("changelog section", () => {
  test("the version from root and component tags", () => {
    expect(versionFromTag("v0.36.0")).toBe("0.36.0");
    expect(versionFromTag("desktop-v0.2.0")).toBe("0.2.0");
    expect(versionFromTag("opencode-plugin-v0.3.0")).toBe("0.3.0");
    expect(() => versionFromTag("next")).toThrow("unsupported release tag");
  });

  test("extracts one section and stops at the next heading", () => {
    const section = extractSection(CHANGELOG, "0.36.0");
    expect(section).toContain("fleet inbox");
    expect(section).not.toContain("quic streams");
    expect(section.startsWith("## [0.36.0]")).toBe(true);
    expect(section.endsWith("* fleet inbox\n")).toBe(true);
  });

  test("a missing section fails closed", () => {
    expect(() => extractSection(CHANGELOG, "9.9.9")).toThrow("no changelog heading");
  });

  test("the CLI writes the tagged section", () => {
    const dir = mkdtempSync(join(tmpdir(), "changelog-"));
    const [changelog, output] = [join(dir, "CHANGELOG.md"), join(dir, "notes.md")];
    writeFileSync(changelog, CHANGELOG);
    expect(writeSection("v0.36.0", changelog, output)).toBe(0);
    expect(readFileSync(output, "utf8")).toBe(extractSection(CHANGELOG, "0.36.0"));
    expect(writeSection("v9.9.9", changelog, output)).toBe(1);
  });

  test("the real root changelog has the current head section", () => {
    const text = readFileSync(repoPath("CHANGELOG.md"), "utf8");
    const heading = text.split("\n").find((line) => line.startsWith("## ["));
    const version = heading?.split("[")[1]?.split("]")[0] ?? "";
    expect(extractSection(text, version).startsWith(`## [${version}]`)).toBe(true);
  });
});
