// Verify immutable engine source and report actual prebuilt coverage.
//
// libghostty-vt-sys links a SHA-256-verified archive of its Ghostty commit,
// downloaded from the libghostty-rs release named in its download.rs, and
// falls back to a Zig source build when its table has no entry for the target.
// Source-only pins are supported: this gate must never require cutting a release
// or accept old archive hashes as evidence for a new engine. Cargo.lock names
// the immutable libghostty-rs rev; its build.rs pins the engine source and its
// download.rs names any separately published archives.
//
// FAILURE POLICY: malformed pins, missing engine source, or a listed archive the
// release does not serve are hard failures. An unreachable service (offline,
// GitHub outage) is a warning and exit 0, never a verification claim.
import { readFileSync } from "node:fs";
import { join } from "node:path";
import { Failure } from "../lib/command.ts";
import type { Level } from "../lib/output.ts";
import { list, str, table } from "../lib/value.ts";

export const TARGETS = [
  "x86_64-unknown-linux-gnu",
  "aarch64-unknown-linux-gnu",
  "aarch64-apple-darwin",
  "x86_64-apple-darwin",
] as const;

const TIMEOUT_MS = 30_000;
const NAME = "ghostty-pin";

/** The outside world this check reads: `fetch`, or a test's stand-in. */
export type Fetch = (url: string, init?: RequestInit) => Promise<Response>;

export interface PinContext {
  readonly root: string;
  readonly fetch: Fetch;
  readonly log: (level: Level, message: string) => void;
  readonly token?: string | undefined;
}

/** A service was unreachable: the check reports nothing and passes. */
class Skip extends Error {}

const fail = (message: string): never => {
  throw new Failure(`${NAME}: ${message}`);
};
const skip = (reason: string): never => {
  throw new Skip(reason);
};

/** The `source = ` line of the libghostty-vt-sys package in Cargo.lock. */
export function lockSource(lock: string): string {
  let found = false;
  for (const line of lock.split("\n")) {
    if (line === 'name = "libghostty-vt-sys"') {
      found = true;
      continue;
    }
    if (found && line.startsWith("source = ")) return line;
    if (line.startsWith("[[package]]")) found = false;
  }
  return "";
}

/** Every `const NAME: &str = "value";` line's value, one per line. */
export function rustConst(source: string, name: string): string {
  const pattern = new RegExp(`^const ${name}: &str = "(.*)";$`);
  return source
    .split("\n")
    .flatMap((line) => {
      const match = pattern.exec(line);
      return match === null ? [] : [match[1] ?? ""];
    })
    .join("\n");
}

/** `prefix<value>` lines' values, one per line. */
function prefixed(text: string, prefix: string): string {
  return text
    .split("\n")
    .filter((line) => line.startsWith(prefix))
    .map((line) => line.slice(prefix.length))
    .join("\n");
}

/** The version without its last dot component (`0.17.1` -> `0.17`). */
const minor = (version: string): string => version.replace(/\.[^.]*$/, "");

function compareVersions(left: string, right: string): number {
  const a = left.split(".").map(Number);
  const b = right.split(".").map(Number);
  for (let i = 0; i < Math.max(a.length, b.length); i++) {
    const delta = (a[i] ?? 0) - (b[i] ?? 0);
    if (delta !== 0) return delta;
  }
  return 0;
}

/** The engine names a minimum; a later patch of the same minor release builds it. */
export function zigCompatible(engine: string, pinned: string): boolean {
  return engine !== "" && minor(engine) === minor(pinned) && compareVersions(engine, pinned) <= 0;
}

/** The PREBUILT_ARCHIVES table on one line; rustfmt may split an entry. */
export function archiveTable(downloadRs: string): string {
  const lines = downloadRs.split("\n");
  const start = lines.findIndex((line) => line.startsWith("const PREBUILT_ARCHIVES:"));
  if (start < 0) return "";
  const end = lines.findIndex((line, index) => index >= start && line.includes(";"));
  return lines
    .slice(start, end < 0 ? undefined : end + 1)
    .join("\n")
    .replace(/\s+/g, " ");
}

