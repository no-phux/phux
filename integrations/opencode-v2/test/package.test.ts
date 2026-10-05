import { expect, test } from "bun:test";
import { mkdtemp, mkdir, rm, symlink, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";
import { spawnSync } from "node:child_process";

const packageRoot = resolve(import.meta.dir, "..");

test("packed plugin loads outside the checkout with only its declared dependency", async () => {
  const directory = await mkdtemp(join(tmpdir(), "phux-opencode-pack-"));
  try {
    const archive = join(directory, "plugin.tgz");
    command([process.execPath, "pm", "pack", "--filename", archive], packageRoot);
    command(["tar", "-xzf", archive, "-C", directory], directory);
    const dependencies = join(directory, "node_modules", "@opencode");
    await mkdir(dependencies, { recursive: true });
    await symlink(join(packageRoot, "node_modules", "@opencode", "plugin"), join(dependencies, "plugin"), "dir");
    await symlink(join(packageRoot, "node_modules", "solid-js"), join(directory, "node_modules", "solid-js"), "dir");
    const executable = join(directory, "phux-fixture");
    await writeFile(executable, "#!/bin/sh\nprintf '%s\\n' '{\"schema_version\":1,\"running\":false}'\nexit 1\n", { mode: 0o700 });
    const output = command([process.execPath, "--eval", `
      import plugin from './package/index.js';
      import tui from './package/tui.js';
      if (tui.id !== 'phux.tui') throw new Error('packed terminal entrypoint not loadable');
      const tools = [];
      const cleanup = await plugin.setup({
        options: { contextAwareness: false, executable: ${JSON.stringify(executable)} },
        tool: { transform: async (f) => f({ add: (tool) => tools.push(tool) }), hook: async () => {} },
        session: { hook: async () => {} },
        event: { subscribe: async function* () {} },
      });
      const status = await tools.find(tool => tool.name === 'phux_status').execute({}, {
        sessionID: 'pack', messageID: 'pack', id: 'pack', agent: 'pack',
      });
      if (status.metadata.result.running !== false) throw new Error('packed runtime did not execute the CLI');
      await cleanup?.();
      console.log(JSON.stringify({ id: plugin.id, tools: tools.map(tool => tool.name).sort() }));
    `], directory);
    expect(JSON.parse(output)).toEqual({
      id: "phux",
      tools: [
        "phux_agent_prompt", "phux_agent_wait", "phux_create", "phux_list", "phux_panes",
        "phux_paste", "phux_resource_wait", "phux_run", "phux_runtime_info", "phux_send_keys",
        "phux_snapshot", "phux_spawn", "phux_status", "phux_wait",
      ],
    });
  } finally {
    await rm(directory, { recursive: true, force: true });
  }
}, 30_000);

function command(args: string[], cwd: string): string {
  const result = spawnSync(args[0]!, args.slice(1), { cwd, encoding: "utf8", timeout: 20_000 });
  if (result.error !== undefined) throw result.error;
  if (result.status !== 0) throw new Error(`${args[0]} exited ${result.status}: ${result.stderr}\n${result.stdout}`);
  return result.stdout.trim();
}
