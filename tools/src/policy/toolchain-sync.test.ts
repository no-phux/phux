// The native manifests, the Mise and Nix environments, and CI agree on their
// toolchains. The authoritative Rust and Zig inputs stay where their native
// consumers require them: rust-toolchain.toml and .config/zig-toolchain.json.
//
// Static: this reads files, so it runs on a CI checkout with no Nix, no Mise
// and no compilers (`just toolchain-check`, and `just guards` through
// tools-check). `just toolchain-parity` is the runtime half that resolves both
// environments and compares binaries.
import { expect, test } from "bun:test";
import { readFileSync } from "node:fs";
import { Glob } from "bun";
import { repoPath, ROOT } from "../lib/repo.ts";
import { str, table, type Table } from "../lib/value.ts";
import { workflowNames, workflowText } from "./workflows.ts";

interface Sources {
  mise: string;
  rustToolchain: string;
  zig: string;
  flake: string;
  toolsPackage: string;
  clippy: string;
  /** Manifests that may spell a literal `rust-version`, by repository path. */
  cargo: Record<string, string>;
  /** Manifests that must pin wasm-bindgen exactly, phux-web first. */
  bindgen: Record<string, string>;
  /** Workflow text, by file name. */
  workflows: Record<string, string>;
}

const read = (path: string): string => readFileSync(repoPath(path), "utf8");

function load(): Sources {
  const cargo = ["Cargo.toml", ...new Glob("clients/*/Cargo.toml").scanSync(ROOT)];
  return {
    mise: read("mise.toml"),
    rustToolchain: read("rust-toolchain.toml"),
    zig: read(".config/zig-toolchain.json"),
    flake: read("flake.nix"),
    toolsPackage: read("tools/package.json"),
    clippy: read("clippy.toml"),
    cargo: Object.fromEntries(cargo.sort().map((path) => [path, read(path)])),
    bindgen: Object.fromEntries(
      ["clients/phux-web/Cargo.toml", "clients/phux-vt-web/Cargo.toml"].map((path) => [
        path,
        read(path),
      ]),
    ),
    workflows: Object.fromEntries(workflowNames().map((name) => [name, workflowText(name)])),
  };
}

const toml = (text: string): Table => table(Bun.TOML.parse(text));

/** A `[tools]` entry:`tool = "1.2.3"` or `tool = { version = "1.2.3", ... }`. */
function miseTool(tools: Table, name: string): string | undefined {
  const value = tools[name];
  return str(value) ?? str(table(value)["version"]);
}

/** The keys of a Nix attribute set `name = { "key" = { ... }; ... };` in flake.nix. */
function digestVersions(flake: string, name: string): string[] {
  const lines = flake.split("\n");
  const start = lines.findIndex((line) => new RegExp(`^\\s*${name} = \\{\\s*$`).test(line));
  if (start < 0) return [];
  const indent = /^\s*/.exec(lines[start] ?? "")?.[0] ?? "";
  const end = lines.findIndex((line, index) => index > start && line === `${indent}};`);
  return lines
    .slice(start + 1, end < 0 ? undefined : end)
    .flatMap((line) => /^\s*"([^"]+)" = \{/.exec(line)?.[1] ?? []);
}

interface Pins {
  rust: string;
  msrv: string;
  zig: string;
  node: string;
  bun: string;
  usage: string;
  mbx: string;
}

function pins(sources: Sources): Pins {
  const tools = table(toml(sources.mise)["tools"]);
  const rust = str(table(toml(sources.rustToolchain)["toolchain"])["channel"]) ?? "";
  return {
    rust,
    msrv: rust.replace(/\.[^.]*$/, ""),
    zig: str(table(JSON.parse(sources.zig))["version"]) ?? "",
    node: miseTool(tools, "node") ?? "",
    bun: miseTool(tools, "bun") ?? "",
    usage: miseTool(tools, "usage") ?? "",
    mbx: miseTool(tools, "mr-boxington") ?? "",
  };
}

