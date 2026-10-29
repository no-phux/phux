// The channel manifest generator (ADR-0176).
import { describe, expect, test } from "bun:test";
import { mkdtempSync, readFileSync, unlinkSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { Failure, UsageError } from "../lib/command.ts";
import { ROOT } from "../lib/repo.ts";
import { table } from "../lib/value.ts";
import {
  assetName,
  bridgeRecord,
  CHANNELS,
  DESKTOP_ARCHIVES,
  entry,
  fixture,
  fixtures,
  render,
  targets,
  type Product,
} from "./manifest.ts";
import { assetDigests, newestStable, releaseManifest, verifiedDist } from "./release-manifest.ts";

// Every line shape the POSIX installers' awk parser accepts.
const LINE =
  /^(\{|\s*\},?|\s*"[A-Za-z0-9_-]+": \{|\s*"[A-Za-z0-9_-]+": ("[A-Za-z0-9._:/+-]*"|[0-9]+),?)$/;
const digests = (hex: string, product: Product = "phux"): Record<string, string> =>
  Object.fromEntries(targets(product).map((target) => [target, hex.repeat(64)]));

describe("manifest", () => {
  test("Windows is a native desktop target, never a native CLI target", () => {
    expect(targets("desktop")).toContain("x86_64-pc-windows-msvc");
    expect(targets("phux")).not.toContain("x86_64-pc-windows-msvc");
  });
  test("historical stable desktop releases retain Unix targets, while new releases and next require Windows", () => {
    const legacy = digests("d");
    expect(targets("desktop", "stable", "0.49.0")).toEqual(targets("phux"));
    expect(targets("desktop", "stable", "0.49.99")).toEqual(targets("phux"));
    expect(targets("desktop", "stable", "0.50.0")).toContain("x86_64-pc-windows-msvc");
    expect(targets("desktop", "stable", "1.0.0")).toContain("x86_64-pc-windows-msvc");
    expect(targets("desktop", "next", "0.49.0")).toContain("x86_64-pc-windows-msvc");
    expect(
      Object.keys(
        table(entry("desktop", "stable", "0.49.0", "a".repeat(40), "0.9", legacy)["artifacts"]),
      ),
    ).toEqual(targets("phux"));
    expect(() => entry("desktop", "stable", "0.50.0", "a".repeat(40), "0.9", legacy)).toThrow(
      "needs digests",
    );
    expect(() => entry("desktop", "next", "0.49.0", "a".repeat(40), "0.9", legacy)).toThrow(
      "needs digests",
    );
    expect(() => targets("desktop", "stable", "0.050.0")).toThrow("not valid");
  });
  test("fixtures are current and line-parseable", () => {
    expect(fixtures(true)).toBe(0);
    for (const channel of CHANNELS) {
      for (const line of render(fixture(channel)).trimEnd().split("\n")) expect(line).toMatch(LINE);
    }
    expect(() => fixtures(true, mkdtempSync(join(tmpdir(), "manifest-")))).toThrow(
      "stale: scripts/fixtures/release-manifest/stable.json, scripts/fixtures/release-manifest/next.json; run bun tools/main.ts release-manifest fixtures",
    );
  });

  test("desktop names cover every target on both channels", () => {
    for (const channel of CHANNELS) {
      const artifacts = table(table(table(fixture(channel)["products"])["desktop"])["artifacts"]);
      const identity = channel === "stable" ? "1.2.3" : `next.${"b".repeat(40)}`;
      expect(Object.keys(artifacts)).toEqual(targets("desktop"));
      for (const [target, suffix] of Object.entries(DESKTOP_ARCHIVES)) {
        const artifact = table(artifacts[target]);
        expect(artifact["name"]).toBe(`phux-desktop-${identity}-${suffix}`);
        expect(String(artifact["url"])).toEndWith(`/${String(artifact["name"])}`);
      }
    }
    expect(assetName("phux", "stable", "1.2.3", "x86_64-unknown-linux-gnu")).toBe(
      "phux-v1.2.3-x86_64-unknown-linux-gnu.tar.gz",
    );
  });

  test("render refuses what the installers cannot parse", () => {
    const bad = fixture("stable");
    table(table(bad["products"])["phux"])["version"] = "1.2.3 evil";
    for (const document of [bad, { a: true }, { a: [1] }, { a: {} }, { a: null }, { a: 1.5 }]) {
      expect(() => render(document)).toThrow(Failure);
    }
  });

  test("entry refuses malformed facts", () => {
    const [version, sha, protocol, all] = ["1.2.3", "a".repeat(40), "0.9", digests("e", "desktop")];
    expect(entry("desktop", "stable", version, sha, protocol, all)).toBeDefined();
    expect(entry("phux", "stable", version, sha, protocol, digests("e"))).toBeDefined();
    expect(() => entry("phux", "stable", version, sha, protocol, all)).toThrow(Failure);
    expect(() => entry("desktop", "stable", version, sha, protocol, digests("e"))).toThrow(Failure);
    const cases: [string, string, string, Record<string, string>][] = [
      ["1.02.3", sha, protocol, all],
      [version, "A".repeat(40), protocol, all],
      [version, sha, "0.09", all],
      [version, sha, protocol, { "aarch64-apple-darwin": "e".repeat(64) }],
      [version, sha, protocol, digests("E", "desktop")],
    ];
    for (const bad of cases) expect(() => entry("desktop", "stable", ...bad)).toThrow(Failure);
  });
});

describe("stable", () => {
  test("legacy published assets rebuild a stable entry without inventing Windows", () => {
    const assets = targets("phux").map((target) => ({
      name: assetName("desktop", "stable", "0.49.0", target),
      digest: `sha256:${"d".repeat(64)}`,
    }));
    const found = assetDigests(assets, "desktop", "stable", "0.49.0");
    expect(found).toEqual(digests("d"));
    expect(() => assetDigests(assets, "desktop", "stable", "0.50.0")).toThrow("not published");
  });
  test("digests come from GitHub and must exist for every asset", () => {
    const names = targets("desktop").map((target) =>
      assetName("desktop", "stable", "1.2.3", target),
    );
    const assets = names.map((name) => ({ name, digest: `sha256:${"d".repeat(64)}` }));
    const found = (given: unknown[]) => assetDigests(given, "desktop", "stable", "1.2.3");
    expect(found(assets)).toEqual(
      Object.fromEntries(targets("desktop").map((target) => [target, "d".repeat(64)])),
    );
    for (const broken of [assets.slice(0, -1), [...assets.slice(1), { name: names[0] }]]) {
      expect(() => found(broken)).toThrow(Failure);
    }
  });

  test("the newest published release per product", () => {
    const tags = ["v0.9.0", "v0.10.0", "desktop-v0.3.0", "desktop-v0.20.0"];
    const newest = newestStable([
      ...[...tags, "desktop-v0.1.0-alpha.17", "pi-extension-v9.0.0"].map((tag_name) => ({
        tag_name,
      })),
      { tag_name: "v0.11.0", draft: true },
      { tag_name: "next", prerelease: true },
    ]);
    expect([newest.phux?.["tag_name"], newest.desktop?.["tag_name"]]).toEqual([
      "v0.10.0",
      "desktop-v0.20.0",
    ]);
  });
});

describe("next", () => {
  test("built assets must be present, match their sidecars and be signed", () => {
    const sha = "c".repeat(40);
    const dist = mkdtempSync(join(tmpdir(), "release-manifest-dist-"));
    const digest = new Bun.CryptoHasher("sha256").update("app").digest("hex");
    for (const target of targets("desktop")) {
      const name = assetName("desktop", "next", sha, target);
      writeFileSync(join(dist, name), "app");
      writeFileSync(join(dist, `${name}.sha256`), `${digest}  ${name}\n`);
      writeFileSync(join(dist, `${name}.sha256.minisig`), "signature\n");
    }
    expect(verifiedDist("desktop", sha, dist)).toEqual(
      Object.fromEntries(targets("desktop").map((t) => [t, digest])),
    );
    const asset = join(dist, assetName("desktop", "next", sha, "aarch64-unknown-linux-gnu"));
    unlinkSync(`${asset}.sha256.minisig`);
    expect(() => verifiedDist("desktop", sha, dist)).toThrow("not signed");
    writeFileSync(`${asset}.sha256.minisig`, "signature\n");
    writeFileSync(`${asset}.sha256`, `${"0".repeat(64)}  ${asset}\n`);
    expect(() => verifiedDist("desktop", sha, dist)).toThrow("does not match");
    unlinkSync(asset);
    expect(() => verifiedDist("desktop", sha, dist)).toThrow();
  });

  test("bridge records keep the pre-manifest shape", () => {
    const sha = "c".repeat(40);
    const [name, text] = bridgeRecord("desktop", sha, "1.2.3", digests("d"));
    expect(name).toBe("desktop-channel.json");
    expect(JSON.parse(text)).toMatchObject({
      archive: `phux-desktop-next.${sha}-macos-arm64.zip`,
      sha256: "d".repeat(64),
    });
    expect(bridgeRecord("phux", sha, "1.2.3", {})[0]).toBe("channel.json");
  });
});

test("bad arguments are usage errors before any I/O", () => {
  for (const bad of [[], ["next-sha", "web"], ["stable"], ["next", "--product", "phux"], ["x"]]) {
    expect(() => releaseManifest.run(bad)).toThrow(UsageError);
  }
  expect(releaseManifest.run(["fixtures", "--check"])).toBe(0);
});

// Every desktop platform stays behind the signing gate.
test("the stable desktop signer waits for every platform and signs all sidecars", () => {
  const release = readFileSync(join(ROOT, ".github/workflows/desktop-release.yml"), "utf8");
  const signer = release.split("\n  sign:\n")[1] ?? "";
  expect(signer).toContain("needs: [capabilities, macos, linux, windows]");
  expect(signer).toContain("pattern: desktop-sidecar-*");
  expect(signer).toContain("bash scripts/ci/publish-desktop-release.sh");
  const publication = readFileSync(join(ROOT, "scripts/ci/publish-desktop-release.sh"), "utf8");
  for (const needle of [
    'test "${#sidecars[@]}" -eq "$EXPECTED_DESKTOP_SIDECARS"',
    'bash scripts/ci/sign-release.sh "${sidecars[@]}"',
  ]) {
    expect(publication).toContain(needle);
  }
});
