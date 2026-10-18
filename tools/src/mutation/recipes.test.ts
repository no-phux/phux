// Exercise the real just recipe without installing or executing mutation tools.
import { expect, test } from "bun:test";
import { chmodSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { ROOT } from "../lib/repo.ts";

test("the mutation recipe preserves argument boundaries", () => {
  const directory = mkdtempSync(join(tmpdir(), "phux-mutation-recipe-"));
  try {
    const log = join(directory, "argv");
    // A `bun` on PATH that records its NUL-separated argv instead of running.
    const stub = join(directory, "bun");
    writeFileSync(
      stub,
      `#!/bin/sh\nfor arg in "$@"; do printf '%s\\0' "$arg"; done > "$MUTATION_ARGV_LOG"\n`,
    );
    chmodSync(stub, 0o755);
    const args = [
      "--re",
      "validate_ratio|match guard.*with true",
      "--list",
      "--file",
      "crates/**/*.rs",
      "--output",
      "reports with spaces",
    ];
    const result = Bun.spawnSync(["just", "mutation-rust", ...args], {
      cwd: ROOT,
      env: {
        ...process.env,
        PATH: `${directory}:${process.env["PATH"] ?? ""}`,
        MUTATION_ARGV_LOG: log,
      },
      stdout: "pipe",
      stderr: "pipe",
    });
    expect(result.exitCode, result.stderr.toString()).toBe(0);
    const argv = readFileSync(log, "utf8").split("\0").slice(0, -1);
    expect(argv).toEqual(["tools/main.ts", "mutation-run", ...args]);
  } finally {
    rmSync(directory, { recursive: true, force: true });
  }
});
