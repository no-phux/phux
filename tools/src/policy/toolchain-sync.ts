// Guard the native manifests, the Mise and Nix environments, CI and the
// standalone workspaces from quietly drifting apart. The authoritative Rust
// and Zig inputs stay where their native consumers read them:
// rust-toolchain.toml and .config/zig-toolchain.json.
//
// A STATIC check: it reads files, so it runs on a CI checkout with no Nix, no
// Mise and no compilers (`just toolchain-check`, a `guards` gate).
import { readdirSync, readFileSync } from "node:fs";
import { join } from "node:path";
import { git } from "../lib/git.ts";
import { str, table } from "../lib/value.ts";

export interface ToolchainSync {
  readonly failures: string[];
  /** The pins every input agreed on, for the success line. */
  readonly summary: string;
}

const EXACT = /^\d+\.\d+\.\d+$/;

/** A mise tool's version, from `tool = "1.2.3"` or `tool = { version = "1.2.3", ... }`. */
function miseVersion(tools: Record<string, unknown>, name: string): string | undefined {
  const value = tools[name];
  return str(value) ?? str(table(value ?? {})["version"]);
}

/** `"<version>" = {` opening an entry of a flake.nix digest table. */
function hasDigestEntry(flake: string, version: string): boolean {
  return new RegExp(`^\\s*"${version.replaceAll(".", "\\.")}" = \\{`, "m").test(flake);
}

