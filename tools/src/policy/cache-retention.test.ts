// The real retention script, with an inert gh function and deterministic
// dates. DELETE is recorded only in this fixture's private directory.
import { expect, test } from "bun:test";
import { mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { repoPath } from "../lib/repo.ts";

const PREFIX = "linux-arm64-mbx-phux-test-";
const MAIN = "refs/heads/main";
const MOCK = `
date() {
  case "$*" in
    "-u +%Y%m%d") echo 20261114 ;;
    "-u +%Y-%m-%d") echo 2026-11-14 ;;
    *) return 2 ;;
  esac
}
sleep() { :; }
gh() {
  local endpoint="" query="" method="" prefix ref=""
  while [[ $# -gt 0 ]]; do
    case "$1" in
      api|--paginate|--silent) shift ;;
      -X) method="$2"; shift 2 ;;
      --jq) query="$2"; shift 2 ;;
      *) endpoint="$1"; shift ;;
    esac
  done
  if [[ "$method" = DELETE ]]; then
    printf '%s\\n' "\${endpoint##*/}" >> "$DELETES"
    return 0
  fi
  printf '%s\\n' "$endpoint" >> "$REQUESTS"
  prefix="\${endpoint#*key=}"
  prefix="\${prefix%%&*}"
  if [[ "$endpoint" == *"ref=refs/heads/main"* ]]; then ref=refs/heads/main; fi
  jq --arg prefix "$prefix" --arg ref "$ref" '
    map(select(.key | startswith($prefix)))
    | map(select($ref == "" or .ref == $ref))
    | sort_by(.created_at) | reverse | {actions_caches:.}
  ' "$ENTRIES" | jq -r "$query"
  test "$FAIL_LIST" != true
}
source "$SCRIPT" "$@"
`;

function entry(id: number, day: string, ref = MAIN) {
  return {
    id,
    key: `${PREFIX}flake-lock-${day.replaceAll("-", "")}`,
    created_at: `${day}T00:00:00Z`,
    ref,
  };
}

function sweep(entries: unknown[], args: string[] = [], failList = false) {
  const dir = mkdtempSync(join(tmpdir(), "phux-cache-retention-"));
  for (const [file, value] of [
    ["entries", JSON.stringify(entries)],
    ["deletes", ""],
    ["requests", ""],
  ]) {
    writeFileSync(join(dir, file ?? ""), value ?? "");
  }
  try {
    const result = Bun.spawnSync(["bash", "-c", MOCK, "fixture", ...args], {
      env: {
        ...process.env,
        GH_TOKEN: "",
        GITHUB_REPOSITORY: "fixture/repo",
        SCRIPT: repoPath("scripts/ci/prune-build-cache.sh"),
        ENTRIES: join(dir, "entries"),
        DELETES: join(dir, "deletes"),
        REQUESTS: join(dir, "requests"),
        FAIL_LIST: String(failList),
      },
      stdout: "pipe",
      stderr: "pipe",
      timeout: 10_000,
    });
    return {
      code: result.exitCode,
      deleted: readFileSync(join(dir, "deletes"), "utf8").trim().split("\n").filter(Boolean),
      requests: readFileSync(join(dir, "requests"), "utf8"),
    };
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
}

test("an absent/failed replacement leaves the newest old restore source", () => {
  const result = sweep([entry(1, "2026-11-13"), entry(2, "2026-11-12")]);
  expect(result.code).toBe(0);
  expect(result.deleted).toEqual(["2"]);
});

test("a published current-day replacement retires old copies", () => {
  const result = sweep([entry(1, "2026-11-13"), entry(2, "2026-11-14")]);
  expect(result.code).toBe(0);
  expect(result.deleted).toEqual(["1"]);
});

test("a PR cache cannot qualify replacement or be deleted by main retention", () => {
  const result = sweep([entry(1, "2026-11-13"), entry(2, "2026-11-14", "refs/pull/7/merge")]);
  expect(result.code).toBe(0);
  expect(result.deleted).toEqual([]);
  expect(result.requests).toContain("ref=refs/heads/main");
});

test("all current-day entries remain protected", () => {
  const today = entry(2, "2026-11-14");
  today.key = `${PREFIX}different-lock`;
  const result = sweep([entry(1, "2026-11-14"), today]);
  expect(result.code).toBe(0);
  expect(result.deleted).toEqual([]);
});

test("unrelated caches and other refs are outside the deletion scope", () => {
  const unrelated = { ...entry(3, "2026-11-12"), key: "bun-dependencies" };
  const result = sweep([
    entry(1, "2026-11-13"),
    entry(2, "2026-11-14"),
    unrelated,
    entry(4, "2026-11-12", "refs/heads/topic"),
  ]);
  expect(result.code).toBe(0);
  expect(result.deleted).toEqual(["1"]);
});

test("dry-run and failed listing cannot delete caches", () => {
  const entries = [entry(1, "2026-11-13"), entry(2, "2026-11-14")];
  expect(sweep(entries, ["--dry-run"]).deleted).toEqual([]);
  const failed = sweep(entries, [], true);
  expect(failed.code).not.toBe(0);
  expect(failed.deleted).toEqual([]);
});

test("pre-upload replacement mode is rejected rather than deleting the sole restore copy", () => {
  const result = sweep([entry(1, "2026-11-13")], ["--replace-prefix", PREFIX]);
  expect(result.code).not.toBe(0);
  expect(result.deleted).toEqual([]);
});
