import { expect, test } from "bun:test";
import { existsSync, mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join } from "node:path";
import { layout } from "./source.ts";
import { prepareWorkspace } from "./wgpu.ts";

test("the disposable workspace patches gpui-fast and keeps the desktop lock", async () => {
  const scratch = mkdtempSync(join(tmpdir(), "phux-wgpu-prep-"));
  const paths = layout(join(scratch, "toolchain"));
  for (const [path, text] of Object.entries({
    "gpui-fast-wgpu/Cargo.toml": "[package]\n",
    "gpui-fast-wgpu/target/junk": "x",
    "gpui-fast-wgpu/Cargo.lock": "stale",
    "gpui-fast/src/fast/rounded_clip_fixtures.rs": "// fixture\n",
    "../../native/Cargo.lock": "# desktop lock\n",
  })) {
    mkdirSync(dirname(join(paths.output, path)), { recursive: true });
    writeFileSync(join(paths.output, path), text);
  }
  const workspace = join(scratch, "workspace");
  mkdirSync(workspace);
  let fetched = "";
  await prepareWorkspace(workspace, paths, async (target) => {
    fetched = target;
  });
  expect(fetched).toBe(workspace);
  const read = (path: string) => readFileSync(join(workspace, path), "utf8");
  const patch = `gpui-fast = { path = ${JSON.stringify(join(paths.output, "gpui-fast"))} }`;
  expect(read("Cargo.toml")).toContain(patch);
  expect(read("Cargo.lock")).toBe("# desktop lock\n");
  const copied = [
    "rust-toolchain.toml",
    "gpui/gpui-fast-wgpu/Cargo.toml",
    "gpui/gpui/src/fast/rounded_clip_fixtures.rs",
  ];
  const skipped = ["gpui/gpui-fast-wgpu/target", "gpui/gpui-fast-wgpu/Cargo.lock"];
  expect([...copied, ...skipped].filter((path) => existsSync(join(workspace, path)))).toEqual(
    copied,
  );
  rmSync(scratch, { recursive: true });
});
