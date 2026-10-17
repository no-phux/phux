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
import {
  canonicalLicenses,
  electedLicense,
  noticeTexts,
  renderNotices,
  shippedCrates,
} from "../../scripts/notices.ts";
import { infoPlist, packageVersion } from "../../scripts/package-app.ts";
import { BUDGETS, checkPerfBudget } from "../../scripts/perf-budget.ts";

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

function perfLine(phase: string, overrides: Record<string, unknown> = {}): string {
  const budget = BUDGETS[phase];
  if (!budget) throw new Error(`no budget for ${phase}`);
  const value = (path: string) => budget.counters[path] ?? 0;
  return JSON.stringify({
    phase,
    frames: value("frames"),
    tick_us: { mean: 100, p50: 90, p99: 200 },
    scene: {
      reused: value("scene.reused"),
      patched: value("scene.patched"),
      shaped_lines: value("scene.shaped_lines"),
    },
    runtime: {
      project_grid: { count: 0 },
      published: value("runtime.published"),
      buffer_fresh: value("runtime.buffer_fresh"),
    },
    process: { max_rss_bytes: 80_000_000 },
    ...overrides,
  });
}

test("headless perf passes within budget and ignores cargo noise", () => {
  const lines = Object.keys(BUDGETS).map((phase) => perfLine(phase));
  expect(checkPerfBudget(["running 1 test", ...lines, "mbx[cache]: done"].join("\n"))).toEqual([]);
});

test("headless perf fails on work, time, RSS and phase-set regressions", () => {
  const phases = Object.keys(BUDGETS);
  const ok = (phase: string) => perfLine(phase);
  const replace = (phase: string, line: string) =>
    phases.map((name) => (name === phase ? line : ok(name))).join("\n");
  expect(
    checkPerfBudget(
      replace("cached", perfLine("cached", { scene: { reused: 0, patched: 0, shaped_lines: 24 } })),
    ),
  ).toEqual([
    "cached: scene.reused is 0, budget 128",
    "cached: scene.shaped_lines is 24, budget 0",
  ]);
  expect(
    checkPerfBudget(replace("scroll", perfLine("scroll", { tick_us: { mean: 1e9 } }))),
  ).toEqual(["scroll: tick_us.mean is 1000000000, ceiling 60000"]);
  expect(
    checkPerfBudget(replace("sparse", perfLine("sparse", { process: { max_rss_bytes: 2 ** 31 } }))),
  ).toEqual(["sparse: process.max_rss_bytes is 2147483648, ceiling 1073741824"]);
  expect(checkPerfBudget(phases.slice(1).map(ok).join("\n"))).toEqual([
    "cached: phase missing from the run",
  ]);
  expect(
    checkPerfBudget(
      [...phases.map(ok), ok("cached"), perfLine("cached", { phase: "new" })].join("\n"),
    ),
  ).toEqual(["cached: phase reported twice", "new: phase has no budget"]);
});

function metadata(
  packages: { id: string; license?: string; source?: string | null; procMacro?: boolean }[],
): string {
  const deps = (id: string, kind: string | null) => ({ pkg: id, dep_kinds: [{ kind }] });
  return JSON.stringify({
    packages: packages.map((pkg) => ({
      id: pkg.id,
      name: pkg.id,
      version: "1.0.0",
      license: pkg.license ?? null,
      license_file: null,
      repository: null,
      authors: [`${pkg.id} Author <author@example.com>`],
      source:
        pkg.source === undefined
          ? "registry+https://github.com/rust-lang/crates.io-index"
          : pkg.source,
      manifest_path: `/nonexistent/${pkg.id}/Cargo.toml`,
      targets: [{ kind: [pkg.procMacro === true ? "proc-macro" : "lib"] }],
    })),
    resolve: {
      root: "phux-desktop",
      nodes: [
        {
          id: "phux-desktop",
          deps: [deps("lib", null), deps("gen", "build"), deps("fixture", "dev")],
        },
        { id: "lib", deps: [deps("leaf", null), deps("derive", null)] },
        { id: "derive", deps: [deps("syntax", null)] },
        { id: "gen", deps: [] },
        { id: "leaf", deps: [] },
        { id: "syntax", deps: [] },
        { id: "fixture", deps: [] },
      ],
    },
  });
}

