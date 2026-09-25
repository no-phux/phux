import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { resolve } from "node:path";

// Run after napi build. Its Rust macro output includes linked dependencies,
// so this compares the whole GPUIX + host boundary, not a handwritten subset.
const generated = resolve(import.meta.dirname, "../.cache/host/index.d.ts");
const committed = resolve(import.meta.dirname, "generated/index.d.ts");
assert.ok(
  readFileSync(generated, "utf8") === readFileSync(committed, "utf8"),
  "Native declarations drifted: rebuild release with napi, review and copy .cache/host/index.d.ts to native/generated/index.d.ts",
);
console.log("PASS: combined GPUIX + desktop native declarations are fresh");
