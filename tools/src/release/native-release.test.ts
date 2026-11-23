import { afterAll, expect, test } from "bun:test";
import { mkdirSync, mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { tmpdir } from "node:os";
import { WORKSPACE_LOCKS } from "../cargo/release-orchestration.ts";
import { git } from "../lib/git.ts";
import { classifyDetection, detect } from "../routing/detect.ts";
import { artifactName, findValidation, type Receipt, type Source } from "../routing/receipt.ts";
import { runeMacosPlan } from "../routing/rune-macos.ts";
import { plan, planOutputs } from "../routing/plan.ts";
import { applyRuneGate, nativeReleaseProof, releaseRefusal } from "./publish-plan.ts";

const repo = mkdtempSync(join(tmpdir(), "phux-native-release-"));
const events = mkdtempSync(join(tmpdir(), "phux-native-events-"));
git(["init", "--quiet"], repo);
git(["config", "user.email", "native@example.invalid"], repo);
git(["config", "user.name", "Native proof fixture"], repo);
git(["config", "core.hooksPath", "/dev/null"], repo);

function commit(files: Record<string, string>): string {
  for (const [path, text] of Object.entries(files)) {
    mkdirSync(dirname(join(repo, path)), { recursive: true });
    writeFileSync(join(repo, path), text);
  }
  git(["add", "--all"], repo);
  git(["commit", "--quiet", "-m", "fixture"], repo);
  return git(["rev-parse", "HEAD"], repo).trim();
}

const metadata = (version: string) => ({
  "Cargo.toml": `[workspace.package]\nversion = "${version}"\n`,
  ...Object.fromEntries(
    WORKSPACE_LOCKS.map((path) => [
      path,
      `version = 3\n[[package]]\nname = "phux"\nversion = "${version}"\n`,
    ]),
  ),
});
const base = commit(metadata("0.1.0"));
const a = commit({ "crates/phux-client-runtime/src/lib.rs": "// native change\n" });
const b = commit({ "docs/fixture.md": "Documentation only.\n" });
const c = commit(metadata("0.2.0"));

afterAll(() => {
  rmSync(repo, { recursive: true, force: true });
  rmSync(events, { recursive: true, force: true });
});

function detection(before: string, after: string) {
  const path = join(events, `${before}-${after}.json`);
  writeFileSync(path, JSON.stringify({ before, after }));
  return detect("push", path, repo);
}

function receipt(sha: string): Receipt {
  return {
    schema: 2,
    tree: git(["rev-parse", `${sha}^{tree}`], repo).trim(),
    workflow: ".github/workflows/rune-macos.yml",
    coverage: "macos,rune_ios",
  };
}

function proof(sha: string, state: string, built = sha) {
  const candidate = receipt(sha);
  const saved = built === sha ? candidate : receipt(built);
  const run = {
    event: "push",
    head_branch: "main",
    head_sha: built,
    path: saved.workflow,
    conclusion: state === "absent" ? "success" : state,
    head_repository: { full_name: "owner/project" },
    repository: { full_name: "owner/project" },
  };
  const source: Source = {
    json(path) {
      if (path.endsWith("/runs/3")) return run;
      if (path.includes("/git/commits/")) return { tree: { sha: saved.tree } };
      return {
        artifacts:
          state === "absent"
            ? []
            : [
                {
                  workflow_run: { id: 3 },
                  expired: false,
                  size_in_bytes: 400,
                },
              ],
      };
    },
    download(runId, name, dir) {
      expect(runId).toBe("3");
      expect(name).toBe(artifactName(candidate));
      writeFileSync(join(dir, "receipt.json"), JSON.stringify(saved));
    },
  };
  const qualified = findValidation(candidate, "owner/project", source, sha) !== undefined;
  return nativeReleaseProof(sha, String(run.conclusion), qualified);
}

function publish(sha: string, native: ReturnType<typeof proof>) {
  return applyRuneGate(
    { publish: ["v1.2.3"], delete: [], wait: false, fail: undefined, blocked: "" },
    sha,
    () => native,
    () => sha,
  );
}

test("metadata C qualifies Apple and preserves main's native/icount policy without changing product routing", () => {
  const change = detection(b, c);
  expect(change.kind).toBe("release-metadata");
  expect(classifyDetection(change).needed.size).toBe(0);
  const classification = classifyDetection(change);
  const options = { pullRequestFast: true, scope: { kind: "none" as const } };
  expect([...plan(classification, "pull_request", options).lanes]).toEqual(["guards"]);
  expect(planOutputs(plan(classification, "push", options), classification)).toMatchObject({
    native: true,
    icount: true,
    rune: false,
  });
  expect(runeMacosPlan(classifyDetection(change, "qualify-rune"), "push")).toEqual({
    macos: true,
    ios: true,
    coverage: "macos,rune_ios",
  });
  expect(runeMacosPlan(classifyDetection(change, "qualify-rune"), "pull_request").coverage).toBe(
    "",
  );
});

test.each(["pending", "failure", "cancelled", "absent"])(
  "A native change, B docs, C metadata cannot erase %s native proof",
  (state) => {
    expect(git(["rev-parse", `${c}^`], repo).trim()).toBe(b);
    expect(git(["rev-parse", `${b}^`], repo).trim()).toBe(a);
    const native = proof(c, state);
    expect(native.evidence).toBe(c);
    expect(releaseRefusal("success", c, native.state)).toBeDefined();
    expect(publish(c, native).publish).toEqual([]);
  },
);

test("a native A..B push qualifies even though head B only changes docs", () => {
  const actualPush = classifyDetection(detection(base, b), "qualify-rune");
  expect(runeMacosPlan(actualPush, "push")).toMatchObject({ macos: true, ios: true });
  // Looking only at B's parent diff would see no Rune inputs.
  expect(classifyDetection(detection(a, b)).needed.has("rune")).toBe(false);
});

test.each(["pending", "failure", "cancelled", "absent"])(
  "a docs-only head B cannot erase %s native proof",
  (state) => {
    const native = proof(b, state);
    expect(releaseRefusal("success", b, native.state)).toBeDefined();
    expect(publish(b, native).publish).toEqual([]);
  },
);

test("only the fully qualified release SHA, not ancestor proof, can ship", () => {
  for (const head of [b, c]) {
    const native = proof(head, "success");
    expect(releaseRefusal("success", head, native.state)).toBeUndefined();
    expect(publish(head, native).publish).toEqual(["v1.2.3"]);
    const ancestor = proof(head, "success", a);
    expect(releaseRefusal("success", head, ancestor.state)).toBeDefined();
    expect(publish(head, ancestor).publish).toEqual([]);
  }
});
