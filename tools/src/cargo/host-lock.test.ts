import { describe, expect, test } from "bun:test";
import { mkdtempSync, readFileSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { ROOT } from "../lib/repo.ts";
import { compare, requiredDependencies, requiredPackage, type Finding } from "./host-lock.ts";

function lock(...packages: string[]): string {
  return "version = 4\n\n" + packages.map((pkg) => pkg.trim() + "\n").join("\n");
}

function registry(name: string, version: string): string {
  return `
[[package]]
name = "${name}"
version = "${version}"
source = "registry+https://github.com/rust-lang/crates.io-index"
checksum = "0" * 64
`;
}

function pathCrate(version: string, ...dependencies: string[]): string {
  const lines = dependencies.map((dependency) => ` "${dependency}",\n`).join("");
  return `
[[package]]
name = "root-path"
version = "${version}"
dependencies = [
${lines}]
`;
}

function fixed(finding: Finding | undefined, text: string): string {
  if (finding?.fix === undefined) throw new Error("expected a repairable finding");
  return finding.fix(text);
}

describe("host lock drift", () => {
  test("matched locks are accepted", () => {
    const root = lock(pathCrate("0.46.0", "sha2 0.11.0"), registry("sha2", "0.11.0"));
    expect(compare(root, root)).toEqual([]);
  });

  test("patch drift within a boundary is accepted", () => {
    const root = lock(pathCrate("0.46.0", "thiserror 2.0.21"), registry("thiserror", "2.0.21"));
    const host = lock(pathCrate("0.46.0", "thiserror 2.0.20"), registry("thiserror", "2.0.20"));
    expect(compare(root, host)).toEqual([]);
  });

  test("a moved boundary is reported and fixed", () => {
    const root = lock(pathCrate("0.46.0", "sha2 0.11.0"), registry("sha2", "0.11.0"));
    const host = lock(
      pathCrate("0.46.0", "sha2 0.10.9"),
      registry("sha2", "0.10.9"),
      registry("sha2", "0.11.0"),
    );
    const findings = compare(root, host);
    expect(findings).toHaveLength(1);
    expect(findings[0]?.message).toContain("sha2 0.10.9");
    expect(compare(root, fixed(findings[0], host))).toEqual([]);
  });

  test("a missing target version is reported without a fix", () => {
    const root = lock(pathCrate("0.46.0", "sha2 0.11.0"), registry("sha2", "0.11.0"));
    const host = lock(pathCrate("0.46.0", "sha2 0.10.9"), registry("sha2", "0.10.9"));
    const findings = compare(root, host);
    expect(findings).toHaveLength(1);
    expect(findings[0]?.fix).toBeUndefined();
  });

  test("path crate version drift is reported and fixed", () => {
    const root = lock(pathCrate("0.46.0"), registry("sha2", "0.11.0"));
    const host = lock(pathCrate("0.45.0"), registry("sha2", "0.11.0"));
    const findings = compare(root, host);
    expect(findings).toHaveLength(1);
    expect(findings[0]?.message).toContain("0.45.0");
    expect(compare(root, fixed(findings[0], host))).toEqual([]);
  });

  test("a dev-only dependency difference is ignored", () => {
    const root = lock(
      pathCrate("0.46.0", "sha2 0.11.0", "criterion 0.7.0"),
      registry("sha2", "0.11.0"),
      registry("criterion", "0.7.0"),
    );
    const host = lock(pathCrate("0.46.0", "sha2 0.11.0"), registry("sha2", "0.11.0"));
    expect(compare(root, host)).toEqual([]);
  });

  test("a missing required edge is reported and fixed", () => {
    // The #904 shape: a root crate gains a dependency whose package the host
    // lock already carries through another crate, but not as this edge.
    const packages = [
      registry("chrono", "0.4.44"),
      registry("nix", "0.31.2"),
      registry("serde", "1.0.228"),
    ];
    const root = lock(pathCrate("0.46.0", "chrono", "nix", "serde"), ...packages);
    const host = lock(pathCrate("0.46.0", "chrono", "serde"), ...packages);
    const required = new Map([["root-path", new Set(["chrono", "nix", "serde"])]]);
    const findings = compare(root, host, required);
    expect(findings).toHaveLength(1);
    expect(findings[0]?.message).toContain("depends on nix");
    const repaired = fixed(findings[0], host);
    expect(repaired).toContain(' "chrono",\n "nix",\n "serde",\n');
    expect(compare(root, repaired, required)).toEqual([]);
  });

  test("a missing edge to an unresolved package needs a refresh", () => {
    const root = lock(pathCrate("0.46.0", "nix"), registry("nix", "0.31.2"));
    const host = lock(pathCrate("0.46.0"));
    const findings = compare(root, host, new Map([["root-path", new Set(["nix"])]]));
    expect(findings).toHaveLength(1);
    expect(findings[0]?.fix).toBeUndefined();
  });

  test("an unrequired root-only edge is ignored", () => {
    // Optional (feature-gated) and dev edges are in the root lock's union only.
    const root = lock(pathCrate("0.46.0", "sha2 0.11.0", "uniffi"), registry("sha2", "0.11.0"));
    const host = lock(pathCrate("0.46.0", "sha2 0.11.0"), registry("sha2", "0.11.0"));
    expect(compare(root, host, new Map([["root-path", new Set(["sha2"])]]))).toEqual([]);
  });
});

/** A lock directory whose workspace has no members, so nothing is required. */
function fixture(rootText: string, hostText: string): string {
  const dir = mkdtempSync(join(tmpdir(), "host-lock-"));
  writeFileSync(join(dir, "Cargo.toml"), '[workspace]\nmembers = ["crates/*"]\n');
  writeFileSync(join(dir, "Cargo.lock"), rootText);
  writeFileSync(join(dir, "host.lock"), hostText);
  return dir;
}

function cli(dir: string, ...args: string[]): { code: number; stdout: string; stderr: string } {
  const result = Bun.spawnSync(
    ["bun", join(ROOT, "tools/main.ts"), "desktop-host-lock", ...args, "Cargo.lock", "host.lock"],
    { cwd: dir, env: { ...process.env, NO_COLOR: "1" }, stdout: "pipe", stderr: "pipe" },
  );
  return {
    code: result.exitCode,
    stdout: result.stdout.toString(),
    stderr: result.stderr.toString(),
  };
}

describe("host lock output", () => {
  test("repair writes only a changed lock and reports on stderr", () => {
    const root = lock(pathCrate("0.46.0"));
    const dir = fixture(root, lock(pathCrate("0.45.0")));
    const repaired = cli(dir, "--fix");
    expect(repaired.code).toBe(0);
    expect(repaired.stdout).toBe("");
    expect(repaired.stderr).toStartWith("phux: ok: fixed 1 entry in host.lock\n");
    expect(compare(root, readFileSync(join(dir, "host.lock"), "utf8"))).toEqual([]);
    // No drift means no write and no misleading repair receipt.
    expect(cli(dir, "--fix").stderr).toBe("phux: ok: host.lock is in step with Cargo.lock\n");
  });

  test("findings choose repair or refresh without stdout", () => {
    const root = lock(pathCrate("0.46.0", "sha2 0.11.0"), registry("sha2", "0.11.0"));
    const host = lock(pathCrate("0.45.0", "sha2 0.10.9"), registry("sha2", "0.10.9"));
    const result = cli(fixture(root, host));
    expect(result.code).toBe(1);
    expect(result.stdout).toBe("");
    expect(result.stderr).toContain("phux: error: host.lock:");
    expect(result.stderr).toContain("  hint: repair with: just desktop-lock-fix");
    expect(result.stderr).toContain("  hint: refresh the host lock: just desktop-lock-refresh");
  });

  test("more than two locks is a usage error", () => {
    const result = Bun.spawnSync(
      ["bun", join(ROOT, "tools/main.ts"), "desktop-host-lock", "a", "b", "c"],
      {
        stderr: "pipe",
      },
    );
    expect(result.exitCode).toBe(2);
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
