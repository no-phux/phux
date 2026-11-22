import { expect, test } from "bun:test";
import { readdir, readFile } from "node:fs/promises";
import { join, resolve } from "node:path";
import { runInNewContext } from "node:vm";

// The SDK keeps this list private to its Vite preset; read it from the installed package.
import { pluginSharedModules, pluginWindowModules } from "../node_modules/@opencode/plugin/dist/gui/modules.js";

import { en } from "../src/gui/i18n.js";

const root = resolve(import.meta.dir, "..");
const bundle = join(root, "dist/gui-experimental.cjs");

/** Evaluate the checked-in bundle the way the app does: CommonJS with the app's modules supplied. */
async function load(): Promise<{ required: string[]; exported: Record<string, unknown> }> {
  const source = await readFile(bundle, "utf8");
  const required: string[] = [];
  const module = { exports: {} as Record<string, unknown> };
  const inert: unknown = new Proxy(function inert() {}, {
    get: (_target, key) => (key === Symbol.toPrimitive ? () => "" : inert),
    apply: () => inert,
    construct: () => inert as object,
  });
  const require = (id: string) => {
    required.push(id);
    if (id === "@opencode/plugin/gui") {
      return {
        Plugin: { define: (definition: unknown) => definition },
        Store: { global: (schema: unknown, initial: unknown) => ({ scope: "global", schema, initial }) },
      };
    }
    return inert;
  };
  runInNewContext(source, { module, exports: module.exports, require, console });
  return { required, exported: module.exports };
}

test("the checked-in GUI bundle requires only modules the app provides", async () => {
  const { required } = await load();
  const provided = new Set<string>([...pluginSharedModules, ...pluginWindowModules]);
  expect(required.length).toBeGreaterThan(0);
  expect(required.filter((id) => !provided.has(id))).toEqual([]);
});

test("the bundle defines a window-only plugin with its stores and copy", async () => {
  const { exported } = await load();
  const definition = exported.default as Record<string, unknown>;
  expect(typeof definition.setup).toBe("function");
  expect(definition.main).toBeUndefined();
  expect(Object.keys(definition.stores as object)).toEqual(["prefs"]);
  expect((definition.i18n as { en: object }).en).toEqual(en);
});

test("every message the GUI reads exists in its English copy", async () => {
  const directory = join(root, "src/gui");
  const sources = await Promise.all((await readdir(directory)).map((file) => readFile(join(directory, file), "utf8")));
  const text = sources.join("\n");
  const keys = new Set<string>();
  for (const [, key] of text.matchAll(/ctx\.t\("([^"]+)"/g)) keys.add(key!);
  for (const [, key] of text.matchAll(/ctx\.plural\("([^"]+)"/g)) {
    keys.add(`${key}.one`);
    keys.add(`${key}.other`);
  }
  // Keys built from a value: every value they can take.
  for (const state of ["unknown", "idle", "working", "blocked", "done", "closed", "shell"]) keys.add(`state.${state}`);
  for (const name of ["notifyBlocked", "notifyFinished", "openSpawned", "fitWidth"]) {
    keys.add(`settings.${name}`);
    keys.add(`settings.${name}.description`);
  }
  for (const section of text.matchAll(/title: "(settings\.section\.[a-z]+)"/g)) keys.add(section[1]!);
  expect(keys.size).toBeGreaterThan(40);
  expect([...keys].filter((key) => !(key in en))).toEqual([]);
});
