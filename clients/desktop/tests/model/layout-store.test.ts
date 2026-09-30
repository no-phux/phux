import { describe, expect, test } from "bun:test";
import { existsSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { fileLayoutStore } from "../../scripts/layout-store";
import { defaultDisplay, loadLayoutStore, saveLayout } from "../../src/workspace/persist";

describe("saved layout files", () => {
  test("malformed JSON stays at its original path and is never automatically replaced", () => {
    const directory = mkdtempSync(join(tmpdir(), "phux-layout-"));
    const path = join(directory, "layout.json");
    const original = '{"version": 2, "tabs": [';
    try {
      writeFileSync(path, original);
      const layouts = loadLayoutStore(fileLayoutStore(path));
      layouts.write(saveLayout("server-a", [], defaultDisplay));
      expect(readFileSync(path, "utf8")).toBe(original);
      expect(existsSync(`${path}.damaged`)).toBe(false);
      expect(existsSync(`${path}.next`)).toBe(false);
    } finally {
      rmSync(directory, { recursive: true, force: true });
    }
  });

  test("only an absent file allows first-launch persistence", () => {
    const directory = mkdtempSync(join(tmpdir(), "phux-layout-"));
    const path = join(directory, "layout.json");
    try {
      const snapshot = saveLayout("server-a", [], defaultDisplay);
      loadLayoutStore(fileLayoutStore(path)).write(snapshot);
      expect(fileLayoutStore(path).read()).toEqual(snapshot);
      // A directory at the file path is a read error, not absence.
      expect(loadLayoutStore(fileLayoutStore(directory)).blocked).toBe(true);
    } finally {
      rmSync(directory, { recursive: true, force: true });
    }
  });
});
