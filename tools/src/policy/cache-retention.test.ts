// Build-cache retention against a fixture store and a fixed clock. The
// store mimics GitHub's listing: main's caches for one prefix, newest first.
import { afterEach, beforeEach, expect, spyOn, test, type Mock } from "bun:test";
import { repoPath } from "../lib/repo.ts";
import {
  cacheEntry,
  LIST_QUERY,
  listEndpoint,
  prune,
  SWEEP,
  type CacheStore,
} from "../mbx/prune.ts";

const PREFIX = "linux-arm64-mbx-phux-test-";
const MAIN = "refs/heads/main";
const NOW = new Date("2026-11-14T09:30:00Z");

interface Raw {
  id: number;
  key: string;
  created_at: string;
  ref: string;
}

function entry(id: number, day: string, ref = MAIN): Raw {
  return {
    id,
    key: `${PREFIX}flake-lock-${day.replaceAll("-", "")}`,
    created_at: `${day}T00:00:00Z`,
    ref,
  };
}

let log: Mock<typeof console.log>;
beforeEach(() => {
  log = spyOn(console, "log").mockImplementation(() => {});
});
afterEach(() => log.mockRestore());

function sweep(entries: Raw[], options: { dry?: boolean; failList?: boolean } = {}) {
  const deleted: string[] = [];
  const store: CacheStore = {
    list(prefix) {
      if (options.failList) throw new Error("listing failed");
      return entries
        .filter((raw) => raw.key.startsWith(prefix) && raw.ref === MAIN)
        .toSorted((left, right) => right.created_at.localeCompare(left.created_at))
        .map((raw) => ({ id: String(raw.id), key: raw.key, created: raw.created_at }));
    },
    remove: (id) => void deleted.push(id),
  };
  let failed = false;
  try {
    prune(store, NOW, options.dry ?? false);
  } catch {
    failed = true;
  }
  return { failed, deleted };
}

test("an absent/failed replacement leaves the newest old restore source", () => {
  expect(sweep([entry(1, "2026-11-13"), entry(2, "2026-11-12")])).toEqual({
    failed: false,
    deleted: ["2"],
  });
  expect(log).toHaveBeenCalledWith(`keeping newest restore ${PREFIX}flake-lock-20261113`);
});

test("a published current-day replacement retires old copies", () => {
  expect(sweep([entry(1, "2026-11-13"), entry(2, "2026-11-14")]).deleted).toEqual(["1"]);
  expect(log).toHaveBeenCalledWith(`keeping current-day ${PREFIX}flake-lock-20261114`);
});

test("a PR cache cannot qualify replacement or be deleted by main retention", () => {
  const result = sweep([entry(1, "2026-11-13"), entry(2, "2026-11-14", "refs/pull/7/merge")]);
  expect(result.deleted).toEqual([]);
  expect(listEndpoint("fixture/repo", PREFIX)).toBe(
    `repos/fixture/repo/actions/caches?key=${PREFIX}&ref=refs/heads/main&sort=created_at&direction=desc&per_page=100`,
  );
});

test("all current-day entries remain protected", () => {
  // A key stamped with an older day still counts when it was created today.
  const today = { ...entry(2, "2026-11-14"), key: `${PREFIX}different-lock` };
  const stamped = { ...entry(3, "2026-11-01"), key: `${PREFIX}lock-20261114` };
  expect(sweep([entry(1, "2026-11-14"), today, stamped]).deleted).toEqual([]);
});

test("unrelated caches and other refs are outside the deletion scope", () => {
  const unrelated = { ...entry(3, "2026-11-12"), key: "bun-dependencies" };
  const result = sweep([
    entry(1, "2026-11-13"),
    entry(2, "2026-11-14"),
    unrelated,
    entry(4, "2026-11-12", "refs/heads/topic"),
  ]);
  expect(result.deleted).toEqual(["1"]);
});

test("dry-run and failed listing cannot delete caches", () => {
  const entries = [entry(1, "2026-11-13"), entry(2, "2026-11-14")];
  expect(sweep(entries, { dry: true }).deleted).toEqual([]);
  expect(log).toHaveBeenCalledWith(`would delete ${PREFIX}flake-lock-20261113 (1)`);
  expect(sweep(entries, { failList: true })).toEqual({ failed: true, deleted: [] });
});

test("pre-upload replacement mode is rejected rather than deleting the sole restore copy", () => {
  const proc = Bun.spawnSync(
    ["bun", "tools/main.ts", "prune-build-cache", "--replace-prefix", PREFIX],
    {
      cwd: repoPath("."),
      // A fake repository and no token: a regression could not reach GitHub.
      env: {
        ...process.env,
        GITHUB_REPOSITORY: "fixture/repo",
        GH_TOKEN: "",
        GH_HOST: "fixture.invalid",
      },
      stdout: "pipe",
      stderr: "pipe",
    },
  );
  expect(proc.exitCode).toBe(2);
});

test("the sweep names each series once", () => {
  expect(new Set(SWEEP).size).toBe(SWEEP.length);
});

test("the production listing query keeps only main's caches and parses each line", () => {
  // The fixture store above filters refs itself; this runs the real `--jq`
  // filter (gh evaluates it with jq semantics) over one listing page.
  const page = JSON.stringify({
    actions_caches: [
      { id: 7, key: `${PREFIX}a`, created_at: "2026-11-14T00:00:00Z", ref: MAIN },
      { id: 8, key: `${PREFIX}b`, created_at: "2026-11-13T00:00:00Z", ref: "refs/pull/9/merge" },
    ],
  });
  const proc = Bun.spawnSync(["jq", "-c", LIST_QUERY], {
    stdin: Buffer.from(page),
    stdout: "pipe",
  });
  expect(proc.exitCode).toBe(0);
  const lines = proc.stdout
    .toString()
    .split("\n")
    .filter((line) => line.trim() !== "");
  expect(lines.map((line) => cacheEntry(JSON.parse(line)))).toEqual([
    { id: "7", key: `${PREFIX}a`, created: "2026-11-14T00:00:00Z" },
  ]);
});
