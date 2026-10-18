// Materialize the pinned, patched GPUI crates the desktop host builds against.
//
// `source.json` pins each crates.io archive by SHA-256 and each reviewed patch
// by SHA-256. The result lives in `toolchain/gpui/<crate>` (ignored build
// state) and is wired into the host through `[patch.crates-io]` in
// `native/Cargo.toml`.
//
// The output is a pure function of the pins. An existing tree is verified
// against a fresh materialization and never overwritten: any difference
// fails, so local edits stay in place for inspection.
import { createHash } from "node:crypto";
import {
  cpSync,
  existsSync,
  mkdirSync,
  mkdtempSync,
  readdirSync,
  readFileSync,
  renameSync,
  rmSync,
  statSync,
  unlinkSync,
  writeFileSync,
} from "node:fs";
import { tmpdir } from "node:os";
import { basename, dirname, join } from "node:path";
import { Failure } from "../lib/command.ts";
import { git as runGit } from "../lib/git.ts";
import { repoPath } from "../lib/repo.ts";
import { isTable, str, table } from "../lib/value.ts";

export class SourceError extends Failure {}

export interface Crate {
  readonly version: string;
  readonly sha256: string;
}

export interface Pin {
  readonly registry: string;
  readonly crates: Record<string, Crate>;
  /** Toolchain-relative patch path -> SHA-256, applied in order. */
  patches: Record<string, string>;
}

/** Where the pins live and where the tree and archive cache go. */
export interface Layout {
  readonly toolchain: string;
  readonly output: string;
  readonly cache: string;
}

export function layout(toolchain = repoPath("clients/desktop/toolchain")): Layout {
  return {
    toolchain,
    output: join(toolchain, "gpui"),
    cache: join(dirname(toolchain), ".cache", "crates"),
  };
}

export function sha256(data: Uint8Array | string): string {
  return createHash("sha256").update(data).digest("hex");
}

function required(value: unknown, what: string): string {
  const text = str(value);
  if (text === undefined) throw new SourceError(`source.json: ${what} must be a string`);
  return text;
}

export function parsePin(document: unknown): Pin {
  const root = table(document);
  const crates: Record<string, Crate> = {};
  for (const [name, crate] of Object.entries(table(root["crates"]))) {
    if (!isTable(crate)) throw new SourceError(`source.json: crate ${name} must be a table`);
    crates[name] = {
      version: required(crate["version"], `${name}.version`),
      sha256: required(crate["sha256"], `${name}.sha256`),
    };
  }
  const patches: Record<string, string> = {};
  for (const [path, digest] of Object.entries(table(root["patches"]))) {
    patches[path] = required(digest, `patch ${path}`);
  }
  return { registry: required(root["registry"], "registry"), crates, patches };
}

export function readPin(paths: Layout): Pin {
  const text = readFileSync(join(paths.toolchain, "source.json"), "utf8");
  return parsePin(JSON.parse(text));
}

/** The archive bytes, from the cache or the registry, only when they match the pin. */
export async function fetchCrate(
  registry: string,
  name: string,
  crate: Crate,
  cache: string,
): Promise<Uint8Array> {
  const cached = join(cache, `${name}-${crate.version}.crate`);
  if (existsSync(cached)) {
    const data = readFileSync(cached);
    if (sha256(data) === crate.sha256) return data;
    throw new SourceError(`cached ${cached} does not match its pin; preserve it and investigate`);
  }
  const url = `${registry}/${name}/${name}-${crate.version}.crate`;
  const response = await fetch(url, { signal: AbortSignal.timeout(60_000) });
  if (!response.ok) throw new SourceError(`${url}: HTTP ${response.status}`);
  const data = new Uint8Array(await response.arrayBuffer());
  if (sha256(data) !== crate.sha256) {
    throw new SourceError(`${url} does not match its pinned sha256`);
  }
  mkdirSync(cache, { recursive: true });
  writeFileSync(cached, data);
  return data;
}

async function unpack(data: Uint8Array, name: string, version: string, into: string) {
  await new Bun.Archive(data).extract(into);
  renameSync(join(into, `${name}-${version}`), join(into, name));
}

/** git with a fixed identity, so `--edit` commits need no user config. */
function git(cwd: string, ...args: string[]): string {
  return runGit(["-c", "user.name=phux", "-c", "user.email=phux@localhost", ...args], cwd);
}

/** The patch files in series order, each checked against its pin. */
export function pinnedPatches(pin: Pin, toolchain: string): string[] {
  return Object.entries(pin.patches).map(([relative, expected]) => {
    const patch = join(toolchain, relative);
    if (sha256(readFileSync(patch)) !== expected) {
      throw new SourceError(`patch checksum mismatch: ${relative}`);
    }
    return patch;
  });
}