const escape = (text: string): string => text.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");

async function request(ctx: PinContext, url: string, init: RequestInit = {}): Promise<Response> {
  return ctx.fetch(url, { ...init, signal: AbortSignal.timeout(TIMEOUT_MS) });
}

/** A source file; HTTP absence is distinct from network failure, a typo is not an outage. */
async function fetchSource(ctx: PinContext, url: string): Promise<string> {
  let response: Response;
  try {
    response = await request(ctx, url);
  } catch {
    return skip(`could not fetch ${url}`);
  }
  if (response.status >= 200 && response.status < 300) return response.text();
  if (response.status >= 400 && response.status < 500) {
    return fail(`${url} is not served (HTTP ${response.status})`);
  }
  return skip(`${url} is unreachable (HTTP ${response.status})`);
}

/** The HTTP status of a HEAD of `url`, or 0 when it could not be asked. */
async function headStatus(ctx: PinContext, url: string): Promise<number> {
  try {
    return (await request(ctx, url, { method: "HEAD" })).status;
  } catch {
    return 0;
  }
}

/** Asset names the GitHub release `tag` under `base` lists; an unreachable API skips. */
async function releaseAssets(ctx: PinContext, base: string, tag: string): Promise<Set<string>> {
  const repo = /^https:\/\/github\.com\/([^/]+\/[^/]+)\/releases\/download$/.exec(base)?.[1];
  if (repo === undefined) return skip(`${base} is not a GitHub release download base`);
  const api = `https://api.github.com/repos/${repo}/releases/tags/${tag}`;
  const headers: Record<string, string> =
    ctx.token === undefined || ctx.token === "" ? {} : { Authorization: `Bearer ${ctx.token}` };
  const unreachable = `GitHub did not serve ${base}/${tag} and its API is unreachable`;
  let text: string;
  try {
    const response = await request(ctx, api, { headers });
    if (!response.ok) return skip(unreachable);
    text = await response.text();
  } catch {
    return skip(unreachable);
  }
  let assets: unknown[] = [];
  try {
    assets = list(table(JSON.parse(text))["assets"]);
  } catch {
    // An unreadable listing lists nothing.
  }
  return new Set(assets.flatMap((asset) => str(table(asset)["name"]) ?? []));
}