function miseFailures(sources: Sources, pin: Pins): string[] {
  const tools = table(toml(sources.mise)["tools"]);
  const exact = /^\d+\.\d+\.\d+$/;
  const failures: string[] = [];
  const check = (holds: boolean, message: string) => holds || failures.push(message);
  check(pin.rust !== "", "rust-toolchain.toml must name a channel");
  check(pin.zig !== "", ".config/zig-toolchain.json must name a version");
  check(miseTool(tools, "rust") === pin.rust, `mise Rust must be ${pin.rust}`);
  check(miseTool(tools, "zig") === pin.zig, `mise Zig must be ${pin.zig}`);
  check(/^\d+$/.test(pin.node), "mise Node must use a major version");
  check(exact.test(pin.bun), "mise Bun must use an exact release");
  check(exact.test(pin.usage), "mise usage must use an exact release");
  check(exact.test(pin.mbx), "mise mr-boxington must use an exact release");
  check(
    table(tools["rust"])["mr_boxington"] === true,
    "mise Rust must set mr_boxington = true so Cargo runs through the pinned mbx",
  );
  return failures;
}

// The Nix shell must resolve the same compilers and runtimes as the Mise path.
// nixpkgs once carried Bun 1.3.13 while mise.toml was on 1.4.0, so the Nix
// path type-checked against a runtime nobody ships. These are string checks
// against flake.nix rather than an evaluation, so they run without Nix. Rust
// needs none: the flake reads rust-toolchain.toml directly. Bun, usage and mbx
// are pinned by construction (the flake reads mise.toml); what can rot is the
// digest table the pinned build needs.
function flakeFailures(sources: Sources, pin: Pins): string[] {
  const { flake } = sources;
  const failures: string[] = [];
  const check = (holds: boolean, message: string) => holds || failures.push(message);
  check(
    flake.includes("./.config/zig-toolchain.json"),
    "flake.nix must install Zig from .config/zig-toolchain.json",
  );
  check(
    flake.includes(`pkgs.nodejs_${pin.node}`),
    `flake.nix must use pkgs.nodejs_${pin.node} for Node ${pin.node}`,
  );
  for (const [tool, attr, digests, version] of [
    ["Bun", "miseTools.bun", "bunDigests", pin.bun],
    ["usage", "miseTools.usage", "usageDigests", pin.usage],
    ["mbx", 'miseTools."mr-boxington"', "mbxDigests", pin.mbx],
  ] as const) {
    check(flake.includes(attr), `flake.nix must read the ${tool} pin from mise.toml (${attr})`);
    check(
      digestVersions(flake, digests).includes(version),
      `flake.nix ${digests} has no entry for ${tool} ${version}; add the per-system hashes (see the comment there)`,
    );
  }
  check(
    flake.includes("libexec/mbx:$PATH"),
    "flake.nix shellHook must put the mbx Cargo wrapper first on PATH",
  );
  // CI installs Bun from the mise pin, so the tools package declares the same
  // engine. It is the only package that pins Bun exactly; plugins take a range.
  const engines = table(table(JSON.parse(sources.toolsPackage))["engines"]);
  check(engines["bun"] === pin.bun, `tools/package.json engines.bun must be ${pin.bun}`);
  return failures;
}

function cargoFailures(sources: Sources, pin: Pins): string[] {
  const failures: string[] = [];
  for (const [path, text] of Object.entries(sources.cargo)) {
    const manifest = toml(text);
    const declared =
      table(manifest["package"])["rust-version"] ??
      table(table(manifest["workspace"])["package"])["rust-version"];
    // `rust-version.workspace = true` inherits the root's, checked here too.
    if (typeof declared === "string" && declared !== pin.msrv) {
      failures.push(`${path} rust-version must be ${pin.msrv} (found ${declared})`);
    }
  }
  if (str(toml(sources.clippy)["msrv"]) !== pin.msrv) {
    failures.push(`clippy.toml msrv must be ${pin.msrv}`);
  }
  const [web, ...others] = Object.entries(sources.bindgen);
  const bindgen = /^wasm-bindgen = "=([^"]+)"/m.exec(web?.[1] ?? "")?.[1];
  if (bindgen === undefined) return [...failures, `${web?.[0]} must pin wasm-bindgen exactly`];
  for (const [path, text] of others) {
    if (!text.includes(`wasm-bindgen = "=${bindgen}"`)) {
      failures.push(`${path} must pin wasm-bindgen ${bindgen}, as ${web?.[0]} does`);
    }
  }
  return failures;
}

