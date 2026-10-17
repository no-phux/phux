import { expect, test } from "bun:test";
import { mkdirSync, mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { assertPresentedEvidence } from "../../scripts/evidence.ts";
import {
  installedPhuxCandidates,
  isInstalledPhux,
  layoutScope,
  productionSocketReason,
} from "../../scripts/host.ts";
import { renderNotices, shippedCrates } from "../../scripts/notices.ts";
import { infoPlist, packageVersion } from "../../scripts/package-app.ts";

const png = Buffer.from([
  137, 80, 78, 71, 13, 10, 26, 10, 0, 0, 0, 13, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
]);

function evidence(files: Record<string, string | Buffer>): string {
  const dir = mkdtempSync(join(tmpdir(), "phux-evidence-"));
  for (const [name, body] of Object.entries(files)) {
    const path = join(dir, name);
    mkdirSync(join(path, ".."), { recursive: true });
    writeFileSync(path, body);
  }
  return dir;
}

const layout = JSON.stringify({
  session: "desktop-smoke",
  presented: true,
  generation: 3,
  placements: [{ id: "pane", terminalId: "local:1" }],
});
const interaction = JSON.stringify({
  kind: "present",
  presentedGeneration: 3,
  visible: true,
  clipped: false,
  bounds: { width: 80, height: 24 },
});

test("presented evidence requires a PNG, a live layout and a matching interaction", () => {
  const dir = evidence({
    "presented.png": png,
    "layout.json": layout,
    "interaction.json": interaction,
  });
  try {
    expect(assertPresentedEvidence(dir, "desktop-smoke").generation).toBe(3);
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
});

test("an ok flag without a presented capture is not success", () => {
  const dir = evidence({ "result.json": '{"ok":true}\n' });
  try {
    expect(() => assertPresentedEvidence(dir, "desktop-smoke")).toThrow(/presented.png/);
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
});

test("a drawable the display could not confirm passes only when allowed", () => {
  const dir = evidence({
    "presented.png": png,
    "layout.json": JSON.stringify({
      session: "desktop-smoke",
      presented: false,
      submitted: true,
      generation: 3,
      placements: [{ id: "pane", terminalId: "local:1" }],
    }),
    "interaction.json": interaction,
  });
  try {
    expect(() => assertPresentedEvidence(dir, "desktop-smoke")).toThrow(/not presented/);
    expect(assertPresentedEvidence(dir, "desktop-smoke", true).presentation).toBe("submitted");
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
});

test("a layout that was not presented is rejected", () => {
  const dir = evidence({
    "presented.png": png,
    "layout.json": JSON.stringify({
      session: "desktop-smoke",
      presented: false,
      generation: 1,
      placements: [],
    }),
    "interaction.json": interaction,
  });
  try {
    expect(() => assertPresentedEvidence(dir, "desktop-smoke")).toThrow(/not presented/);
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
});

test("layout scope stays the socket-and-session hash", () => {
  expect(layoutScope("/tmp/phux.sock", "desktop")).toHaveLength(24);
  expect(layoutScope("/tmp/phux.sock", "desktop")).toBe(layoutScope("/tmp/phux.sock", "desktop"));
  expect(layoutScope("/tmp/phux.sock", "other")).not.toBe(layoutScope("/tmp/phux.sock", "desktop"));
});

test("qualification refuses installed CLI paths and production sockets", () => {
  const home = "/Users/example";
  expect(installedPhuxCandidates(home)[0]).toBe(`${home}/.local/bin/phux`);
  expect(isInstalledPhux("/opt/homebrew/bin/phux", home)).toBe(true);
  expect(
    productionSocketReason("/tmp/phux-example/phux.sock", {
      USER: "example",
      TMPDIR: "/var/folders/t",
    }),
  ).toBe("refusing production socket /tmp/phux-example/phux.sock");
  expect(
    productionSocketReason("/tmp/phux-desktop-smoke-1/run/phux.sock", {
      USER: "example",
      TMPDIR: "/tmp/phux-desktop-smoke-1",
    }),
  ).toBeUndefined();
});

test("the app plist identifies the GPUI host and a stable release version", () => {
  expect(packageVersion('{"version":"0.1.0"}')).toBe("0.1.0");
  expect(() => packageVersion('{"version":"0.1.0-alpha.13"}')).toThrow(/release version/);
  expect(() => packageVersion('{"version":"latest"}')).toThrow(/release version/);
  const plist = infoPlist("0.1.0", "abc");
  expect(plist).toContain("<key>PhuxDesktopHost</key><string>gpui</string>");
  expect(plist).toContain("<key>CFBundleIdentifier</key><string>dev.phux.desktop</string>");
  expect(plist).not.toContain(".node");
  expect(plist).toContain("<key>PhuxDesktopChannel</key><string>stable</string>");
  expect(infoPlist("0.1.0", "abc", "next")).toContain(
    "<key>PhuxDesktopChannel</key><string>next</string>",
  );
});

function metadata(packages: { id: string; license?: string; source?: string | null }[]): string {
  const deps = (id: string, kind: string | null) => ({ pkg: id, dep_kinds: [{ kind }] });
  return JSON.stringify({
    packages: packages.map((pkg) => ({
      id: pkg.id,
      name: pkg.id,
      version: "1.0.0",
      license: pkg.license ?? null,
      license_file: null,
      repository: null,
      source:
        pkg.source === undefined
          ? "registry+https://github.com/rust-lang/crates.io-index"
          : pkg.source,
      manifest_path: `/nonexistent/${pkg.id}/Cargo.toml`,
    })),
    resolve: {
      root: "phux-desktop",
      nodes: [
        {
          id: "phux-desktop",
          deps: [deps("lib", null), deps("gen", "build"), deps("fixture", "dev")],
        },
        { id: "lib", deps: [deps("leaf", null)] },
        { id: "gen", deps: [] },
        { id: "leaf", deps: [] },
        { id: "fixture", deps: [] },
      ],
    },
  });
}

test("the third-party notice covers every shipped crate and refuses an unlicensed one", () => {
  const graph = [
    { id: "phux-desktop", license: "Apache-2.0", source: null },
    { id: "lib", license: "MIT OR Apache-2.0" },
    { id: "gen", license: "MIT" },
    { id: "leaf", license: "Zlib" },
    { id: "fixture" },
  ];
  const crates = shippedCrates(metadata(graph));
  expect(crates.map((crate) => crate.name)).toEqual(["gen", "leaf", "lib", "phux-desktop"]);
  const shared = "MIT License\n\nCopyright (c) example";
  const notice = renderNotices(
    crates,
    (crate) => new Map(crate.name === "leaf" ? [] : [["LICENSE-MIT", shared]]),
  );
  expect(notice).toContain("lib  1.0.0  MIT OR Apache-2.0");
  expect(notice).toContain("leaf  1.0.0  Zlib");
  expect(notice).not.toContain("phux-desktop  1.0.0");
  expect(notice.split(shared)).toHaveLength(2);
  expect(notice).toContain("gen 1.0.0 (LICENSE-MIT)\nlib 1.0.0 (LICENSE-MIT)");
  graph[3] = { id: "leaf" };
  expect(() => shippedCrates(metadata(graph))).toThrow(/no declared license.*leaf 1\.0\.0/);
});
