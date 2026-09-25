import assert from "node:assert/strict";
import { mkdir, writeFile, readFile } from "node:fs/promises";
import { resolve } from "node:path";
import { spawn } from "node:child_process";
import { connectStdio, type App } from "@gpuix/solid/automation";
import { buildDesktopBundle, prepareDesktopFramework } from "./desktop-bundle";
import { terminateChild } from "./terminate-child";

const root = resolve(import.meta.dir, "..");
const output = resolve(root, ".cache/terminal-feasibility");
const addon = process.env.PHUX_DESKTOP_ADDON;
const home = process.env.PHUX_FEASIBILITY_HOME;
if (!addon || !home) throw new Error("Run tests/feasibility/run.sh with PHUX_DESKTOP_ADDON");
const editorFile = resolve(home, "editor.txt");
await mkdir(output, { recursive: true });
prepareDesktopFramework();
const result = await buildDesktopBundle(resolve(root, "tests/feasibility/entry.ts"), output);
if (!result.success) throw new AggregateError(result.logs, "Production Solid build failed");

const child = spawn(process.execPath, [resolve(output, "entry.js")], {
  cwd: root,
  env: { ...process.env, GPUIX_BACKGROUND: "1" },
  stdio: ["pipe", "pipe", "pipe"],
});
let stderr = "";
child.stderr.on("data", (chunk: Buffer) => {
  stderr = (stderr + chunk.toString("utf8")).slice(-32_768);
});
const exited = new Promise<never>((_, reject) => {
  child.once("error", reject);
  child.once("exit", (code) => reject(new Error(`Terminal process exited ${code}: ${stderr}`)));
});
const deadline = Promise.withResolvers<never>();
const timer = setTimeout(
  () => deadline.reject(new Error(`Terminal fixture timed out: ${stderr}`)),
  60_000,
);
let app: App | undefined;

async function exercise(): Promise<void> {
  app = await connectStdio({
    write: (chunk) => {
      child.stdin.write(chunk);
    },
    feed: (listener) => {
      child.stdout.on("data", (chunk: Buffer) => listener(chunk.toString("utf8")));
    },
    close: () => Promise.resolve(),
  });
  await app.getByTestId("right-terminal").waitFor({ timeoutMs: 25_000 });
  assert.equal(await app.getByType("phux-terminal").count(), 2);
  await app.getByTestId("shell").click();
  await app
    .getByText("Shell: ready | Editor: pending | Independent: pending")
    .waitFor({ timeoutMs: 10_000 });
  await app.screenshot({ path: resolve(output, "shell.png") });
  await app.getByTestId("separate").click();
  await app
    .getByText("Shell: ready | Editor: pending | Independent: ready")
    .waitFor({ timeoutMs: 5_000 });
  await app.screenshot({ path: resolve(output, "independent-views.png") });
  await app.getByTestId("editor").click();
  await app.getByText("Vim: ready").waitFor({ timeoutMs: 10_000 });
  await app.getByTestId("insert").click();
  await app
    .getByText("Shell: ready | Editor: ready | Independent: ready")
    .waitFor({ timeoutMs: 10_000 });
  await app.screenshot({ path: resolve(output, "editor.png") });
  await app.getByTestId("save").click();
  await waitForSavedFile();
  await app.getByTestId("close").click();
  await app
    .getByText("Status: closed; views destroyed; final events processed")
    .waitFor({ timeoutMs: 5_000 });
  assert.equal(await app.getByType("phux-terminal").count(), 0);
  await app.screenshot({ path: resolve(output, "closed.png") });
}

async function waitForSavedFile(): Promise<void> {
  const until = Date.now() + 10_000;
  while ((await readFile(editorFile, "utf8")) !== "EDITOR UTF-8: 界 é \u{1F642}\n") {
    assert.ok(Date.now() < until, "Vim did not save the owned fixture file");
    await Bun.sleep(25);
  }
  assert.equal(await readFile(editorFile, "utf8"), "EDITOR UTF-8: 界 é \u{1F642}\n");
}

try {
  await Promise.race([exercise(), exited, deadline.promise]);
} finally {
  clearTimeout(timer);
  try {
    await app?.close();
  } finally {
    await terminateChild(child);
  }
  await writeFile(resolve(output, "stderr.log"), stderr);
}
assert.equal(child.exitCode, 0, `Native host teardown failed (${child.signalCode}): ${stderr}`);
console.log(
  `Production Solid terminal fixture passed; Metal captures and event receipt: ${output}`,
);
