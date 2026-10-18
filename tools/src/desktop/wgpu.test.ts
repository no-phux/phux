import { afterAll, expect, test } from "bun:test";
import { existsSync, mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join } from "node:path";
import { layout } from "./source.ts";
import { prepareWorkspace } from "./wgpu.ts";

const scratch = mkdtempSync(join(tmpdir(), "phux-wgpu-prep-"));
afterAll(() => rmSync(scratch, { recursive: true, force: true }));

function write(path: string, text: string): void {
  mkdirSync(dirname(path), { recursive: true });
  writeFileSync(path, text);
}

test("the disposable workspace patches gpui-fast and keeps the desktop lock", async () => {
  const toolchain = join(scratch, "clients/desktop/toolchain");
  const paths = layout(toolchain);
  write(join(paths.output, "gpui-fast-wgpu/Cargo.toml"), "[package]\n");
  write(join(paths.output, "gpui-fast-wgpu/target/junk"), "x");
  write(join(paths.output, "gpui-fast-wgpu/Cargo.lock"), "stale");
  write(join(paths.output, "gpui-fast/src/fast/rounded_clip_fixtures.rs"), "// fixture\n");
  write(join(toolchain, "../native/Cargo.lock"), "# desktop lock\n");
  const workspace = join(scratch, "workspace");
  mkdirSync(workspace);
  let fetched = "";
  await prepareWorkspace(workspace, paths, async (target) => {
    fetched = target;
  });
  expect(fetched).toBe(workspace);
  expect(readFileSync(join(workspace, "Cargo.toml"), "utf8")).toContain(
    `gpui-fast = { path = ${JSON.stringify(join(paths.output, "gpui-fast"))} }`,
  );
  expect(readFileSync(join(workspace, "Cargo.lock"), "utf8")).toBe("# desktop lock\n");
  expect(existsSync(join(workspace, "rust-toolchain.toml"))).toBe(true);
  expect(existsSync(join(workspace, "gpui/gpui-fast-wgpu/Cargo.toml"))).toBe(true);
  expect(existsSync(join(workspace, "gpui/gpui-fast-wgpu/target"))).toBe(false);
  expect(existsSync(join(workspace, "gpui/gpui-fast-wgpu/Cargo.lock"))).toBe(false);
  expect(existsSync(join(workspace, "gpui/gpui/src/fast/rounded_clip_fixtures.rs"))).toBe(true);
});