const graph = () => [
  { id: "phux-desktop", license: "Apache-2.0", source: null },
  { id: "lib", license: "MIT OR Apache-2.0" },
  { id: "derive", license: "MIT", procMacro: true },
  { id: "syntax", license: "MIT/Apache-2.0" },
  { id: "gen", license: "MPL-2.0" },
  { id: "leaf", license: "Zlib" },
  { id: "fixture" },
];

test("the third-party notice covers every shipped crate and refuses an unlicensed one", () => {
  const crates = shippedCrates(metadata(graph()));
  expect(crates.map((crate) => crate.name)).toEqual([
    "derive",
    "gen",
    "leaf",
    "lib",
    "phux-desktop",
    "syntax",
  ]);
  expect(crates.filter((crate) => crate.buildOnly).map((crate) => crate.name)).toEqual([
    "derive",
    "gen",
    "syntax",
  ]);
  const shared = "MIT License\n\nCopyright (c) example";
  const notice = renderNotices(crates, () => new Map([["LICENSE-MIT", shared]]));
  const [linked = "", buildTime = ""] = notice.split(
    "Build-time tools, not part of the executable:",
  );
  expect(linked).toContain("lib  1.0.0  MIT OR Apache-2.0");
  expect(linked).toContain("leaf  1.0.0  Zlib");
  expect(linked).not.toContain("gen  1.0.0");
  expect(buildTime).toContain("gen  1.0.0  MPL-2.0");
  expect(buildTime).toContain("derive  1.0.0  MIT");
  expect(notice).not.toContain("phux-desktop  1.0.0");
  expect(notice.split(shared)).toHaveLength(2);
  expect(notice).toContain("derive 1.0.0 (LICENSE-MIT)\ngen 1.0.0 (LICENSE-MIT)");
  const unlicensed = graph();
  unlicensed[5] = { id: "leaf" };
  expect(() => shippedCrates(metadata(unlicensed))).toThrow(/no declared license.*leaf 1\.0\.0/);
});

test("a crate with no license file gets the canonical text of a license it offers", () => {
  const canonical = { "Apache-2.0": "Apache text", MIT: "MIT text\nCopyright (c) {holders}" };
  const crate = shippedCrates(metadata(graph())).find((each) => each.name === "derive");
  if (crate === undefined) throw new Error("derive is shipped");
  expect(noticeTexts(crate, canonical)).toEqual(
    new Map([["MIT, canonical text", "MIT text\nCopyright (c) derive Author"]]),
  );
  const dual = noticeTexts({ ...crate, license: "MIT / Apache-2.0" }, canonical);
  expect(dual).toEqual(new Map([["Apache-2.0, canonical text", "Apache text"]]));
  const anonymous = noticeTexts({ ...crate, authors: [] }, canonical);
  expect(anonymous.get("MIT, canonical text")).toBe("MIT text\nCopyright (c) The derive authors");
  expect(electedLicense("Zlib OR Apache-2.0 OR MIT")).toBe("Apache-2.0");
  expect(electedLicense("MIT AND BSD-3-Clause")).toBeUndefined();
  expect(electedLicense("Apache-2.0 WITH LLVM-exception")).toBeUndefined();
  expect(electedLicense("Zlib")).toBeUndefined();
  const zlib = { ...crate, license: "Zlib" };
  expect(() => renderNotices([zlib], (each) => noticeTexts(each, canonical))).toThrow(
    /no license text.*derive 1\.0\.0 \(Zlib\)/,
  );
});

test("the bundled canonical license texts are the unmodified SPDX texts", () => {
  const { MIT, "Apache-2.0": apache } = canonicalLicenses();
  expect(apache).toStartWith("                                 Apache License");
  expect(apache).toContain("END OF TERMS AND CONDITIONS");
  expect(MIT).toContain("Copyright (c) {holders}\n\nPermission is hereby granted");
});