function workflowFailures(sources: Sources, pin: Pins): string[] {
  const failures: string[] = [];
  for (const [name, text] of Object.entries(sources.workflows)) {
    for (const [, version] of text.matchAll(/node-version: (\d+)/g)) {
      if (version !== pin.node) failures.push(`${name} Node version must be ${pin.node}`);
    }
  }
  if (!(sources.workflows["ci.yml"] ?? "").includes(`nixpkgs#nodejs_${pin.node} `)) {
    failures.push(`ci.yml guards must run nixpkgs#nodejs_${pin.node}`);
  }
  return failures;
}

function failures(sources: Sources): string[] {
  const pin = pins(sources);
  return [
    ...miseFailures(sources, pin),
    ...flakeFailures(sources, pin),
    ...cargoFailures(sources, pin),
    ...workflowFailures(sources, pin),
  ];
}

const SOURCES = load();
const PIN = pins(SOURCES);

test("mise.toml mirrors the native Rust and Zig pins and pins its tools exactly", () => {
  expect(miseFailures(SOURCES, PIN)).toEqual([]);
});

test("flake.nix resolves the same toolchain as mise.toml", () => {
  expect(flakeFailures(SOURCES, PIN)).toEqual([]);
});

test("Cargo manifests and clippy share the MSRV, and the web clients one wasm-bindgen", () => {
  expect(cargoFailures(SOURCES, PIN)).toEqual([]);
});

test("workflows install the mise Node major", () => {
  expect(workflowFailures(SOURCES, PIN)).toEqual([]);
});

/** SOURCES with one file's text rewritten. */
function edit<K extends keyof Sources>(key: K, change: (value: Sources[K]) => Sources[K]): Sources {
  return { ...SOURCES, [key]: change(SOURCES[key]) };
}

test("each drift is named", () => {
  const bump = (text: string) => text.replace(/^bun = "[^"]+"/m, 'bun = "9.9.9"');
  expect(failures(edit("mise", bump))).toEqual([
    "flake.nix bunDigests has no entry for Bun 9.9.9; add the per-system hashes (see the comment there)",
    "tools/package.json engines.bun must be 9.9.9",
  ]);
  expect(
    failures(edit("mise", (text) => text.replace(/^bun = "[^"]+"/m, 'bun = "1.4"'))),
  ).toContain("mise Bun must use an exact release");
  expect(
    failures(edit("mise", (text) => text.replace("mr_boxington = true", "mr_boxington = false"))),
  ).toEqual(["mise Rust must set mr_boxington = true so Cargo runs through the pinned mbx"]);
  expect(failures(edit("rustToolchain", (text) => text.replace(PIN.rust, "1.98.0")))).toEqual([
    "mise Rust must be 1.98.0",
    ...Object.keys(SOURCES.cargo)
      .filter((path) => /rust-version = "/.test(SOURCES.cargo[path] ?? ""))
      .map((path) => `${path} rust-version must be 1.98 (found ${PIN.msrv})`),
    "clippy.toml msrv must be 1.98",
  ]);
  expect(failures(edit("flake", (text) => text.replace("libexec/mbx:$PATH", "")))).toEqual([
    "flake.nix shellHook must put the mbx Cargo wrapper first on PATH",
  ]);
  const vt = "clients/phux-vt-web/Cargo.toml";
  const bindgen = (text: string) =>
    text.replace(/^wasm-bindgen = "=[^"]+"/m, 'wasm-bindgen = "=0.0.1"');
  expect(
    failures(edit("bindgen", (record) => ({ ...record, [vt]: bindgen(record[vt] ?? "") }))),
  ).toEqual([
    `${vt} must pin wasm-bindgen ${/wasm-bindgen = "=([^"]+)"/.exec(SOURCES.bindgen[vt] ?? "")?.[1]}, as clients/phux-web/Cargo.toml does`,
  ]);
  expect(
    failures(
      edit("workflows", (record) => ({ ...record, "x.yml": "with:\n  node-version: 24\n" })),
    ),
  ).toEqual(["x.yml Node version must be " + PIN.node]);
  expect(failures(edit("workflows", (record) => ({ ...record, "ci.yml": "" })))).toEqual([
    `ci.yml guards must run nixpkgs#nodejs_${PIN.node}`,
  ]);
});

test("a digest for another version, or another tool's table, does not satisfy the pin", () => {
  const usage = PIN.usage;
  const flake = SOURCES.flake.replace(`"${usage}" = {`, `"0.0.0" = {`);
  expect(digestVersions(flake, "usageDigests")).not.toContain(usage);
  expect(digestVersions(SOURCES.flake, "bunDigests")).not.toContain(usage);
});
