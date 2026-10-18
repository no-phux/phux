// The release drift check, against a fake `gh` and `git` on PATH.
import { beforeEach, describe, expect, test } from "bun:test";
import { chmodSync, mkdtempSync, readFileSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { repoPath } from "../lib/repo.ts";
import { str, table } from "../lib/value.ts";

const FAKE_GH = `#!${process.execPath}
const state = JSON.parse(require("node:fs").readFileSync(process.env.DRIFT_STATE, "utf8"));
const args = process.argv.slice(2);
if (args[0] === "release" && args[1] === "download") {
  console.log(JSON.stringify(state.stable));
} else if (args[1].endsWith("/releases")) {
  for (const row of state.releases) console.log(JSON.stringify(row));
} else if (!args[1].includes("/issues?")) {
  console.error("unexpected gh call: " + JSON.stringify(args));
  process.exit(1);
}
`;

interface Release {
  tag: string;
  draft: boolean;
  prerelease: boolean;
  assets: number;
  created: string;
}

const release = (tag: string, draft = false, assets = 2): Release => ({
  tag,
  draft,
  prerelease: false,
  assets,
  created: "2020-01-01T00:00:00Z",
});

describe("release drift", () => {
  let bin: string;
  let releases: Release[];
  let stable: unknown;

  beforeEach(() => {
    bin = mkdtempSync(join(tmpdir(), "release-drift-"));
    writeFileSync(join(bin, "gh"), FAKE_GH);
    writeFileSync(join(bin, "git"), "#!/bin/sh\necho 2020-01-01T00:00:00Z\n");
    for (const name of ["gh", "git"]) chmodSync(join(bin, name), 0o755);
    const read = (path: string): Record<string, unknown> =>
      table(JSON.parse(readFileSync(repoPath(path), "utf8")));
    const packages = table(read("release-please-config.json")["packages"]);
    const manifest = read(".release-please-manifest.json");
    releases = Object.entries(manifest).map(([path, version]) => {
      const component = str(table(packages[path])["component"]);
      return release(component ? `${component}-v${String(version)}` : `v${String(version)}`);
    });
    stable = {
      products: {
        phux: { version: manifest["."] },
        desktop: { version: manifest["clients/desktop"] },
      },
    };
  });

  function drift(): { code: number; stdout: string; stderr: string } {
    const state = join(bin, "state.json");
    writeFileSync(state, JSON.stringify({ releases, stable }));
    const result = Bun.spawnSync([process.execPath, repoPath("tools/main.ts"), "release-drift"], {
      cwd: repoPath("."),
      stdout: "pipe",
      stderr: "pipe",
      env: {
        ...process.env,
        PATH: `${bin}:${process.env["PATH"] ?? ""}`,
        GITHUB_ACTIONS: "true",
        GITHUB_REPOSITORY: "example/repo",
        GRACE_MINUTES: "0",
        DRIFT_STATE: state,
      },
    });
    return {
      code: result.exitCode ?? -1,
      stdout: result.stdout.toString(),
      stderr: result.stderr.toString(),
    };
  }

  function expectStuck(message: string): void {
    const result = drift();
    expect(result.code).toBe(1);
    expect(result.stderr).toContain(message);
  }

  test("a finished pipeline passes", () => {
    const result = drift();
    expect(result.stderr).toBe("");
    expect(result.code).toBe(0);
    expect(result.stdout).toContain("release drift check passed");
  });

  test("an old draft names its recovery", () => {
    releases.push(release("desktop-v99.0.0", true));
    expectStuck("gh workflow run publish.yml --repo example/repo -f tag=desktop-v99.0.0");
  });

  test("a public release needs assets", () => {
    releases.push(release("v0.0.9", false, 0));
    expectStuck("v0.0.9 is published with no assets");
  });

  test("every manifest version needs a release", () => {
    const missing = releases.pop();
    expectStuck(`${missing?.tag} has no release`);
  });

  test("the stable manifest names the newest release", () => {
    releases.push(release("v99.0.0"));
    expectStuck("but v99.0.0 is published");
  });
});
