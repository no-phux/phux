// Leave disk for the mbx objects-mode post step (phux-gsmcg, phux-2mxuz).
//
// The post step copies the store into an export directory and actions/cache
// compresses it. On ext4 the managed target is a second copy of the store, so
// run 38505694112 passed its tests and then hit ENOSPC on export. Never run
// mbx GC here: pending export groups reference the results GC evicts. Instead
// link duplicate target bytes to the store, drop preinstalled toolchains, and
// if the copy still cannot fit, retire the pending receipts so the post step
// records an empty export and the job stays green.
import { existsSync, readdirSync, rmSync, statSync } from "node:fs";
import { join } from "node:path";
import { describe, Failure } from "../lib/command.ts";
import { status } from "../lib/output.ts";
import { capture, inherit } from "../lib/process.ts";
import { collapse } from "./collapse.ts";

const NAME = "mbx-export-headroom";
type Env = Record<string, string | undefined>;

/** Preinstalled runner toolchains the setup step deletes only under 40 GiB free. */
const RUNNER_TOOLCHAINS = [
  "/usr/share/dotnet",
  "/usr/local/lib/android",
  "/opt/ghc",
  "/usr/local/.ghcup",
  "/opt/hostedtoolcache",
];

/** Run `argv` with the caller's stdio; a missing program is exit 127, not a throw. */
function tryInherit(argv: string[]): number {
  try {
    return inherit(argv);
  } catch {
    return 127;
  }
}

const isDir = (path: string): boolean => existsSync(path) && statSync(path).isDirectory();
const warn = (message: string): void => status("warn", `${NAME}: ${message}`);

/** The Cargo home to clean; an empty HOME would make it `/.cargo`, never the root. */
export function cargoHome(env: Env): string {
  const home = env["CARGO_HOME"] || `${env["HOME"] ?? ""}/.cargo`;
  if (!home.startsWith("/") || ["/", "/.cargo", "//.cargo"].includes(home)) {
    throw new Failure(`${NAME}: refusing to clean an empty Cargo home`);
  }
  return home;
}

export function mbxCacheDir(env: Env): string {
  return env["MBX_CACHE_DIR"] || `${env["XDG_CACHE_HOME"] || `${env["HOME"] ?? ""}/.cache`}/mbx`;
}

/** The export copy, a zstd archive at about a quarter of the store, and a 1 GiB floor. */
export function exportNeedKb(storeKb: number): number {
  return storeKb + Math.floor(storeKb / 4) + 1024 * 1024;
}

/** The first number a command prints in `column` of `row`, or undefined. */
function measure(argv: string[], row: number, column: number): number | undefined {
  try {
    const field = capture(argv).split("\n")[row]?.trim().split(/\s+/)[column];
    return field !== undefined && /^[0-9]+$/.test(field) ? Number(field) : undefined;
  } catch {
    return undefined;
  }
}

/** Retire pending export receipts: every file under the groups directory. */
function retireReceipts(groups: string): void {
  try {
    for (const entry of readdirSync(groups, { recursive: true, withFileTypes: true })) {
      if (entry.isFile()) rmSync(join(entry.parentPath, entry.name), { force: true });
    }
  } catch {
    warn("could not retire export receipts");
  }
}

function keepExportRoom(store: string): void {
  const freeKb = measure(["df", "-Pk", "/"], 1, 3);
  const storeKb = measure(["du", "-sk", store], 0, 0);
  if (freeKb === undefined || storeKb === undefined) {
    warn("could not measure free space; leaving the export alone");
    return;
  }
  const needKb = exportNeedKb(storeKb);
  if (freeKb >= needKb) return;
  warn(`${freeKb} KiB free, export needs about ${needKb} KiB; not saving this store`);
  const groups = join(store, "build-receipts/v1/groups");
  if (isDir(groups)) retireReceipts(groups);
}

/** Make room for the mbx export; outside GitHub Actions only with PHUX_MBX_EXPORT_HEADROOM=1. */
export function exportHeadroom(env: Env = process.env): void {
  const actions = env["GITHUB_ACTIONS"] === "true";
  if (!actions && env["PHUX_MBX_EXPORT_HEADROOM"] !== "1") {
    console.log(`${NAME}: skipped outside GitHub Actions`);
    return;
  }
  const cargo = cargoHome(env);
  tryInherit(["df", "-h", "/"]);
  for (const dir of ["registry", "git"]) rmSync(join(cargo, dir), { recursive: true, force: true });
  if (actions && env["RUNNER_OS"]) {
    if (tryInherit(["sudo", "-n", "rm", "-rf", ...RUNNER_TOOLCHAINS]) !== 0) {
      warn("toolchain cleanup skipped");
    }
  }
  const cache = mbxCacheDir(env);
  if (isDir(join(cache, "targets")) && isDir(join(cache, "actions/cas/v1"))) {
    try {
      collapse(cache);
    } catch (error) {
      warn(`collapse failed; continuing (${describe(error)})`);
    }
  }
  const store = join(cache, "actions");
  if (isDir(store)) keepExportRoom(store);
  tryInherit(["df", "-h", "/"]);
}
