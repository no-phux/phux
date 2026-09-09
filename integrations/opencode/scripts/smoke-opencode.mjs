import assert from "node:assert/strict";
import { execFileSync, spawn } from "node:child_process";
import { mkdir, mkdtemp, rm, writeFile } from "node:fs/promises";
import { createServer } from "node:net";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";

const packageRoot = fileURLToPath(new URL("../", import.meta.url));
const entry = join(packageRoot, "dist", "index.js");
const opencode = process.env.OPENCODE_BIN ?? "opencode2";
const temporaryRoot = await mkdtemp(join(tmpdir(), "phux-opencode-runtime-"));
const configDirectory = join(temporaryRoot, "config", "opencode");
const projectDirectory = join(temporaryRoot, "project");
const pluginUrl = pathToFileURL(entry).href;
const password = "phux-opencode-smoke";
let child;

try {
  await mkdir(configDirectory, { recursive: true });
  await mkdir(projectDirectory, { recursive: true });
  execFileSync("git", ["init", "--quiet"], { cwd: projectDirectory });
  await mkdir(join(projectDirectory, ".opencode", "plugins"), { recursive: true });
  const config = `${JSON.stringify({ plugins: [pluginUrl] }, null, 2)}\n`;
  await writeFile(join(configDirectory, "opencode.json"), config);
  await writeFile(
    join(projectDirectory, ".opencode", "plugins", "phux.js"),
    `export { default } from ${JSON.stringify(pluginUrl)};\n`,
  );
  const port = await availablePort();
  const env = {
    ...process.env,
    HOME: temporaryRoot,
    XDG_CONFIG_HOME: join(temporaryRoot, "config"),
    XDG_DATA_HOME: join(temporaryRoot, "data"),
    XDG_CACHE_HOME: join(temporaryRoot, "cache"),
    XDG_STATE_HOME: join(temporaryRoot, "state"),
    OPENCODE_DISABLE_AUTOUPDATE: "1",
    OPENCODE_CONFIG_CONTENT: config,
    OPENCODE_SERVER_PASSWORD: password,
    PHUX_CONTEXT_AWARENESS: "0",
  };
  child = spawn(opencode, ["serve", "--hostname", "127.0.0.1", "--port", String(port), "--log-level", "debug"], {
    cwd: projectDirectory,
    env,
    stdio: ["ignore", "pipe", "pipe"],
  });
  let diagnostics = "";
  child.stdout.on("data", (chunk) => { diagnostics += chunk.toString(); });
  child.stderr.on("data", (chunk) => { diagnostics += chunk.toString(); });
  const url = `http://127.0.0.1:${String(port)}`;
  const headers = { authorization: `Basic ${Buffer.from(`opencode:${password}`).toString("base64")}` };
  await waitForHealth(url, headers, () => diagnostics);
  const configEndpoint = new URL("/api/config", url);
  configEndpoint.searchParams.set("location[directory]", projectDirectory);
  const response = await fetch(configEndpoint, { headers });
  assert.equal(response.ok, true, `config endpoint failed: ${response.status.toString()} ${diagnostics}`);
  const sources = await response.json();
  assert.ok(Array.isArray(sources), `unexpected config response: ${JSON.stringify(sources)}\n${diagnostics}`);
  assert.ok(
    sources.some((source) => source?.info?.plugins?.includes(pluginUrl)),
    `OpenCode did not accept the isolated plugin spec: ${JSON.stringify(sources)}\n${diagnostics}`,
  );
  process.stdout.write(`OpenCode accepted ${pluginUrl} in an isolated server config.\n`);
} finally {
  if (child !== undefined && child.exitCode === null) {
    child.kill("SIGTERM");
    await Promise.race([
      new Promise((resolve) => child.once("exit", resolve)),
      new Promise((resolve) => setTimeout(resolve, 5_000)),
    ]);
    if (child.exitCode === null) child.kill("SIGKILL");
  }
  await rm(temporaryRoot, { recursive: true, force: true });
}

async function availablePort() {
  const server = createServer();
  await new Promise((resolve, reject) => {
    server.once("error", reject);
    server.listen(0, "127.0.0.1", resolve);
  });
  const address = server.address();
  assert.notEqual(address, null);
  assert.notEqual(typeof address, "string");
  await new Promise((resolve, reject) => server.close((error) => error === undefined ? resolve() : reject(error)));
  return address.port;
}

async function waitForHealth(url, headers, diagnostics) {
  const deadline = Date.now() + 60_000;
  while (Date.now() < deadline) {
    if (child.exitCode !== null) throw new Error(`OpenCode exited ${String(child.exitCode)} before readiness:\n${diagnostics()}`);
    try {
      const response = await fetch(`${url}/api/health`, { headers });
      if (response.ok) return;
    } catch {}
    await new Promise((resolve) => setTimeout(resolve, 100));
  }
  throw new Error(`OpenCode did not become healthy:\n${diagnostics()}`);
}
