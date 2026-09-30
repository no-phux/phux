import assert from "node:assert/strict";
import { mkdtemp, mkdir, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { dirname, join, resolve } from "node:path";
import type { ExtensionContext } from "@oh-my-pi/pi-coding-agent";
import type { Skill } from "@oh-my-pi/pi-coding-agent/capability/skill";

// Isolate before importing the SDK: discovery reads environment at module load.
// Load the actual packed artifact so missing files and checkout imports fail.
if (process.argv[2] !== "--isolated") {
  const root = await mkdtemp(join(tmpdir(), "phux-omp-load-"));
  try {
    const source = resolve(dirname(import.meta.path), "..");
    const artifact = join(root, "package");
    const archive = join(root, "phux-omp.tgz");
    const packed = Bun.spawnSync([process.execPath, "pm", "pack", "--ignore-scripts", "--filename", archive], { cwd: source });
    assert.equal(packed.exitCode, 0, packed.stderr.toString());
    const extracted = Bun.spawnSync(["tar", "-xzf", archive, "-C", root]);
    assert.equal(extracted.exitCode, 0, extracted.stderr.toString());
    await mkdir(join(root, "home"));
    await mkdir(join(root, "agent"));
    const executable = join(root, "phux-fixture");
    await writeFile(executable, [
      "#!/bin/sh",
      'case "$*" in',
      '  *"new --json"*) printf \'%s\\n\' \'{"session":"smoke","terminal_id":18}\' ;;',
      '  *) printf \'unexpected CLI invocation\\n\' >&2; exit 79 ;;',
      "esac",
      "",
    ].join("\n"), { mode: 0o700 });
    const child = Bun.spawn([process.execPath, import.meta.path, "--isolated", artifact], {
      cwd: root,
      env: {
        PATH: process.env.PATH ?? "/usr/bin:/bin",
        HOME: join(root, "home"),
        PI_CODING_AGENT_DIR: join(root, "agent"),
        XDG_CONFIG_HOME: join(root, "config"),
        XDG_DATA_HOME: join(root, "data"),
        XDG_CACHE_HOME: join(root, "cache"),
        XDG_STATE_HOME: join(root, "state"),
        PHUX_BIN: executable,
        PHUX_SOCKET: join(root, "never-connect.sock"),
        PHUX_TERMINAL_ID: "17",
        NO_COLOR: "1",
      },
      stdout: "inherit",
      stderr: "inherit",
    });
    const deadline = setTimeout(() => child.kill(), 30_000);
    try {
      assert.equal(await child.exited, 0, "native OMP load smoke failed");
    } finally {
      clearTimeout(deadline);
    }
  } finally {
    await rm(root, { recursive: true, force: true });
  }
} else {
  // These imports intentionally occur after process isolation, matching native
  // startup's module-loading boundary rather than touching the user's settings.
  const { discoverExtensionPaths, loadExtensions } = await import("@oh-my-pi/pi-coding-agent/extensibility/extensions");
  const { validateToolArguments } = await import("@oh-my-pi/pi-ai/utils/validation");
  const { injectOmpExtensionCliRoots } = await import("@oh-my-pi/pi-coding-agent/discovery/omp-extension-roots");
  const { loadCapability } = await import("@oh-my-pi/pi-coding-agent/capability");
  const { skillCapability } = await import("@oh-my-pi/pi-coding-agent/capability/skill");
  const artifact = process.argv[3]!;
  injectOmpExtensionCliRoots([artifact], process.env.HOME!, process.cwd());
  const paths = await discoverExtensionPaths([artifact], process.cwd());
  assert.deepEqual(paths, [join(artifact, "dist/index.js")], "explicit directory must resolve its omp manifest");
  const skills = await loadCapability<Skill>(skillCapability.id, { cwd: process.cwd(), providers: ["omp-plugins"] });
  assert.deepEqual(skills.items.map(skill => skill.name), ["using-phux-tools"], "native -e package discovery must expose the shipped skill");
  const loaded = await loadExtensions(paths, process.cwd());
  assert.deepEqual(loaded.errors, [], "native loader rejected package");
  assert.equal(loaded.extensions.length, 1);
  const extension = loaded.extensions[0]!;
  const names = [
    "phux_list", "phux_create", "phux_snapshot", "phux_send_keys", "phux_run", "phux_wait",
    "phux_panes", "phux_paste", "phux_spawn", "phux_agent_prompt", "phux_agent_wait",
    "phux_resource_wait", "phux_status", "phux_runtime_info",
  ];
  assert.deepEqual([...extension.tools.keys()].sort(), names.sort());
  assert.deepEqual([...extension.commands.keys()].sort(), ["phux-attach", "phux-status"]);
  const run = extension.tools.get("phux_run")!.definition;
  const args = { target: "@17", command: "echo unsafe" };
  assert.deepEqual(validateToolArguments(run, { type: "toolCall", id: "valid", name: run.name, arguments: args }), args);
  assert.throws(() => validateToolArguments(run, {
    type: "toolCall", id: "invalid", name: run.name, arguments: { command: "true", timeout_seconds: 0 },
  }), "native host must validate JSON Schema, not merely register it");

  let branch: Array<{ type: "custom"; customType: string; data: unknown }> = [];
  const session = "smoke-session";
  const ctx = {
    cwd: process.cwd(),
    sessionManager: {
      getSessionId: () => session,
      getLeafId: () => "smoke-leaf",
      getBranch: () => branch,
    },
  } as unknown as ExtensionContext;
  loaded.runtime.appendEntry = (customType, data) => { branch.push({ type: "custom", customType, data }); };

  for (const [name, input] of [
    ["phux_run", args],
    ["phux_send_keys", { target: "@17", keys: ["Enter"] }],
    ["phux_paste", { target: "@17", text: "unsafe" }],
    ["phux_agent_prompt", { target: "@17", text: "unsafe" }],
  ] as const) {
    const result = await extension.tools.get(name)!.definition.execute("parent", input, undefined, undefined, ctx);
    assert.equal(result.isError, true, `${name} must refuse the hosting pane`);
    assert.match(JSON.stringify(result), /hosting|host|parent|running|own pane/i);
    assert.doesNotMatch(JSON.stringify(result), /unexpected CLI invocation/);
  }

  const controller = new AbortController();
  controller.abort();
  const cancelled = await extension.tools.get("phux_snapshot")!.definition.execute(
    "cancel", { target: "@18" }, controller.signal, undefined, ctx,
  );
  assert.equal(cancelled.isError, true);
  assert.equal(cancelled.details.error.code, "aborted", "OMP signal must reach the subprocess runtime");

  const create = extension.tools.get("phux_create")!.definition;
  const created = await create.execute("create", { name: "smoke" }, undefined, undefined, ctx);
  assert.notEqual(created.isError, true);
  const selection = branch.at(-1)!.data;
  assert.ok(selection && typeof selection === "object" && "target" in selection);
  assert.equal(selection.target, "@18");
  // Resumed branch history, not process-global focus, determines selection.
  branch = [{ type: "custom", customType: "sh.phux.omp.selected-target", data: { version: 1, target: "@17" } }];
  const restored = await run.execute("restored", { command: "unsafe" }, undefined, undefined, ctx);
  assert.equal(restored.isError, true);
  assert.doesNotMatch(JSON.stringify(restored), /unexpected CLI invocation/);

  branch = [];
  const inFlight = create.execute("racing-create", { name: "smoke" }, undefined, undefined, ctx);
  for (const handler of extension.handlers.get("session_tree") ?? []) {
    await handler({ type: "session_tree" }, ctx);
  }
  const completed = await inFlight;
  assert.notEqual(completed.isError, true, "created terminal must remain available after session navigation");
  assert.deepEqual(branch, [], "late create must not select a terminal on the new session/branch");
  const missing = await run.execute("unselected", { command: "unsafe" }, undefined, undefined, ctx);
  assert.equal(missing.isError, true, "a new branch must not fall back to terminal focus");
  assert.doesNotMatch(JSON.stringify(missing), /unexpected CLI invocation/);
  console.log("Native OMP load smoke passed: isolated bundled package, 14 tools, JSON Schema validation, self-pane guard, cancellation, branch selection and navigation race.");
}
