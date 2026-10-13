/**
 * Local server discovery for the dev launcher.
 * The shipping app resolves the installed CLI itself; this module never
 * substitutes an installed binary into a qualification run.
 */
import { spawnSync } from "node:child_process";
import { existsSync } from "node:fs";
import { homedir } from "node:os";
import { installedPhuxCandidates } from "./host.ts";

export interface CommandResult {
  status: number | null;
  stdout: string;
  stderr: string;
  error?: string;
}

function spawnCommand(binary: string, args: string[]): CommandResult {
  const run = spawnSync(binary, args, { encoding: "utf8", stdio: ["ignore", "pipe", "pipe"] });
  return {
    status: run.status,
    stdout: run.stdout,
    stderr: run.stderr,
    ...(run.error ? { error: run.error.message } : {}),
  };
}

function requireSuccess(result: CommandResult, fallback: string): void {
  if (result.error || result.status !== 0) {
    throw new Error(result.stderr.trim() || result.error || fallback);
  }
}

/** A packaged app starts with launchd's minimal PATH, so look where installs land. */
export function findPhux(explicit?: string, home = homedir()): string | undefined {
  if (explicit) return explicit;
  const found = installedPhuxCandidates(home).find((path) => existsSync(path));
  if (found) return found;
  const which = spawnSync("/usr/bin/which", ["phux"], { encoding: "utf8" });
  const path = which.stdout.trim();
  return which.status === 0 && path ? path : undefined;
}

export function ensureServer(phux: string, socket?: string): void {
  const args = socket ? ["--socket", socket, "server", "--ensure"] : ["server", "--ensure"];
  requireSuccess(spawnCommand(phux, args), "`phux server --ensure` failed");
}

interface Status {
  socket?: unknown;
  sessions?: unknown;
  running?: unknown;
  pid?: unknown;
}

export function serverStatus(phux: string, socket?: string): Status | undefined {
  const args = socket ? ["--socket", socket, "status", "--json"] : ["status", "--json"];
  const parsed = json(phux, args);
  return parsed && typeof parsed === "object" ? parsed : undefined;
}

export function serverSocket(phux: string): string {
  const socket = serverStatus(phux)?.socket;
  if (typeof socket !== "string")
    throw new Error("No running phux server after `phux server --ensure`");
  return socket;
}

export function sessionNames(phux: string, socket: string): string[] {
  const sessions = serverStatus(phux, socket)?.sessions;
  if (!Array.isArray(sessions)) return [];
  return sessions.flatMap((session: unknown) =>
    session !== null &&
    typeof session === "object" &&
    "name" in session &&
    typeof session.name === "string"
      ? [session.name]
      : [],
  );
}

/**
 * Create an explicitly requested session when absent. `phux new` makes a
 * session it is given a name for persistent.
 */
export function ensureSession(phux: string, socket: string, name: string): void {
  if (sessionNames(phux, socket).includes(name)) return;
  if (!json(phux, ["--socket", socket, "new", "--json", "-s", name])) {
    throw new Error(`Could not create phux session "${name}"`);
  }
}

function json(phux: string, args: string[]): object | undefined {
  const run = spawnCommand(phux, args);
  if (run.error) throw new Error(`Could not run ${phux}: ${run.error}`);
  if (run.status !== 0 || !run.stdout) return undefined;
  try {
    const parsed: unknown = JSON.parse(run.stdout);
    return parsed !== null && typeof parsed === "object" ? parsed : undefined;
  } catch {
    return undefined;
  }
}
