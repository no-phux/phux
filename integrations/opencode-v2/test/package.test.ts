import { expect, test } from "bun:test";
import { mkdtemp, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";
import { spawnSync } from "node:child_process";

const packageRoot = resolve(import.meta.dir, "..");
// OpenCode 2.2.6's runtime. Keep this independent of the plugin manifest so
// dependency refreshes cannot silently stop testing the actual host contract.
const hostPeers = {
  "@opentui/core": "0.8.1",
  "@opentui/solid": "0.8.1",
  "solid-js": "1.9.17",
};

test("declared peers accept the OpenCode 2.2.6 host runtime", async () => {
  const manifest = await Bun.file(join(packageRoot, "package.json")).json();
  for (const [name, version] of Object.entries(hostPeers)) {
    expect(Bun.semver.satisfies(version, manifest.peerDependencies[name])).toBe(true);
  }
});

test("packed plugin shares the host OpenTUI and Solid runtime outside the checkout", async () => {
  const directory = await mkdtemp(join(tmpdir(), "phux-opencode-pack-"));
  try {
    const archive = join(directory, "plugin.tgz");
    command([process.execPath, "pm", "pack", "--filename", archive], packageRoot);
    await writeFile(join(directory, "package.json"), JSON.stringify({
      private: true,
      dependencies: {
        "@phux/opencode": archive,
        ...hostPeers,
      },
    }));
    command([process.execPath, "install"], directory);
    const executable = join(directory, "phux-fixture");
    await writeFile(executable, "#!/bin/sh\nprintf '%s\\n' '{\"schema_version\":1,\"running\":false}'\nexit 1\n", { mode: 0o700 });
    const output = command([process.execPath, "--eval", `
      import { Host } from '@opencode/plugin/host';
       const entrypoints = Host.resolve({ directory: ${JSON.stringify(directory)}, name: '@phux/opencode' });
      if (!entrypoints.server || !entrypoints.tui || !entrypoints.gui || entrypoints.rpc) {
        throw new Error('native loader must discover exactly the packed server, TUI, and GUI entrypoints');
      }
      // The real CLI loads its runtime first. Loading a plugin in a fresh process
      // alone misses incompatible OpenTUI environment registration and Solid roots.
      for (const name of ['@opentui/core', '@opentui/solid', 'solid-js']) {
        const host = Bun.resolveSync(name, ${JSON.stringify(directory)});
        const plugin = Bun.resolveSync(name, entrypoints.tui);
        if (plugin !== host) throw new Error(name + ' must resolve to the host runtime, not a private copy');
        await import(host);
      }
      const { default: plugin } = await Host.load(entrypoints.server);
      const { default: tui } = await Host.load(entrypoints.tui);
      if (tui.id !== 'phux.tui') throw new Error('packed terminal entrypoint not loadable');
      const tools = [];
      const rpcs = [];
      const cleanup = await plugin.setup({
        rpc: { register: async (definition) => { rpcs.push(definition.id); return { dispose: async () => {} }; } },
        options: { contextAwareness: false, executable: ${JSON.stringify(executable)} },
        tool: { transform: async (f) => f({ add: (tool) => tools.push(tool) }), hook: async () => {} },
        session: { hook: async () => {} },
        shell: { hook: async () => {} },
        event: { subscribe: async function* () {} },
      });
      const status = await tools.find(tool => tool.name === 'phux_status').execute({}, {
        sessionID: 'pack', messageID: 'pack', id: 'pack', agent: 'pack',
      });
      if (status.metadata.result.running !== false) throw new Error('packed runtime did not execute the CLI');
      await cleanup?.();
      console.log(JSON.stringify({ id: plugin.id, rpcs, gui: entrypoints.gui.endsWith('dist/gui-experimental.cjs'), tools: tools.map(tool => tool.name).sort() }));
    `], directory);
    expect(JSON.parse(output)).toEqual({
      id: "phux",
      rpcs: ["phux"],
      gui: true,
      tools: [
        "phux_agent_prompt", "phux_agent_wait", "phux_kill", "phux_panes", "phux_paste",
        "phux_resource_wait", "phux_run", "phux_send_keys", "phux_snapshot", "phux_spawn",
        "phux_status", "phux_wait",
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
