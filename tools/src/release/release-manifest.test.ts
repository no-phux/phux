// The channel manifest generator (ADR-0176).
import { describe, expect, test } from "bun:test";
import { mkdtempSync, readFileSync, unlinkSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { ROOT } from "../lib/repo.ts";
import { table } from "../lib/value.ts";
import { parseManifestArgs } from "./commands.ts";
import {
  assetDigests,
  assetName,
  bridgeRecord,
  CHANNELS,
  DESKTOP_ARCHIVES,
  entry,
  fixture,
  fixtures,
  ManifestError,
  newestStable,
  render,
  TARGETS,
  verifiedDist,
} from "./release-manifest.ts";

// Every line shape the POSIX installers' awk parser accepts.
const LINE =
  /^(\{|\s*\},?|\s*"[A-Za-z0-9_-]+": \{|\s*"[A-Za-z0-9_-]+": ("[A-Za-z0-9._:/+-]*"|[0-9]+),?)$/;

const row = (
  tag: string,
  options: { assets?: [string, string | null][]; draft?: boolean; prerelease?: boolean } = {},
): Record<string, unknown> => ({
  tag_name: tag,
  draft: options.draft ?? false,
  prerelease: options.prerelease ?? false,
  assets: (options.assets ?? []).map(([name, digest]) => ({ name, digest })),
});

describe("manifest", () => {
  test("desktop names cover both Linux architectures on both channels", () => {
    for (const channel of CHANNELS) {
      const desktop = table(table(fixture(channel)["products"])["desktop"]);
      const artifacts = table(desktop["artifacts"]);
      const identity = channel === "stable" ? "1.2.3" : `next.${"b".repeat(40)}`;
      for (const [target, suffix] of Object.entries(DESKTOP_ARCHIVES)) {
        const artifact = table(artifacts[target]);
        expect(artifact["name"]).toBe(`phux-desktop-${identity}-${suffix}`);
        expect(String(artifact["url"]).endsWith(`/${String(artifact["name"])}`)).toBe(true);
      }
      expect(Object.keys(artifacts).toSorted()).toEqual([...TARGETS.desktop].toSorted());
    }
  });

  test("a desktop entry requires every platform", () => {
    expect(() =>
      entry("desktop", "stable", "1.2.3", "a".repeat(40), "0.9", {
        "aarch64-apple-darwin": "d".repeat(64),
      }),
    ).toThrow("exactly");
  });

  test("fixtures are current and line-parseable", () => {
    expect(fixtures(true)).toBe(0);
    for (const channel of CHANNELS) {
      const text = render(fixture(channel));
      for (const line of text.trimEnd().split("\n")) expect(line).toMatch(LINE);
    }
  });

  test("a stale fixture names the command that rewrites it", () => {
    const root = mkdtempSync(join(tmpdir(), "release-manifest-"));
    expect(() => fixtures(true, root)).toThrow(
      "stale: scripts/fixtures/release-manifest/stable.json, scripts/fixtures/release-manifest/next.json; run bun tools/main.ts release-manifest fixtures",
    );
  });

  test("render refuses what the installers cannot parse", () => {
    const bad = fixture("stable");
    table(table(bad["products"])["phux"])["version"] = "1.2.3 evil";
    for (const document of [
      bad,
      { flag: true },
      { list: [1] },
      { empty: {} },
      { none: null },
      { real: 1.5 },
    ]) {
      expect(() => render(document)).toThrow(ManifestError);
    }
  });

  test("entry refuses malformed facts", () => {
    const digests = Object.fromEntries(TARGETS.phux.map((target) => [target, "e".repeat(64)]));
    const good = ["1.2.3", "a".repeat(40), "0.9"] as const;
    const call = (
      version: string,
      sha: string,
      protocol: string,
      given: Record<string, string>,
    ): unknown => entry("phux", "stable", version, sha, protocol, given);
    expect(call(...good, digests)).toBeDefined();
    expect(() => call("1.02.3", good[1], good[2], digests)).toThrow(ManifestError);
    expect(() => call(good[0], "A".repeat(40), good[2], digests)).toThrow(ManifestError);
    expect(() => call(good[0], good[1], "0.09", digests)).toThrow(ManifestError);
    expect(() => call(...good, { "aarch64-apple-darwin": "e".repeat(64) })).toThrow(ManifestError);
    const upper = Object.fromEntries(TARGETS.phux.map((target) => [target, "E".repeat(64)]));
    expect(() => call(...good, upper)).toThrow(ManifestError);
  });
});

describe("stable", () => {
  test("desktop stable requires uploaded assets for every target", () => {
    const names = TARGETS.desktop.map((target) =>
      assetName("desktop", "stable", "1.2.3", undefined, target),
    );
    const assets = names.map((name) => ({ name, digest: `sha256:${"d".repeat(64)}` }));
    expect(Object.keys(assetDigests(assets, names))).toHaveLength(TARGETS.desktop.length);
    expect(() => assetDigests(assets.slice(0, -1), names)).toThrow(ManifestError);
  });

  test("the newest published release per product", () => {
    const rows = [
      row("v0.9.0"),
      row("v0.10.0"),
      row("v0.11.0", { draft: true }),
      row("desktop-v0.3.0"),
      row("desktop-v0.20.0"),
      row("desktop-v0.1.0-alpha.17"),
      row("pi-extension-v9.0.0"),
      row("next", { prerelease: true }),
      row("channels", { prerelease: true }),
    ];
    const newest = newestStable(rows);
    expect(Object.fromEntries(Object.entries(newest).map(([p, r]) => [p, r["tag_name"]]))).toEqual({
      phux: "v0.10.0",
      desktop: "desktop-v0.20.0",
    });
  });

  test("digests come from GitHub and must exist", () => {
    const name = assetName("phux", "stable", "1.2.3", undefined, "x86_64-unknown-linux-gnu");
    expect(name).toBe("phux-v1.2.3-x86_64-unknown-linux-gnu.tar.gz");
    const assets = [{ name, digest: `sha256:${"e".repeat(64)}` }];
    expect(assetDigests(assets, [name])).toEqual({ [name]: "e".repeat(64) });
    for (const broken of [[], [{ name, digest: null }]]) {
      expect(() => assetDigests(broken, [name])).toThrow(ManifestError);
    }
  });
});

describe("next", () => {
  const built = (sha: string) => {
    const dist = mkdtempSync(join(tmpdir(), "release-manifest-dist-"));
    const digest = new Bun.CryptoHasher("sha256").update("app").digest("hex");
    for (const target of TARGETS.desktop) {
      const name = assetName("desktop", "next", undefined, sha, target);
      writeFileSync(join(dist, name), "app");
      writeFileSync(join(dist, `${name}.sha256`), `${digest}  ${name}\n`);
      writeFileSync(join(dist, `${name}.sha256.minisig`), "signature\n");
    }
    return { dist, digest };
  };

  test("built assets must match their sidecars", () => {
    const sha = "c".repeat(40);
    const { dist, digest } = built(sha);
    const name = assetName("desktop", "next", undefined, sha, "aarch64-apple-darwin");
    unlinkSync(join(dist, `${name}.sha256.minisig`));
    expect(() => verifiedDist("desktop", sha, dist)).toThrow("not signed");
    writeFileSync(join(dist, `${name}.sha256.minisig`), "signature\n");
    expect(verifiedDist("desktop", sha, dist)["aarch64-apple-darwin"]?.digest).toBe(digest);
    writeFileSync(join(dist, `${name}.sha256`), `${"0".repeat(64)}  ${name}\n`);
    expect(() => verifiedDist("desktop", sha, dist)).toThrow(ManifestError);
  });

  test("a missing Linux archive or signature prevents publication", () => {
    const sha = "c".repeat(40);
    const { dist } = built(sha);
    expect(Object.keys(verifiedDist("desktop", sha, dist)).toSorted()).toEqual(
      [...TARGETS.desktop].toSorted(),
    );
    const name = assetName("desktop", "next", undefined, sha, "aarch64-unknown-linux-gnu");
    unlinkSync(join(dist, `${name}.sha256.minisig`));
    expect(() => verifiedDist("desktop", sha, dist)).toThrow("not signed");
    unlinkSync(join(dist, name));
    expect(() => verifiedDist("desktop", sha, dist)).toThrow();
  });

  test("bridge records keep the pre-manifest shape", () => {
    const [name, text] = bridgeRecord("desktop", "c".repeat(40), "1.2.3", {
      "aarch64-apple-darwin": "d".repeat(64),
    });
    const record: unknown = JSON.parse(text);
    expect(name).toBe("desktop-channel.json");
    expect(record).toMatchObject({
      archive: `phux-desktop-next.${"c".repeat(40)}-macos-arm64.zip`,
    });
    expect(bridgeRecord("phux", "c".repeat(40), "1.2.3", {})[0]).toBe("channel.json");
  });
});

describe("arguments", () => {
  test("each subcommand parses as argparse did", () => {
    expect(parseManifestArgs(["stable", "--out", "x.json", "--publish"])).toEqual({
      command: "stable",
      out: "x.json",
      publish: true,
    });
    expect(parseManifestArgs(["next-sha", "desktop"])).toEqual({
      command: "next-sha",
      product: "desktop",
    });
    expect(parseManifestArgs(["fixtures"])).toEqual({ command: "fixtures", check: false });
    for (const bad of [
      [],
      ["next-sha", "web"],
      ["stable"],
      ["next", "--product", "phux"],
      ["bogus"],
    ]) {
      expect(() => parseManifestArgs(bad)).toThrow();
    }
  });
});

// Every release platform stays behind the same qualification and signing gate.
describe("desktop workflows", () => {
  const workflow = (name: string) => readFileSync(join(ROOT, ".github/workflows", name), "utf8");

  test("Linux builds and qualifies natively in the PR and release lanes", () => {
    for (const name of ["desktop-test.yml", "desktop-release.yml"]) {
      const linux = workflow(name).split("\n  linux:\n")[1]?.split("\n  # Signing")[0] ?? "";
      for (const required of [
        "os: ubuntu-24.04\n",
        "os: ubuntu-24.04-arm\n",
        "target: x86_64-unknown-linux-gnu",
        "target: aarch64-unknown-linux-gnu",
        "just desktop-test",
        "just desktop-native-build",
        "package-linux.ts",
        "tar -xzf",
        "smoke-linux.ts",
        "xvfb-run",
        "dbus-run-session",
      ]) {
        expect([name, required, linux.includes(required)]).toEqual([name, required, true]);
      }
    }
  });

  test("the stable signer waits for every platform and signs all sidecars", () => {
    const release = workflow("desktop-release.yml");
    const signer = release.split("\n  sign:\n")[1] ?? "";
    for (const needle of [
      "needs: [macos, linux]",
      "pattern: desktop-sidecar-*",
      'test "${#sidecars[@]}" -eq 3',
      'bash scripts/ci/sign-release.sh "${sidecars[@]}"',
      'cat "${sidecars[@]}"',
    ]) {
      expect([needle, signer.includes(needle)]).toEqual([needle, true]);
    }
    for (const platform of ["macos-arm64", "linux-${{ matrix.arch }}"]) {
      expect(release).toContain(`name: desktop-sidecar-${platform}`);
      expect(release).toContain(`name: desktop-${platform}`);
    }
  });

  test("next rebuilds when desktop-only inputs change and signs Linux", () => {
    const next = workflow("next-release.yml");
    expect(next).toContain("changed desktop phux_needed native_needed desktop_needed");
    expect(next).toContain("merge-multiple: true");
    expect(next).toContain("sign-release.sh dist/*.sha256");
  });
});
