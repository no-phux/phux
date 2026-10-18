// Test the pinned wgpu dependency in a disposable standalone Cargo workspace.
//
// Runs on Linux with Vulkan or GL (lavapipe is sufficient). Arguments are
// forwarded to `cargo test --workspace --lib --features test-support`. No
// committed lockfile, materialized source, or installed application changes.
import { cpSync, mkdirSync, mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { basename, dirname, join } from "node:path";
import { ROOT } from "../lib/repo.ts";
import type { Layout } from "./source.ts";

// The original fixtures the inherited GPUI unit tests name, at an immutable
// Zed revision: gpui-fast's archive omits fonts and metadata.
const FONT_REVISION = "279fe070bb389b79652e52065b2f001edcc0b11b";
const FONTS = [
  "lilex/Lilex-Regular.ttf",
  "lilex/Lilex-Bold.ttf",
  "ibm-plex-sans/IBMPlexSans-Regular.ttf",
];
const SKIPPED = new Set([".git", "target", "Cargo.lock"]);

async function fetchFonts(workspace: string): Promise<void> {
  for (const font of FONTS) {
    const url = `https://raw.githubusercontent.com/zed-industries/zed/${FONT_REVISION}/assets/fonts/${font}`;
    const response = await fetch(url, { signal: AbortSignal.timeout(60_000) });
    if (!response.ok) throw new Error(`${url}: HTTP ${response.status}`);
    const target = join(workspace, "assets/fonts", font);
    mkdirSync(dirname(target), { recursive: true });
    writeFileSync(target, new Uint8Array(await response.arrayBuffer()));
  }
}

export async function prepareWorkspace(
  workspace: string,
  paths: Layout,
  fonts: (workspace: string) => Promise<void> = fetchFonts,
): Promise<void> {
  // rustup otherwise chooses the machine default outside the checkout.
  cpSync(join(ROOT, "rust-toolchain.toml"), join(workspace, "rust-toolchain.toml"));
  cpSync(join(paths.output, "gpui-fast-wgpu"), join(workspace, "gpui/gpui-fast-wgpu"), {
    recursive: true,
    filter: (source) => !SKIPPED.has(basename(source)),
  });
  const gpui = JSON.stringify(join(paths.output, "gpui-fast"));
  writeFileSync(
    join(workspace, "Cargo.toml"),
    `[workspace]\nresolver = "3"\nmembers = ["gpui/gpui-fast-wgpu"]\n\n` +
      `[patch.crates-io]\ngpui-fast = { path = ${gpui} }\n`,
  );
  // Reuse the desktop's resolved versions. Cargo may prune unrelated host
  // packages or add test-only entries in this disposable copy, never in native/.
  cpSync(join(paths.toolchain, "../native/Cargo.lock"), join(workspace, "Cargo.lock"));
  // gpui-fast's shared rounded-clip tests keep their upstream sibling path.
  const fixture = "src/fast/rounded_clip_fixtures.rs";
  mkdirSync(dirname(join(workspace, "gpui/gpui", fixture)), { recursive: true });
  cpSync(join(paths.output, "gpui-fast", fixture), join(workspace, "gpui/gpui", fixture));
  // Upstream unit tests reference omitted archive fixtures three levels above
  // src/; that layout stays inside the disposable workspace.
  await fonts(workspace);
}

/** Run the wgpu crate's tests; returns cargo's exit code. */
export async function testWgpu(cargoArgs: readonly string[], paths: Layout): Promise<number> {
  const workspace = mkdtempSync(join(tmpdir(), "phux-wgpu-tests-"));
  try {
    await prepareWorkspace(workspace, paths);
    const runtime = join(workspace, "runtime");
    mkdirSync(runtime, { mode: 0o700 });
    const env = { ...process.env, XDG_RUNTIME_DIR: process.env["XDG_RUNTIME_DIR"] || runtime };
    const cargo = Bun.spawn(
      ["cargo", "test", "--workspace", "--lib", "--features", "test-support", ...cargoArgs],
      { cwd: workspace, env, stdio: ["inherit", "inherit", "inherit"] },
    );
    return await cargo.exited;
  } finally {
    rmSync(workspace, { recursive: true, force: true });
  }
}