/** Unpack the pinned crates into `into` and apply the series; `commits` makes it an edit repo. */
export async function materialize(
  pin: Pin,
  into: string,
  paths: Layout,
  commits = false,
): Promise<void> {
  for (const [name, crate] of Object.entries(pin.crates)) {
    const data = await fetchCrate(pin.registry, name, crate, paths.cache);
    await unpack(data, name, crate.version, into);
  }
  if (commits) {
    git(into, "init", "-q");
    git(into, "add", "-A");
    git(into, "commit", "-qm", "base: pinned crates.io sources");
  }
  for (const patch of pinnedPatches(pin, paths.toolchain)) {
    git(into, "apply", "--whitespace=nowarn", patch);
    if (commits) {
      git(into, "add", "-A");
      git(into, "commit", "-qm", basename(patch, ".patch"));
    }
  }
}

/** Write an `--edit` checkout's commits back as `patches/` and their pins. */
export function exportSeries(pin: Pin, paths: Layout): number {
  const { output, toolchain } = paths;
  if (!existsSync(join(output, ".git"))) {
    throw new SourceError(`${output} is not an --edit checkout`);
  }
  if (git(output, "status", "--porcelain").trim()) {
    throw new SourceError(`commit or discard the changes in ${output} first`);
  }
  const commits = git(output, "rev-list", "--reverse", "HEAD").split(/\s+/).filter(Boolean);
  const series: Record<string, string> = {};
  for (const commit of commits.slice(1)) {
    const subject = git(output, "log", "-1", "--format=%s", commit).trim();
    const relative = `patches/${subject}.patch`;
    const diff = git(output, "diff", "--no-color", "--no-renames", `${commit}^`, commit);
    writeFileSync(join(toolchain, relative), diff);
    series[relative] = sha256(diff);
  }
  for (const stale of readdirSync(join(toolchain, "patches"))) {
    if (stale.endsWith(".patch") && !(`patches/${stale}` in series)) {
      unlinkSync(join(toolchain, "patches", stale));
    }
  }
  pin.patches = series;
  writeFileSync(join(toolchain, "source.json"), `${JSON.stringify(pin, null, 2)}\n`);
  return Object.keys(series).length;
}

/**
 * Same names and byte-identical files at every level, `.git` ignored. Like
 * Python's filecmp, entries are compared through symlinks, and a name that
 * is a file on one side and a directory on the other is a difference.
 */
export function sameTree(left: string, right: string): boolean {
  const names = (dir: string) =>
    readdirSync(dir)
      .filter((name) => name !== ".git")
      .sort();
  const leftNames = names(left);
  const rightNames = names(right);
  if (leftNames.join("\0") !== rightNames.join("\0")) return false;
  return leftNames.every((name) => sameEntry(join(left, name), join(right, name)));
}

function sameEntry(left: string, right: string): boolean {
  let leftStat, rightStat;
  try {
    leftStat = statSync(left);
    rightStat = statSync(right);
  } catch {
    return false;
  }
  if (leftStat.isDirectory() && rightStat.isDirectory()) return sameTree(left, right);
  if (!leftStat.isFile() || !rightStat.isFile()) return false;
  if (leftStat.size !== rightStat.size) return false;
  return readFileSync(left).equals(readFileSync(right));
}

/** shutil.move: a rename, or a copy when the scratch is on another filesystem. */
function move(from: string, to: string): void {
  try {
    renameSync(from, to);
  } catch (error) {
    if (!(error instanceof Error && "code" in error && error.code === "EXDEV")) throw error;
    cpSync(from, to, { recursive: true, verbatimSymlinks: true, preserveTimestamps: true });
    rmSync(from, { recursive: true, force: true });
  }
}

export type Mode = "materialize" | "check" | "edit";

/** Materialize, verify, or lay out an edit repo; returns the line to print. */
export async function bootstrap(mode: Mode, paths: Layout): Promise<string> {
  const pin = readPin(paths);
  const { output } = paths;
  const scratch = mkdtempSync(join(tmpdir(), "phux-gpui-"));
  try {
    const fresh = join(scratch, "gpui");
    mkdirSync(fresh);
    await materialize(pin, fresh, paths, mode === "edit");
    if (mode === "edit" && existsSync(output) && !existsSync(join(output, ".git"))) {
      if (!sameTree(output, fresh)) {
        throw new SourceError(`${output} has unrecorded edits; preserve them and investigate`);
      }
      rmSync(output, { recursive: true, force: true });
    }
    if (existsSync(output)) {
      if (!sameTree(output, fresh)) {
        throw new SourceError(
          `${output} differs from the pinned, patched source; preserve it and ` +
            "investigate (move it aside to rebuild)",
        );
      }
      return `verified pinned GPUI source at ${output}`;
    }
    if (mode === "check") throw new SourceError(`${output} is missing; run just desktop-source`);
    move(fresh, output);
  } finally {
    rmSync(scratch, { recursive: true, force: true });
  }
  const versions = Object.entries(pin.crates)
    .map(([name, crate]) => `${name} ${crate.version}`)
    .join(", ");
  const count = Object.keys(pin.patches).length;
  return `materialized ${versions} with ${count} patch(es) at ${output}`;
}
