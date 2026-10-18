import { describe, expect, test } from "bun:test";
import { mkdtempSync, readFileSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { syncPathVersions } from "./lock.ts";
import { syncLocks } from "./sync.ts";

function lock(version: string): string {
  return `version = 4

[[package]]
name = "phux-protocol"
version = "${version}"

[[package]]
name = "phux-edge"
version = "0.0.0"

[[package]]
name = "registry-package"
version = "0.46.0"
source = "registry+https://github.com/rust-lang/crates.io-index"
`;
}

describe("sync-path-lock-versions", () => {
  test("only shared path versions change", () => {
    const after = syncPathVersions(
      lock("0.46.0"),
      new Map([
        ["phux-protocol", "0.48.0"],
        ["registry-package", "0.48.0"],
      ]),
    );
    expect(after).toBe(lock("0.48.0"));
    expect(syncPathVersions(after, new Map([["phux-protocol", "0.48.0"]]))).toBe(after);
  });

  test("check reports without mutation and fix is idempotent", () => {
    const dir = mkdtempSync(join(tmpdir(), "sync-lock-"));
    writeFileSync(join(dir, "root.lock"), lock("0.48.0"));
    writeFileSync(join(dir, "edge.lock"), lock("0.46.0"));
    const edge = (): string => readFileSync(join(dir, "edge.lock"), "utf8");
    expect(syncLocks("root.lock", ["edge.lock"], true, dir)).toBe(false);
    expect(edge()).toBe(lock("0.46.0"));
    expect(syncLocks("root.lock", ["edge.lock"], false, dir)).toBe(true);
    expect(syncLocks("root.lock", ["edge.lock"], true, dir)).toBe(true);
    expect(edge()).toBe(lock("0.48.0"));
  });
});
