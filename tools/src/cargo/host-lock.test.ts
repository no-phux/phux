import { describe, expect, test } from "bun:test";
import { mkdtempSync, readFileSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { ROOT } from "../lib/repo.ts";
import { applyFixes, compare, requiredDependencies, requiredPackage } from "./host-lock.ts";

const lock = (...packages: string[]) => `version = 4\n${packages.join("")}`;
const registry = (name: string, version: string) =>
  `\n[[package]]\nname = "${name}"\nversion = "${version}"\nsource = "registry+https://x"\n`;
const pathCrate = (version: string, ...dependencies: string[]) =>
  `\n[[package]]\nname = "root-path"\nversion = "${version}"\ndependencies = [\n` +
  `${dependencies.map((dependency) => ` "${dependency}",\n`).join("")}]\n`;
const sha2 = (version: string) => registry("sha2", version);
const extra = [
  registry("chrono", "0.4.44"),
  registry("nix", "0.31.2"),
  registry("serde", "1.0.228"),
];

// [case, root lock, host lock, required, expected findings as [message part, repairable]]
const cases: [string, string, string, string[], [string, boolean][]][] = [
  ["matched", lock(pathCrate("0.46.0", "sha2 0.11.0"), sha2("0.11.0")), "", [], []],
  [
    "patch drift within a boundary",
    lock(pathCrate("0.46.0", "thiserror 2.0.21"), registry("thiserror", "2.0.21")),
    lock(pathCrate("0.46.0", "thiserror 2.0.20"), registry("thiserror", "2.0.20")),
    [],
    [],
  ],
  [
    "a moved boundary",
    lock(pathCrate("0.46.0", "sha2 0.11.0"), sha2("0.11.0")),
    lock(pathCrate("0.46.0", "sha2 0.10.9"), sha2("0.10.9"), sha2("0.11.0")),
    [],
    [["selects sha2 0.10.9", true]],
  ],
  [
    "a moved boundary to a version the host lacks",
    lock(pathCrate("0.46.0", "sha2 0.11.0"), sha2("0.11.0")),
    lock(pathCrate("0.46.0", "sha2 0.10.9"), sha2("0.10.9")),
    [],
    [["selects sha2 0.10.9", false]],
  ],
  [
    "path crate version drift",
    lock(pathCrate("0.46.0"), sha2("0.11.0")),
    lock(pathCrate("0.45.0"), sha2("0.11.0")),
    [],
    [["root-path is 0.45.0", true]],
  ],
  [
    "a dev-only or optional root edge",
    lock(pathCrate("0.46.0", "sha2 0.11.0", "criterion 0.7.0", "uniffi"), sha2("0.11.0")),
    lock(pathCrate("0.46.0", "sha2 0.11.0"), sha2("0.11.0")),
    ["sha2"],
    [],
  ],
  [
    // The #904 shape: the host lock carries the package, not the edge.
    "a missing required edge",
    lock(pathCrate("0.46.0", "chrono", "nix", "serde"), ...extra),
    lock(pathCrate("0.46.0", "chrono", "serde"), ...extra),
    ["chrono", "nix", "serde"],
    [["depends on nix", true]],
  ],
  [
    "a missing edge to an unresolved package",
    lock(pathCrate("0.46.0", "nix"), registry("nix", "0.31.2")),
    lock(pathCrate("0.46.0")),
    ["nix"],
    [["depends on nix", false]],
  ],
];

describe("host lock drift", () => {
  test.each(cases)("%s", (_, root, host, names, expected) => {
    const required = new Map([["root-path", new Set(names)]]);
    const findings = compare(root, host || root, required);
    expect(findings.map(({ fix }) => fix !== undefined)).toEqual(expected.map(([, ok]) => ok));
    for (const [i, [part]] of expected.entries()) expect(findings[i]?.message).toContain(part);
    if (!expected.every(([, fixable]) => fixable)) return;
    const repaired = applyFixes(host || root, findings);
    expect(compare(root, repaired, required)).toEqual([]);
    if (names.includes("nix")) expect(repaired).toContain(' "chrono",\n "nix",\n "serde",\n');
  });
});

function cli(dir: string, ...args: string[]) {
  const main = join(ROOT, "tools/main.ts");
  const result = Bun.spawnSync(["bun", main, "desktop-host-lock", ...args], {
    cwd: dir,
    env: { ...process.env, NO_COLOR: "1" },
  });
  return {
    code: result.exitCode,
    stdout: result.stdout.toString(),
    stderr: result.stderr.toString(),
  };
}

/** A lock directory whose workspace has no members, so nothing is required. */
function fixture(rootText: string, hostText: string): string {
  const dir = mkdtempSync(join(tmpdir(), "host-lock-"));
  writeFileSync(join(dir, "Cargo.toml"), '[workspace]\nmembers = ["crates/*"]\n');
  writeFileSync(join(dir, "Cargo.lock"), rootText);
  writeFileSync(join(dir, "host.lock"), hostText);
  return dir;
}

describe("desktop-host-lock", () => {
  test("--fix writes only a changed lock and reports on stderr", () => {
    const root = lock(pathCrate("0.46.0"));
    const dir = fixture(root, lock(pathCrate("0.45.0")));
    expect(cli(dir, "--fix", "Cargo.lock", "host.lock")).toEqual({
      code: 0,
      stdout: "",
      stderr:
        "phux: ok: fixed 1 entry in host.lock\nphux: ok: host.lock is in step with Cargo.lock\n",
    });
    expect(compare(root, readFileSync(join(dir, "host.lock"), "utf8"))).toEqual([]);
    expect(cli(dir, "--fix", "Cargo.lock", "host.lock").stderr).toBe(
      "phux: ok: host.lock is in step with Cargo.lock\n",
    );
  });

  test("findings choose repair or refresh without stdout", () => {
    const root = lock(pathCrate("0.46.0", "sha2 0.11.0"), sha2("0.11.0"));
    const host = lock(pathCrate("0.45.0", "sha2 0.10.9"), sha2("0.10.9"));
    const result = cli(fixture(root, host), "Cargo.lock", "host.lock");
    expect(result.code).toBe(1);
    expect(result.stdout).toBe("");
    expect(result.stderr).toContain("phux: error: host.lock:");
    expect(result.stderr).toContain("  hint: repair with: just desktop-lock-fix");
    expect(result.stderr).toContain("  hint: refresh the host lock: just desktop-lock-refresh");
  });

  test("more than two locks is a usage error", () => {
    expect(cli(ROOT, "a", "b", "c").code).toBe(2);
  });
});

describe("required dependencies", () => {
  test("manifest entries resolve to package names", () => {
    const inherited = { usage: { package: "usage-rs", version: "6" }, serde: "1" };
    const cases: [string, unknown, string | undefined][] = [
      ["serde", "1", "serde"],
      ["serde", { workspace: true, features: ["derive"] }, "serde"],
      ["usage", { workspace: true }, "usage-rs"],
      ["alias", { package: "real", version: "1" }, "real"],
      ["uniffi", { version: "0.32", optional: true }, undefined],
      ["serde", { workspace: true, optional: true }, undefined],
    ];
    for (const [key, spec, expected] of cases) {
      expect(requiredPackage(key, spec, inherited)).toBe(expected);
    }
  });

  test("repository manifests require unconditional dependencies only", () => {
    const required = requiredDependencies(ROOT);
    expect(required.get("phux-config")?.has("nix")).toBe(true);
    expect(required.get("phux-config")?.has("insta")).toBe(false); // dev-dependency
    expect(required.get("phux-client-ffi")?.has("uniffi")).toBe(false); // optional
  });
});
