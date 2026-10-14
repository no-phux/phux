/**
 * Launch the checkout-built GPUI executable against a checkout-built CLI.
 * Does not fall back to an installed `phux` or a production socket.
 */
import { existsSync } from "node:fs";
import { resolve } from "node:path";
import { spawn } from "node:child_process";
import type { ChildProcess } from "node:child_process";
import { desktopBinary, desktopRoot, isInstalledPhux, layoutFile, repoRoot } from "./host.ts";
import { ensureServer, ensureSession, serverSocket, serverStatus } from "./server.ts";

const root = desktopRoot;
const repo = repoRoot;

/**
 * Without `session` the app opens the owner's Intake (or the most recent
 * session) and keys its layout to that session itself.
 */
export function desktopArguments(options: {
  socket: string;
  session?: string;
  phux: string;
  layout?: string;
  evidence?: string;
  smoke?: boolean;
  fresh?: boolean;
  quick?: boolean;
  terminal?: string;
}): string[] {
  const args = ["--socket", options.socket, "--phux", options.phux];
  if (options.session) args.push("--session", options.session);
  if (options.layout) args.push("--layout", options.layout);
  if (options.evidence) args.push("--evidence", options.evidence);
  if (options.smoke) args.push("--smoke");
  if (options.fresh) args.push("--fresh");
  if (options.quick) args.push("--quick");
  if (options.terminal) args.push("--terminal", options.terminal);
  return args;
}

export function launchDesktop(
  binary: string,
  args: string[],
  env: NodeJS.ProcessEnv,
): ChildProcess {
  return spawn(binary, args, { cwd: root, env, stdio: "inherit" });
}

function requiredBinary(path: string | undefined, fallback: string, label: string): string {
  const chosen = path ?? fallback;
  if (!existsSync(chosen)) throw new Error(`Build the ${label} first: ${chosen}`);
  if (isInstalledPhux(chosen)) throw new Error(`refusing installed phux CLI: ${chosen}`);
  return chosen;
}

if (import.meta.main) {
  const desktop = requiredBinary(
    process.env.PHUX_DESKTOP_BIN,
    desktopBinary("debug"),
    "native desktop (just desktop-app)",
  );
  const phux = requiredBinary(
    process.env.PHUX_BIN,
    resolve(repo, "target/debug/phux"),
    "checkout CLI (just desktop-cli)",
  );
  const demo = process.env.PHUX_DESKTOP_DEMO === "1";
  if (demo) {
    if (!process.env.PHUX_BIN || !process.env.PHUX_SOCKET || !process.env.PHUX_DESKTOP_BIN) {
      throw new Error("Demo launcher requires an explicit CLI, socket, and desktop binary");
    }
    if (!serverStatus(phux, process.env.PHUX_SOCKET))
      throw new Error("Private demo server is not running");
  } else {
    ensureServer(phux, process.env.PHUX_SOCKET);
  }
  const socketPath = process.env.PHUX_SOCKET || serverSocket(phux);
  // An explicit session is created when missing; otherwise the app opens the Intake.
  const sessionName = process.env.PHUX_SESSION || undefined;
  if (!demo && sessionName) ensureSession(phux, socketPath, sessionName);
  const state = process.env.XDG_STATE_HOME ?? resolve(process.env.HOME ?? "", ".local/state");
  const layout =
    process.env.PHUX_DESKTOP_LAYOUT ??
    (sessionName ? layoutFile(state, socketPath, sessionName) : undefined);
  const child = launchDesktop(
    desktop,
    desktopArguments({
      socket: socketPath,
      ...(sessionName ? { session: sessionName } : {}),
      phux,
      ...(layout ? { layout } : {}),
      ...(process.env.PHUX_DESKTOP_EVIDENCE ? { evidence: process.env.PHUX_DESKTOP_EVIDENCE } : {}),
      ...(process.env.PHUX_DESKTOP_FRESH === "1" ? { fresh: true } : {}),
      ...(process.env.PHUX_DESKTOP_QUICK === "1" ? { quick: true } : {}),
    }),
    {
      ...process.env,
      PHUX_BIN: phux,
      PHUX_SOCKET: socketPath,
      ...(sessionName ? { PHUX_SESSION: sessionName } : {}),
    },
  );
  child.once("error", (error) => {
    console.error(error);
    process.exitCode = 1;
  });
  child.once("exit", (code) => process.exit(code ?? 1));
}
