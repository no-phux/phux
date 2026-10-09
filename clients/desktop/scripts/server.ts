/**
 * Local server discovery shared by the dev launcher and the packaged app.
 * The `phux` CLI owns the socket: `server --ensure` starts or reuses the
 * server for the active profile, and `status --json` reads its path back.
 */
import { existsSync } from "node:fs";
import { homedir } from "node:os";
import { join } from "node:path";
import { spawnSync } from "node:child_process";
import {
  createSessionArgs,
  createSessionError,
  renameSessionArgs,
  renameSessionError,
} from "../src/workspace/session-cli";

/** What a session command's process returned. `status` null means it did not exit. */
export interface CommandResult {
  status: number | null;
  stderr: string;
}

export type CommandRunner = (binary: string, args: string[]) => CommandResult;

function spawnCommand(binary: string, args: string[]): CommandResult {
  const run = spawnSync(binary, args, { encoding: "utf8", stdio: ["ignore", "pipe", "pipe"] });
  if (run.error) return { status: 1, stderr: run.error.message };
  return { status: run.status, stderr: run.stderr };
}

function requireSuccess(result: CommandResult, fallback: string): void {
  if (result.status === 0) return;
  const detail = result.stderr.trim();
  throw new Error(detail || `${fallback}: exit ${result.status ?? "unknown"}`);
}

/** A packaged app starts with launchd's minimal PATH, so look where installs land. */
export function findPhux(explicit?: string): string | undefined {
  if (explicit) return explicit;
  const candidates = [
    join(homedir(), ".local/bin/phux"),
    "/opt/homebrew/bin/phux",
    "/usr/local/bin/phux",
    join(homedir(), ".cargo/bin/phux"),
  ];
  const found = candidates.find((path) => existsSync(path));
  if (found) return found;
  const which = spawnSync("/usr/bin/which", ["phux"], { encoding: "utf8" });
  const path = which.stdout.trim();
  return which.status === 0 && path ? path : undefined;
}

export function ensureServer(phux: string, socket?: string): void {
  const args = socket ? ["--socket", socket, "server", "--ensure"] : ["server", "--ensure"];
  const run = spawnSync(phux, args, { encoding: "utf8", stdio: ["ignore", "ignore", "pipe"] });
  if (run.error) throw new Error(`Could not run ${phux}: ${run.error.message}`);
  if (run.status !== 0) {
    throw new Error(
      `\`phux server --ensure\` failed: ${run.stderr.trim() || `exit ${run.status}`}`,
    );
  }
}

interface Status {
  socket?: unknown;
  sessions?: unknown;
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
 * Create a session on `socket` whose seed pane starts in `directory`.
 * An empty directory is refused before any process starts, so the desktop
 * launcher's own cwd cannot become the session directory.
 */
export function createNamedSession(
  phux: string,
  socket: string,
  name: string,
  directory: string,
  run: CommandRunner = spawnCommand,
): void {
  const problem = createSessionError(name, directory);
  if (problem) throw new Error(problem);
  requireSuccess(
    run(phux, createSessionArgs(socket, name.trim(), directory)),
    "Could not create the session",
  );
}

/** Rename a session on `socket`. An unchanged name sends nothing. */
export function renameNamedSession(
  phux: string,
  socket: string,
  current: string,
  next: string,
  run: CommandRunner = spawnCommand,
): void {
  const problem = renameSessionError(current, next);
  if (problem) throw new Error(problem);
  if (current.trim() === next.trim()) return;
  requireSuccess(
    run(phux, renameSessionArgs(socket, current.trim(), next.trim())),
    "Could not rename the session",
  );
}

/** The desktop attaches an existing session; create the requested one when absent. */
export function ensureSession(phux: string, socket: string, name: string): void {
  if (sessionNames(phux, socket).includes(name)) return;
  if (!json(phux, ["--socket", socket, "new", "--json", "-s", name])) {
    throw new Error(`Could not create phux session "${name}"`);
  }
}

/** An explicit choice, else `default`, else the first session, else a new `default`. */
export function pickSession(phux: string, socket: string, requested?: string): string {
  if (requested) return requested;
  const names = sessionNames(phux, socket);
  if (names.includes("default")) return "default";
  return names[0] ?? "default";
}

function json(phux: string, args: string[]): object | undefined {
  const run = spawnSync(phux, args, { encoding: "utf8", stdio: ["ignore", "pipe", "pipe"] });
  if (run.error) throw new Error(`Could not run ${phux}: ${run.error.message}`);
  if (run.status !== 0 || !run.stdout) return undefined;
  try {
    const parsed: unknown = JSON.parse(run.stdout);
    return parsed !== null && typeof parsed === "object" ? parsed : undefined;
  } catch {
    return undefined;
  }
}
