import assert from "node:assert/strict";
import { mkdir, mkdtemp, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { spawnSync } from "node:child_process";

const packageDir = dirname(dirname(fileURLToPath(import.meta.url)));
const temp = await mkdtemp(join(tmpdir(), "phux-pi-load-"));
const archiveDir = join(temp, "archives");
const workDir = join(temp, "work");
const piDir = join(temp, "pi");
const runtimeDir = join(temp, "runtime");
const projectPackage = join(temp, "project-package");

try {
  await Promise.all([
    mkdir(archiveDir), mkdir(workDir), mkdir(piDir), mkdir(runtimeDir), mkdir(projectPackage),
  ]);
  const packed = run("npm", ["pack", "--json", "--ignore-scripts", "--pack-destination", archiveDir], {
    cwd: packageDir,
  });
  const report = JSON.parse(packed.stdout);
  assert.equal(report.length, 1);
  const archive = join(archiveDir, report[0].filename);
  run("tar", ["-xzf", archive, "-C", temp], { cwd: workDir });
  const packedPackage = join(temp, "package");
  run("tar", ["-xzf", archive, "-C", projectPackage, "--strip-components=1"], { cwd: workDir });
  const projectConfigDir = join(workDir, ".pi");
  await mkdir(projectConfigDir);
  await writeFile(join(projectConfigDir, "settings.json"), `${JSON.stringify({
    packages: [projectPackage],
  })}\n`);

  const env = {
    ...process.env,
    PI_CODING_AGENT_DIR: piDir,
    PI_CODING_AGENT_SESSION_DIR: join(temp, "sessions"),
    PHUX_SOCKET: join(runtimeDir, "never-started.sock"),
    XDG_CACHE_HOME: join(temp, "xdg-cache"),
    XDG_CONFIG_HOME: join(temp, "xdg-config"),
    XDG_DATA_HOME: join(temp, "xdg-data"),
    XDG_RUNTIME_DIR: runtimeDir,
    XDG_STATE_HOME: join(temp, "xdg-state"),
  };
  const pi = process.env.PI_BIN ?? "pi";

  run(pi, ["install", packedPackage, "--approve"], { cwd: workDir, env, timeout: 120_000 });
  const rpc = run(pi, [
    "--mode", "rpc",
    "--no-session",
    "--offline",
    "--no-skills",
    "--no-context-files",
    "--approve",
  ], {
    cwd: workDir,
    env,
    input: '{"id":"load-check","type":"get_commands"}\n',
    timeout: 30_000,
  });

  const records = rpc.stdout.trim().split("\n").filter(Boolean).map((line) => JSON.parse(line));
  const response = records.find((record) => record.id === "load-check");
  assert.equal(response?.success, true, `Pi RPC failed to load the packed extension: ${rpc.stdout}`);
  assert.doesNotMatch(
    rpc.stderr,
    /conflicts with|Failed to load extension/,
    `global and project @phux/pi copies must arbitrate without conflicts: ${rpc.stderr}`,
  );
  const commands = response.data.commands
    .filter((command) => command.source === "extension")
    .map((command) => command.name)
    .sort();
  assert.deepEqual(commands, ["phux", "phux-attach", "phux-status"]);

  process.stdout.write(
    `extension load passed: global @phux/pi won over the project copy and registered ${commands.join(", ")} without an LLM\n`,
  );
} finally {
  await rm(temp, { recursive: true, force: true });
}

function run(command, args, options) {
  const result = spawnSync(command, args, {
    encoding: "utf8",
    ...options,
  });
  if (result.error !== undefined) throw result.error;
  if (result.status !== 0) {
    throw new Error(`${command} ${args.join(" ")} failed (${String(result.status)}):\n${result.stderr}\n${result.stdout}`);
  }
  return result;
}
