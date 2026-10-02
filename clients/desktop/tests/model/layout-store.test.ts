import { describe, expect, test } from "bun:test";
import {
  existsSync,
  mkdirSync,
  mkdtempSync,
  readFileSync,
  readdirSync,
  rmSync,
  writeFileSync,
} from "node:fs";
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

  test("a failed snapshot preserves the last layout and removes its staging file", () => {
    const directory = mkdtempSync(join(tmpdir(), "phux-layout-"));
    try {
      const path = join(directory, "layout.json");
      const store = fileLayoutStore(path);
      store.write({ tabs: ["keep"] });
      const cyclic: { self?: unknown } = {};
      cyclic.self = cyclic;
      expect(() => store.write(cyclic)).toThrow();
      expect(store.read()).toEqual({ tabs: ["keep"] });
      expect(readdirSync(directory)).toEqual(["layout.json"]);
    } finally {
      rmSync(directory, { recursive: true, force: true });
    }
  });

  test("target stores import legacy state without writing into it or one another", () => {
    const directory = mkdtempSync(join(tmpdir(), "phux-layout-"));
    try {
      const legacy = join(directory, "layout.json");
      writeFileSync(legacy, '{"tabs":["legacy"]}');
      const first = fileLayoutStore(join(directory, "first/layout.json"), legacy);
      const second = fileLayoutStore(join(directory, "second/layout.json"), legacy);
      expect(first.read()).toEqual({ tabs: ["legacy"] });
      first.write({ tabs: ["first"] });
      second.write({ tabs: ["second"] });
      expect(first.read()).toEqual({ tabs: ["first"] });
      expect(second.read()).toEqual({ tabs: ["second"] });
      expect(JSON.parse(readFileSync(legacy, "utf8"))).toEqual({ tabs: ["legacy"] });
      const broken = join(directory, "blocked/layout.json");
      mkdirSync(broken, { recursive: true });
      expect(fileLayoutStore(broken, legacy).read()).toBeNull();
    } finally {
      rmSync(directory, { recursive: true, force: true });
    }
  });
});
