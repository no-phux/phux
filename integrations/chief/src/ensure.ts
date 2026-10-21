import { spawn } from "node:child_process";
import { hostname } from "node:os";
import { fileURLToPath } from "node:url";

import type { PhuxCli } from "@phux/integration-runtime/adapter";

import { chiefDisabled, chiefPaths, loadConfig, shortHost } from "./config.js";
import { CHIEF_ROLE } from "./phux-stream.js";

export type EnsureCli = Pick<PhuxCli, "status" | "tag" | "create" | "spawn" | "agentList">;

export type EnsureOutcome =
  | { readonly kind: "disabled" | "elsewhere" | "no-server" }
  | { readonly kind: "running" | "started"; readonly pane: string };

/**
 * Supervision, step one: make sure the one chief pane exists on the chief
 * host. Idempotent, so launchd can run it at load and every minute. Step two
 * is `supervise`, which runs inside that pane and restarts a crashed chief;
 * the conversation itself survives either restart in its SQLite storage.
 */
export async function ensure(cli: EnsureCli, env: NodeJS.ProcessEnv = process.env, here = shortHost(hostname())): Promise<EnsureOutcome> {
  const paths = chiefPaths(env);
  if (chiefDisabled(paths)) return { kind: "disabled" };
  const config = loadConfig(paths, env);
  if (shortHost(config.host) !== here) return { kind: "elsewhere" };
  try {
    await cli.status({ timeoutMs: 5_000 });
  } catch {
    return { kind: "no-server" };
  }
  const tagged = await cli.tag("ls", `#${CHIEF_ROLE}`, [], { timeoutMs: 5_000 }).catch(() => []);
  if (tagged.length > 0) return { kind: "running", pane: tagged[0]!.terminal };

  const command = [process.execPath, fileURLToPath(new URL("./main.js", import.meta.url)), "supervise"];
  let pane: string;
  try {
    const created = await cli.create(config.session, { cwd: config.cwd, command, timeoutMs: 10_000 });
    pane = `@${created.terminal_id}`;
  } catch {
    // The session already exists: place the chief beside one of its panes.
    const panes = await cli.agentList({ timeoutMs: 5_000 });
    const sibling = panes.agents.find((candidate) => candidate.session === config.session && !candidate.terminal.includes("/"));
    if (sibling === undefined) throw new Error(`session ${config.session} exists but has no local pane to place the chief beside`);
    const spawned = await cli.spawn({ target: sibling.terminal, split: "vertical", cwd: config.cwd, command, timeoutMs: 10_000 });
    pane = `@${spawned.terminal_id}`;
  }
  // The chief tags itself on start; tagging here too closes the window in which a second ensure would spawn again.
  await cli.tag("add", pane, [CHIEF_ROLE], { timeoutMs: 5_000 }).catch(() => []);
  return { kind: "started", pane };
}

/** Restart a chief that exits, backing off while it keeps failing fast. Runs in the chief pane. */
export async function supervise(argv: readonly string[] = [], env: NodeJS.ProcessEnv = process.env): Promise<never> {
  let delay = 2;
  for (;;) {
    const started = Date.now();
    const code = await new Promise<number | null>((resolve) => {
      const child = spawn(process.execPath, [fileURLToPath(new URL("./main.js", import.meta.url)), "run", ...argv], { stdio: "inherit", env });
      child.on("exit", (exitCode) => resolve(exitCode));
      child.on("error", () => resolve(null));
    });
    if (chiefDisabled(chiefPaths(env))) {
      process.stdout.write("\r\nchief disabled; supervisor exiting\r\n");
      process.exit(0);
    }
    delay = Date.now() - started > 60_000 ? 2 : Math.min(delay * 2, 120);
    process.stdout.write(`\r\nchief exited (${code ?? "signal"}); restarting in ${delay}s\r\n`);
    await new Promise((resolve) => setTimeout(resolve, delay * 1000));
  }
}