async function verify(ctx: PinContext): Promise<void> {
  const read = (path: string) => readFileSync(join(ctx.root, path), "utf8");
  const sourceLine = lockSource(read("Cargo.lock"));
  const pin =
    /git\+https:\/\/github\.com\/([^/]+\/[^/.]+)(\.git)?\?rev=[0-9a-f]{40}#([0-9a-f]{40})/.exec(
      sourceLine,
    );
  if (pin === null) {
    return fail(
      `Cargo.lock has no git source for libghostty-vt-sys (got: ${sourceLine || "nothing"})`,
    );
  }
  const fork = pin[1] ?? "";
  const forkRev = pin[3] ?? "";
  if (!sourceLine.includes(`?rev=${forkRev}#${forkRev}`)) {
    return fail("Cargo.lock binding revision does not match its resolved commit");
  }

  const raw = `https://raw.githubusercontent.com/${fork}/${forkRev}/crates/libghostty-vt-sys`;
  const buildRs = await fetchSource(ctx, `${raw}/build.rs`);
  const downloadRs = await fetchSource(ctx, `${raw}/download.rs`);

  const commit = rustConst(buildRs, "GHOSTTY_COMMIT");
  const engineRepo = rustConst(buildRs, "GHOSTTY_REPO");
  if (!/^[0-9a-f]{40}$/.test(commit)) {
    return fail(`engine revision is not an immutable full commit: ${commit}`);
  }
  const engineFork = /^https:\/\/github\.com\/([^/]+\/[^/.]+)(\.git)?$/.exec(engineRepo)?.[1];
  if (engineFork === undefined) {
    return fail(`cannot verify engine source repository: ${engineRepo}`);
  }
  const browserBuild = read("clients/phux-vt-web/scripts/build-vt-wasm.sh");
  const browserCommit = prefixed(browserBuild, "revision=");
  if (browserCommit !== commit) {
    return fail(`native engine ${commit} differs from browser engine ${browserCommit}`);
  }
  const nixCommit = /^ *ghosttyRev = "([0-9a-f]*)";$/m.exec(read("nix/package.nix"))?.[1] ?? "";
  if (nixCommit !== commit) {
    return fail(
      `native engine ${commit} differs from the Nix package's ${nixCommit || "missing"} (nix/package.nix)`,
    );
  }
  if (!/^[0-9a-f]{64}$/.test(prefixed(browserBuild, "archive_sha256="))) {
    return fail("browser engine source checksum is missing or malformed");
  }
  if (!browserBuild.includes(`https://codeload.github.com/${engineFork}/tar.gz/$revision`)) {
    return fail("native and browser engine source repositories differ");
  }
  const zon = await fetchSource(
    ctx,
    `https://raw.githubusercontent.com/${engineFork}/${commit}/build.zig.zon`,
  );
  const engineZig = /\.minimum_zig_version = "([^"]*)"/.exec(zon)?.[1] ?? "";
  const pinnedZig = str(table(JSON.parse(read(".config/zig-toolchain.json")))["version"]) ?? "";
  if (!zigCompatible(engineZig, pinnedZig)) {
    return fail(`engine requires Zig ${engineZig || "unknown"}; toolchain pins ${pinnedZig}`);
  }
  ctx.log("ok", `${NAME}: immutable engine source ${engineFork}@${commit} verified`);

  const base = rustConst(downloadRs, "PREBUILT_BASE_URL");
  const optimize = rustConst(downloadRs, "PREBUILT_OPTIMIZE");
  const cpu = rustConst(downloadRs, "PREBUILT_CPU");
  if (base === "" || optimize === "" || cpu === "") {
    return fail(
      `could not read the Ghostty commit and prebuilt flavor from ${fork}@${forkRev.slice(0, 12)}`,
    );
  }
  const tag = `ghostty-vt-${commit.slice(0, 12)}-${optimize.toLowerCase()}-${cpu}`;

  const archives = archiveTable(downloadRs);
  if (!archives.includes("const PREBUILT_ARCHIVES:")) {
    return fail("prebuilt coverage table is missing");
  }
  let listed: Set<string> | undefined;
  let prebuilt = 0;
  let sourceOnly = 0;
  for (const target of TARGETS) {
    if (!archives.includes(`"${target}"`)) {
      ctx.log("info", `${NAME}: ${target} uses verified source (no prebuilt declared)`);
      sourceOnly++;
      continue;
    }
    if (!new RegExp(`\\( ?"${escape(target)}", ?"[0-9a-f]{64}",? ?\\)`).test(archives)) {
      return fail(`malformed prebuilt checksum for ${target}`);
    }
    prebuilt++;
    const archive = `libghostty-vt-${target}.tar.gz`;
    const url = `${base}/${tag}/${archive}`;
    const status = await headStatus(ctx, url);
    if (status >= 200 && status < 300) continue;
    if (status >= 400 && status < 500) return fail(`${url} is not served`);
    // releases/download can answer one asset with a 5xx for hours; the API
    // still says whether the release has it.
    listed ??= await releaseAssets(ctx, base, tag);
    if (!listed.has(archive)) {
      return fail(`${url} is not served (HTTP ${status}, and the release does not list it)`);
    }
  }
  ctx.log(
    "ok",
    `${NAME}: ${fork}@${forkRev.slice(0, 12)}: ${prebuilt} prebuilt, ${sourceOnly} source-only targets (${tag})`,
  );
}

/** Run the check; an unreachable service warns and passes, a broken pin throws Failure. */
export async function checkGhosttyPin(ctx: PinContext): Promise<void> {
  try {
    await verify(ctx);
  } catch (error) {
    if (!(error instanceof Skip)) throw error;
    ctx.log("warn", `${NAME}: skipped (${error.message})`);
  }
}
