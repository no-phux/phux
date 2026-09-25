import { mkdir } from "node:fs/promises";
import { resolve } from "node:path";
import { spawn } from "node:child_process";
import { connectStdio, type App } from "@gpuix/solid/automation";
import { terminateChild } from "./terminate-child";
import { buildDesktopBundle, prepareDesktopFramework } from "./desktop-bundle";

const root = resolve(import.meta.dir, "..");
const output = resolve(root, "dist/framework");
const addon = resolve(root, "toolchain/gpuix/packages/native/gpuix-native.darwin-arm64.node");
if (!(await Bun.file(addon).exists())) {
  throw new Error("Build the matched release addon first: just desktop-source-build");
}
await mkdir(output, { recursive: true });
prepareDesktopFramework();
console.log("Building the production Solid framework fixture...");
const result = await buildDesktopBundle(resolve(root, "tests/framework/window.tsx"), output);
if (!result.success) throw new AggregateError(result.logs, "Production Solid build failed");

console.log("Launching the source-built native host...");
const child = spawn(process.execPath, [resolve(output, "window.js")], {
  cwd: root,
  env: { ...process.env, NAPI_RS_NATIVE_LIBRARY_PATH: addon, GPUIX_BACKGROUND: "1" },
  stdio: ["pipe", "pipe", "pipe"],
});
let stderr = "";
child.stderr.on("data", (chunk: Buffer) => {
  stderr = (stderr + chunk.toString("utf8")).slice(-16_384);
});
const exited = new Promise<never>((_, reject) => {
  child.once("error", reject);
  child.once("exit", (code) => reject(new Error(`Framework process exited ${code}: ${stderr}`)));
});
const deadline = Promise.withResolvers<never>();
const timer = setTimeout(
  () => deadline.reject(new Error(`Framework timed out: ${stderr}`)),
  45_000,
);

let app: App | undefined;

async function exerciseWindow(): Promise<void> {
  app = await connectStdio({
    write: (chunk) => {
      child.stdin.write(chunk);
    },
    feed: (listener) => {
      child.stdout.on("data", (chunk: Buffer) => listener(chunk.toString("utf8")));
    },
    close: () => {
      child.kill();
      return Promise.resolve();
    },
  });
  console.log("Waiting for first native paint...");
  await app.getByText("Count: 0").waitFor({ timeoutMs: 30_000 });
  await app.getByTestId("increment").click();
  await app.getByText("Count: 1").waitFor({ timeoutMs: 5_000 });
  await app.getByTestId("entry").fill("native Solid input");
  await app.getByText("Typed: native Solid input").waitFor({ timeoutMs: 5_000 });
  await app.screenshot({ path: resolve(output, "window.png") });
}

try {
  await Promise.race([exerciseWindow(), exited, deadline.promise]);
} finally {
  clearTimeout(timer);
  await app?.close();
  await terminateChild(child);
}
console.log("Production Solid bundle: native click, text input, GPU capture and cleanup passed.");