export function toolchainSync(root: string): ToolchainSync {
  const read = (path: string): string => readFileSync(join(root, path), "utf8");
  const failures: string[] = [];
  const fail = (message: string): void => {
    failures.push(`toolchain sync: ${message}`);
  };

  const rust =
    str(table(table(Bun.TOML.parse(read("rust-toolchain.toml")))["toolchain"])["channel"]) ?? "";
  const zig = str(table(JSON.parse(read(".config/zig-toolchain.json")))["version"]) ?? "";
  const msrv = rust.slice(0, rust.lastIndexOf("."));
  const tools = table(table(Bun.TOML.parse(read("mise.toml")))["tools"]);
  const node = miseVersion(tools, "node") ?? "";
  const bun = miseVersion(tools, "bun") ?? "";
  const usage = miseVersion(tools, "usage") ?? "";
  const mbx = miseVersion(tools, "mr-boxington") ?? "";

  if (miseVersion(tools, "rust") !== rust) fail(`mise Rust must be ${rust}`);
  if (miseVersion(tools, "zig") !== zig) fail(`mise Zig must be ${zig}`);
  if (!/^\d+$/.test(node)) fail("mise Node must use a major version");
  if (!EXACT.test(bun)) fail("mise Bun must use an exact release");
  if (!EXACT.test(usage)) fail("mise usage must use an exact release");
  if (!EXACT.test(mbx)) fail("mise mr-boxington must use an exact release");
  if (table(tools["rust"] ?? {})["mr_boxington"] !== true) {
    fail("mise Rust must set mr_boxington = true so Cargo runs through the pinned mbx");
  }

  // The Nix shell must resolve the same compilers and runtimes as the Mise
  // path. Nothing checked this before, and the two had already parted:
  // nixpkgs carried Bun 1.3.13 while mise.toml was on 1.4.0. String checks
  // against flake.nix, not an evaluation, so they run without Nix. Rust needs
  // none: the flake reads rust-toolchain.toml directly.
  const flake = read("flake.nix");
  const flakeHas = (needle: string, message: string): void => {
    if (!flake.includes(needle)) fail(message);
  };
  flakeHas(
    "./.config/zig-toolchain.json",
    "flake.nix must install Zig from .config/zig-toolchain.json",
  );
  flakeHas(`pkgs.nodejs_${node}`, `flake.nix must use pkgs.nodejs_${node} for Node ${node}`);

  // Bun, usage and mbx are pinned by construction: the flake reads mise.toml.
  // What CAN rot is the digest table each pinned build needs.
  flakeHas(
    "miseTools.bun",
    "flake.nix must read the Bun pin from mise.toml (bunVersion = miseTools.bun)",
  );
  if (!hasDigestEntry(flake, bun)) {
    fail(
      `flake.nix bunDigests has no entry for Bun ${bun}; add the per-system hashes (see the comment there)`,
    );
  }
  // CI installs Bun from the mise pin, so the tools package must declare the
  // same engine. It is the only package that pins Bun exactly.
  if (!read("tools/package.json").includes(`"bun": "${bun}"`)) {
    fail(`tools/package.json engines.bun must be ${bun}`);
  }
  flakeHas(
    "miseTools.usage",
    "flake.nix must read the usage pin from mise.toml (usageVersion = miseTools.usage)",
  );
  flakeHas("usageDigests", "flake.nix must declare usageDigests");
  if (!hasDigestEntry(flake, usage)) {
    fail(
      `flake.nix usageDigests has no entry for usage ${usage}; add the per-system hashes (see the comment there)`,
    );
  }
  flakeHas(
    'miseTools."mr-boxington"',
    'flake.nix must read the mbx pin from mise.toml (mbxVersion = miseTools."mr-boxington")',
  );
  if (!hasDigestEntry(flake, mbx)) {
    fail(
      `flake.nix mbxDigests has no entry for mbx ${mbx}; add the per-system hashes (see the comment there)`,
    );
  }
  flakeHas("libexec/mbx:$PATH", "flake.nix shellHook must put the mbx Cargo wrapper first on PATH");

  // Every workspace root: the repo, each client, and the plugin index generator.
  const manifests = git(
    ["ls-files", "--", "Cargo.toml", "clients/*/Cargo.toml", "plugins/index/Cargo.toml"],
    root,
  )
    .split("\n")
    .filter((path) => path !== "");
  for (const manifest of manifests) {
    for (const [, version] of read(manifest).matchAll(/rust-version = "([0-9][^"]*)"/g)) {
      if (version !== msrv) fail(`${manifest} rust-version must be ${msrv}`);
    }
  }
  if (!read("clippy.toml").includes(`msrv = "${msrv}"`)) fail(`clippy MSRV must be ${msrv}`);

  const bindgen = /^wasm-bindgen = "=([^"]+)"/m.exec(read("clients/phux-web/Cargo.toml"))?.[1];
  if (bindgen === undefined) fail("phux-web must pin wasm-bindgen");
  for (const client of ["phux-web", "phux-vt-web"]) {
    if (!read(`clients/${client}/Cargo.toml`).includes(`wasm-bindgen = "=${bindgen}"`)) {
      fail(`${client} must pin wasm-bindgen ${bindgen}`);
    }
  }

  const workflows = readdirSync(join(root, ".github/workflows"))
    .filter((name) => name.endsWith(".yml"))
    .map((name) => `.github/workflows/${name}`);
  for (const workflow of workflows) {
    for (const [, version] of read(workflow).matchAll(/node-version: (\d+)/g)) {
      if (version !== node) fail(`${workflow} Node version must be ${node}`);
    }
  }

  // Pins spelled outside a `node-version:` key or a Cargo manifest.
  const pluginIndex = read(".github/workflows/plugin-index.yml");
  if (!new RegExp(`^\\s*toolchain: ${rust.replaceAll(".", "\\.")}$`, "m").test(pluginIndex)) {
    fail(`plugin-index.yml must install Rust ${rust}`);
  }
  const deny = miseVersion(tools, "cargo-deny") ?? "";
  if (!new RegExp(`^\\s*version=${deny.replaceAll(".", "\\.")}$`, "m").test(pluginIndex)) {
    fail(`plugin-index.yml must download cargo-deny ${deny} (mise.toml), with its digest`);
  }
  if (!read(".github/workflows/ci.yml").includes(`nixpkgs#nodejs_${node} `)) {
    fail(`ci.yml guards must run nixpkgs#nodejs_${node}`);
  }

  return {
    failures,
    summary: `toolchain sync (Rust ${rust}, Zig ${zig}, Node ${node}, Bun ${bun}, usage ${usage}, mbx ${mbx})`,
  };
}
