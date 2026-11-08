// The channel manifest generator (ADR-0176).
import { describe, expect, test } from "bun:test";
import { mkdtempSync, unlinkSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { Failure, UsageError } from "../lib/command.ts";
import { table } from "../lib/value.ts";
import {
  assetName,
  bridgeRecord,
  CHANNELS,
  entry,
  fixture,
  fixtures,
  PRODUCTS,
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
  test("the CLI is the only product, and Windows is never a native CLI target", () => {
    expect(PRODUCTS).toEqual(["phux"]);
    expect(targets("phux")).not.toContain("x86_64-pc-windows-msvc");
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

  test("names cover every target on both channels", () => {
    for (const channel of CHANNELS) {
      const products = table(fixture(channel)["products"]);
      expect(Object.keys(products)).toEqual(["phux"]);
      const artifacts = table(table(products["phux"])["artifacts"]);
      const identity = channel === "stable" ? "v9.8.7" : `next.${"a".repeat(40)}`;
      expect(Object.keys(artifacts)).toEqual(targets("phux"));
      for (const target of targets("phux")) {
        const artifact = table(artifacts[target]);
        expect(artifact["name"]).toBe(`phux-${identity}-${target}.tar.gz`);
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
    const [version, sha, protocol, all] = ["1.2.3", "a".repeat(40), "0.9", digests("e")];
    expect(entry("phux", "stable", version, sha, protocol, all)).toBeDefined();
    const cases: [string, string, string, Record<string, string>][] = [
      ["1.02.3", sha, protocol, all],
      [version, "A".repeat(40), protocol, all],
      [version, sha, "0.09", all],
      [version, sha, protocol, { "aarch64-apple-darwin": "e".repeat(64) }],
      [version, sha, protocol, digests("E")],
    ];
    for (const bad of cases) expect(() => entry("phux", "stable", ...bad)).toThrow(Failure);
  });
});

describe("stable", () => {
  test("digests come from GitHub and must exist for every asset", () => {
    const names = targets("phux").map((target) => assetName("phux", "stable", "1.2.3", target));
    const assets = names.map((name) => ({ name, digest: `sha256:${"d".repeat(64)}` }));
    const found = (given: unknown[]) => assetDigests(given, "phux", "stable", "1.2.3");
    expect(found(assets)).toEqual(digests("d"));
    for (const broken of [assets.slice(0, -1), [...assets.slice(1), { name: names[0] }]]) {
      expect(() => found(broken)).toThrow(Failure);
    }
  });

  test("the newest published release per product; retired desktop tags are ignored", () => {
    const tags = ["v0.9.0", "v0.10.0", "desktop-v0.3.0", "desktop-v0.20.0"];
    const newest = newestStable([
      ...[...tags, "pi-extension-v9.0.0"].map((tag_name) => ({ tag_name })),
      { tag_name: "v0.11.0", draft: true },
      { tag_name: "next", prerelease: true },
    ]);
    expect(Object.keys(newest)).toEqual(["phux"]);
    expect(newest.phux?.["tag_name"]).toBe("v0.10.0");
  });
});

describe("next", () => {
  test("built assets must be present, match their sidecars and be signed", () => {
    const sha = "c".repeat(40);
    const dist = mkdtempSync(join(tmpdir(), "release-manifest-dist-"));
    const digest = new Bun.CryptoHasher("sha256").update("app").digest("hex");
    for (const target of targets("phux")) {
      const name = assetName("phux", "next", sha, target);
      writeFileSync(join(dist, name), "app");
      writeFileSync(join(dist, `${name}.sha256`), `${digest}  ${name}\n`);
      writeFileSync(join(dist, `${name}.sha256.minisig`), "signature\n");
    }
    expect(verifiedDist("phux", sha, dist)).toEqual(
      Object.fromEntries(targets("phux").map((t) => [t, digest])),
    );
    const asset = join(dist, assetName("phux", "next", sha, "aarch64-unknown-linux-gnu"));
    unlinkSync(`${asset}.sha256.minisig`);
    expect(() => verifiedDist("phux", sha, dist)).toThrow("not signed");
    writeFileSync(`${asset}.sha256.minisig`, "signature\n");
    writeFileSync(`${asset}.sha256`, `${"0".repeat(64)}  ${asset}\n`);
    expect(() => verifiedDist("phux", sha, dist)).toThrow("does not match");
    unlinkSync(asset);
    expect(() => verifiedDist("phux", sha, dist)).toThrow();
  });

  test("bridge records keep the pre-manifest shape", () => {
    const sha = "c".repeat(40);
    const [name, text] = bridgeRecord("phux", sha, "1.2.3");
    expect(name).toBe("channel.json");
    expect(JSON.parse(text)).toMatchObject({ product: "phux", sha, version: "1.2.3" });
  });
});

test("bad arguments are usage errors before any I/O", () => {
  for (const bad of [[], ["next-sha", "web"], ["stable"], ["next", "--product", "phux"], ["x"]]) {
    expect(() => releaseManifest.run(bad)).toThrow(UsageError);
  }
  expect(releaseManifest.run(["fixtures", "--check"])).toBe(0);
});
