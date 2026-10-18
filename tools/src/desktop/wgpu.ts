// Test the pinned wgpu renderer crate (Linux with Vulkan or GL) in a
// disposable standalone workspace; nothing committed or materialized changes.
import { cpSync, mkdirSync, mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { basename, dirname, join } from "node:path";
import { Failure } from "../lib/command.ts";
import { inherit } from "../lib/process.ts";
import { ROOT } from "../lib/repo.ts";
import type { Layout } from "./source.ts";

// The fixtures the inherited GPUI unit tests name, which gpui-fast's archive
// omits, at an immutable Zed revision.
const FONTS =
  "https://raw.githubusercontent.com/zed-industries/zed/279fe070bb389b79652e52065b2f001edcc0b11b/assets/fonts/";
const FONT_FILES = [
  "lilex/Lilex-Regular.ttf",
  "lilex/Lilex-Bold.ttf",
  "ibm-plex-sans/IBMPlexSans-Regular.ttf",
];

async function fetchFonts(workspace: string): Promise<void> {
  for (const font of FONT_FILES) {
    const response = await fetch(FONTS + font, { signal: AbortSignal.timeout(60_000) });
    if (!response.ok) throw new Failure(`${FONTS + font}: HTTP ${response.status}`);
    const target = join(workspace, "assets/fonts", font);
    mkdirSync(dirname(target), { recursive: true });
    writeFileSync(target, new Uint8Array(await response.arrayBuffer()));
  }
}

export async function prepareWorkspace(
  workspace: string,
  { output, toolchain }: Layout,
  fonts = fetchFonts,
): Promise<void> {
  // rustup otherwise chooses the machine default outside the checkout.
  cpSync(join(ROOT, "rust-toolchain.toml"), join(workspace, "rust-toolchain.toml"));
  cpSync(join(output, "gpui-fast-wgpu"), join(workspace, "gpui/gpui-fast-wgpu"), {
    recursive: true,
    filter: (source) => ![".git", "target", "Cargo.lock"].includes(basename(source)),
  });
  writeFileSync(
    join(workspace, "Cargo.toml"),
    `[workspace]\nresolver = "3"\nmembers = ["gpui/gpui-fast-wgpu"]\n\n` +
      `[patch.crates-io]\ngpui-fast = { path = ${JSON.stringify(join(output, "gpui-fast"))} }\n`,
  );
  // The desktop's resolved versions; Cargo may prune or add entries in this copy only.
  cpSync(join(toolchain, "../native/Cargo.lock"), join(workspace, "Cargo.lock"));
  // gpui-fast's rounded-clip tests keep their upstream sibling path.
  const fixture = "src/fast/rounded_clip_fixtures.rs";
  cpSync(join(output, "gpui-fast", fixture), join(workspace, "gpui/gpui", fixture));
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
    const argv = ["cargo", "test", "--workspace", "--lib", "--features", "test-support"];
    return inherit([...argv, ...cargoArgs], { cwd: workspace, env });
  } finally {
    rmSync(workspace, { recursive: true, force: true });
  }
}
