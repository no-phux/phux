/**
 * Paths and hard guards shared by launch, package, smoke and perf.
 * Qualification never discovers an installed CLI or a production socket.
 */
import { createHash } from "node:crypto";
import { existsSync, realpathSync, statSync } from "node:fs";
import { homedir } from "node:os";
import { isAbsolute, join, relative, resolve } from "node:path";

export const desktopRoot = resolve(import.meta.dir, "..");
export const repoRoot = resolve(desktopRoot, "../..");

export type HostProfile = "debug" | "release";

/** Standalone GPUI executable. Cargo must be pointed at native/target. */
export function desktopBinary(profile: HostProfile): string {
  return resolve(desktopRoot, "native/target", profile, "phux-desktop");
}

/** sha256(JSON.stringify([socket, session])) hex, first 24 chars. */
export function layoutScope(socket: string, session: string): string {
  return createHash("sha256")
    .update(JSON.stringify([socket, session]))
    .digest("hex")
    .slice(0, 24);
}

export function layoutFile(stateHome: string, socket: string, session: string): string {
  return join(stateHome, "phux-desktop", layoutScope(socket, session), "layout.json");
}

/** Common installed CLI locations; qualification must never use these. */
export function installedPhuxCandidates(home = homedir()): string[] {
  return [
    join(home, ".local/bin/phux"),
    "/opt/homebrew/bin/phux",
    "/usr/local/bin/phux",
    "/usr/bin/phux",
    "/bin/phux",
    join(home, ".cargo/bin/phux"),
  ];
}

/** Whether `path` is (or resolves to) an installed phux CLI candidate. */
export function isInstalledPhux(path: string, home = homedir()): boolean {
  return installedPhuxCandidates(home).some((candidate) => sameFile(path, candidate));
}

function sameFile(left: string, right: string): boolean {
  if (resolve(left) === resolve(right)) return true;
  if (!existsSync(left) || !existsSync(right)) return false;
  try {
    return realpathSync(left) === realpathSync(right);
  } catch {
    return false;
  }
}
export function within(parent: string, child: string): boolean {
  const path = relative(parent, child);
  return path === "" || (path !== ".." && !path.startsWith("../") && !isAbsolute(path));
}

/**
 * Production sockets a qualification run must not attach to.
 * A socket under the temp directory is a harness, even if the path shape matches.
 */
export function productionSocketReason(
  socket: string,
  env: Readonly<Record<string, string | undefined>> = process.env,
): string | undefined {
  const resolved = resolve(socket);
  const tmpdir = env["TMPDIR"];
  const temp = tmpdir ? resolve(tmpdir) : undefined;
  if (temp && (resolved === temp || resolved.startsWith(`${temp}/`))) return undefined;
  const user = env["USER"] ?? "";
  const uid = env["UID"] ?? "";
  const banned = [
    user ? `/tmp/phux-${user}/phux.sock` : undefined,
    uid ? `/run/user/${uid}/phux/phux.sock` : undefined,
    env["XDG_RUNTIME_DIR"] ? join(env["XDG_RUNTIME_DIR"], "phux/phux.sock") : undefined,
  ].filter((path): path is string => path !== undefined);
  if (banned.some((path) => resolve(path) === resolved)) {
    return `refusing production socket ${socket}`;
  }
  return undefined;
}

export function assertCheckoutBinary(path: string, repo = repoRoot): void {
  if (!existsSync(path)) throw new Error(`checkout binary does not exist: ${path}`);
  const real = realpathSync(path);
  if (!statSync(real).isFile()) throw new Error(`checkout binary is not a regular file: ${path}`);
  if (!within(repo, real)) {
    throw new Error(
      "qualification requires a checkout-built binary (no installed or external artifacts)",
    );
  }
  if (isInstalledPhux(real)) {
    throw new Error(`refusing installed phux CLI: ${path}`);
  }
}

export function assertIsolatedTarget(phux: string, socket: string, repo = repoRoot): void {
  assertCheckoutBinary(phux, repo);
  const reason = productionSocketReason(socket);
  if (reason) throw new Error(reason);
}
