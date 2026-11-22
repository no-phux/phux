import { expect, test } from "bun:test";
import { mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { git } from "../lib/git.ts";
import { job, run, steps, workflow } from "./workflows.ts";

test("commitlint ignores real integration merges, never authored messages that start with Merge", () => {
  const root = mkdtempSync(join(tmpdir(), "phux-commitlint-"));
  try {
    git(["init", "--quiet", "--initial-branch=main"], root);
    git(["config", "user.name", "Commitlint Test"], root);
    git(["config", "user.email", "commitlint@example.invalid"], root);
    git(["config", "core.hooksPath", "/dev/null"], root);
    git(["config", "commit.gpgsign", "false"], root);
    git(["commit", "--quiet", "--allow-empty", "-m", "chore: bootstrap"], root);
    const base = git(["rev-parse", "HEAD"], root).trim();
    git(["checkout", "--quiet", "-b", "side"], root);
    git(["commit", "--quiet", "--allow-empty", "-m", "fix(ci): side change"], root);
    git(["checkout", "--quiet", "main"], root);
    git(["commit", "--quiet", "--allow-empty", "-m", "fix(ci): main change"], root);
    git(["merge", "--quiet", "--no-ff", "side", "-m", "Merge origin/main; integration only"], root);
    mkdirSync(join(root, ".github"));
    writeFileSync(join(root, ".github/cockpit-history-tip"), `${base}\n`);
    const bin = join(root, "bin");
    mkdirSync(bin);
    const messages = join(root, "messages");
    writeFileSync(
      join(bin, "npx"),
      '#!/usr/bin/env bash\nmessage=$(</dev/stdin)\nprintf \'%s\\n\' "$message" >> "$COMMITLINT_INPUTS"\n[[ "$message" == \'fix(ci): \'* ]]\n',
      { mode: 0o755 },
    );
    const script = run(
      steps(job(workflow("commitlint.yml"), "commitlint")).find(
        (step) => step["name"] === "Lint PR commits",
      ) ?? {},
    );
    const check = () =>
      Bun.spawnSync(["bash", "-euo", "pipefail", "-c", script], {
        cwd: root,
        env: {
          ...process.env,
          PATH: `${bin}:${process.env["PATH"] ?? ""}`,
          BASE_SHA: base,
          HEAD_SHA: git(["rev-parse", "HEAD"], root).trim(),
          COMMITLINT_INPUTS: messages,
        },
      });
    expect(check().exitCode).toBe(0);
    expect(readFileSync(messages, "utf8").trim().split("\n").toSorted()).toEqual([
      "fix(ci): main change",
      "fix(ci): side change",
    ]);
    git(
      [
        "commit",
        "--quiet",
        "--allow-empty",
        "-m",
        "Merge is not an excuse for a malformed authored commit",
      ],
      root,
    );
    expect(check().exitCode).not.toBe(0);
    expect(readFileSync(messages, "utf8")).toContain("Merge is not an excuse");
  } finally {
    rmSync(root, { recursive: true, force: true });
  }
});
