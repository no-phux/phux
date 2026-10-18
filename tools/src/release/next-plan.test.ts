import { expect, test } from "bun:test";
import { mkdirSync, mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { git } from "../lib/git.ts";
import { needsBuild, reason } from "./next-plan.ts";

test.each<[string, string[], boolean]>([
  ["phux", ["crates/phux/src/main.rs"], true],
  ["phux", ["scripts/pack-release.sh"], true],
  ["phux", [".github/workflows/build-cli.yml"], true],
  ["phux", ["clients/desktop/native/src/main.rs"], false],
  ["phux", ["docs/RELEASING.md"], false],
  ["desktop", ["clients/desktop/native/src/main.rs"], true],
  ["desktop", [".github/workflows/desktop-release.yml"], true],
  ["desktop", ["crates/phux-protocol/src/lib.rs"], true],
  ["desktop", [".github/workflows/build-cli.yml"], false],
  ["desktop", ["integrations/pi/src/index.ts"], false],
])("%s for %p rebuilds: %p", (product, paths, rebuild) => {
  expect(reason(product as "phux" | "desktop", paths) !== undefined).toBe(rebuild);
});

test("out-of-order main CI cannot move next backwards or onto divergent history", () => {
  const root = mkdtempSync(join(tmpdir(), "phux-next-history-"));
  try {
    git(["init", "--quiet", "--initial-branch=main"], root);
    git(["config", "user.name", "Next Test"], root);
    git(["config", "user.email", "next@example.invalid"], root);
    git(["config", "core.hooksPath", "/dev/null"], root);
    git(["config", "commit.gpgsign", "false"], root);
    git(["commit", "--quiet", "--allow-empty", "-m", "chore: base"], root);
    const base = git(["rev-parse", "HEAD"], root).trim();
    mkdirSync(join(root, "crates/phux/src"), { recursive: true });
    writeFileSync(join(root, "crates/phux/src/main.rs"), "changed product\n");
    git(["add", "."], root);
    git(["commit", "--quiet", "-m", "fix: product"], root);
    const newer = git(["rev-parse", "HEAD"], root).trim();
    expect(needsBuild("phux", base, newer, root)).toBe(true);
    expect(needsBuild("phux", newer, base, root)).toBe(false);
    expect(needsBuild("phux", newer, newer, root)).toBe(false);
    git(["checkout", "--quiet", "-b", "other", base], root);
    git(["commit", "--quiet", "--allow-empty", "-m", "fix: other history"], root);
    const divergent = git(["rev-parse", "HEAD"], root).trim();
    expect(needsBuild("phux", newer, divergent, root)).toBe(false);
    expect(needsBuild("phux", "f".repeat(40), newer, root)).toBe(true);
  } finally {
    rmSync(root, { recursive: true, force: true });
  }
});
