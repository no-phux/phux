// The pinned, patched GPUI crates the desktop host builds against.
// `source.json` pins each crates.io archive and each patch by SHA-256; the
// tree in `toolchain/gpui` is a pure function of the pins. An existing tree
// is verified against a fresh materialization and never overwritten.
import { createHash } from "node:crypto";
import {
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
import { basename, dirname, join } from "node:path";
import { Failure } from "../lib/command.ts";
import { git as runGit } from "../lib/git.ts";
import { repoPath } from "../lib/repo.ts";

export interface Pin {
  readonly registry: string;
  readonly crates: Record<string, { readonly version: string; readonly sha256: string }>;
  /** Toolchain-relative patch path -> SHA-256, applied in order. */
  patches: Record<string, string>;
}

export interface Layout {
  readonly toolchain: string;
  readonly output: string;
  /** Ignored build state beside the output, so a finished tree is renamed into place. */
  readonly cache: string;
}

export function layout(toolchain = repoPath("clients/desktop/toolchain")): Layout {
  const cache = join(dirname(toolchain), ".cache");
  return { toolchain, output: join(toolchain, "gpui"), cache };
}

export function sha256(data: Uint8Array | string): string {
  return createHash("sha256").update(data).digest("hex");
}

export function readPin(paths: Layout): Pin {
  return JSON.parse(readFileSync(join(paths.toolchain, "source.json"), "utf8")) as Pin;
}

/** The archive bytes, from the cache or the registry, only when they match the pin. */
export async function fetchCrate(
  registry: string,
  name: string,
  { version, sha256: expected }: Pin["crates"][string],
  cache: string,
): Promise<Uint8Array> {
  const cached = join(cache, "crates", `${name}-${version}.crate`);
  if (existsSync(cached)) {
    const data = readFileSync(cached);
    if (sha256(data) === expected) return data;
    throw new Failure(`cached ${cached} does not match its pin; preserve it and investigate`);
  }
  const url = `${registry}/${name}/${name}-${version}.crate`;
  const response = await fetch(url, { signal: AbortSignal.timeout(60_000) });
  if (!response.ok) throw new Failure(`${url}: HTTP ${response.status}`);
  const data = new Uint8Array(await response.arrayBuffer());
  if (sha256(data) !== expected) throw new Failure(`${url} does not match its pinned sha256`);
  mkdirSync(dirname(cached), { recursive: true });
  writeFileSync(cached, data);
  return data;
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
      throw new Failure(`patch checksum mismatch: ${relative}`);
    }
    return patch;
  });
}

/** Unpack the pinned crates into `into` and apply the series; `commits` makes it an edit repo. */
async function materialize(pin: Pin, into: string, paths: Layout, commits: boolean) {
  for (const [name, crate] of Object.entries(pin.crates)) {
    await new Bun.Archive(await fetchCrate(pin.registry, name, crate, paths.cache)).extract(into);
    renameSync(join(into, `${name}-${crate.version}`), join(into, name));
  }
  const commit = (message: string) => {
    if (!commits) return;
    git(into, "add", "-A");
    git(into, "commit", "-qm", message);
  };
  if (commits) git(into, "init", "-q");
  commit("base: pinned crates.io sources");
  for (const patch of pinnedPatches(pin, paths.toolchain)) {
    git(into, "apply", "--whitespace=nowarn", patch);
    commit(basename(patch, ".patch"));
  }
}

/** Write an `--edit` checkout's commits back as `patches/` and their pins. */
export function exportSeries(pin: Pin, { output, toolchain }: Layout): number {
  if (!existsSync(join(output, ".git"))) throw new Failure(`${output} is not an --edit checkout`);
  if (git(output, "status", "--porcelain").trim()) {
    throw new Failure(`commit or discard the changes in ${output} first`);
  }
  const commits = git(output, "rev-list", "--reverse", "HEAD").split(/\s+/).filter(Boolean);
  pin.patches = {};
  for (const commit of commits.slice(1)) {
    const relative = `patches/${git(output, "log", "-1", "--format=%s", commit).trim()}.patch`;
    const diff = git(output, "diff", "--no-color", "--no-renames", `${commit}^`, commit);
    writeFileSync(join(toolchain, relative), diff);
    pin.patches[relative] = sha256(diff);
  }
  for (const stale of readdirSync(join(toolchain, "patches"))) {
    if (stale.endsWith(".patch") && !(`patches/${stale}` in pin.patches)) {
      unlinkSync(join(toolchain, "patches", stale));
    }
  }
  writeFileSync(join(toolchain, "source.json"), `${JSON.stringify(pin, null, 2)}\n`);
  return Object.keys(pin.patches).length;
}

/** Same names and byte-identical files (through symlinks) at every level, `.git` ignored. */
export function sameTree(left: string, right: string): boolean {
  const names = (dir: string) =>
    readdirSync(dir)
      .filter((name) => name !== ".git")
      .sort();
  const leftNames = names(left);
  if (leftNames.join("\0") !== names(right).join("\0")) return false;
  return leftNames.every((name) => {
    const [a, b] = [join(left, name), join(right, name)];
    const [aStat, bStat] = [
      statSync(a, { throwIfNoEntry: false }),
      statSync(b, { throwIfNoEntry: false }),
    ];
    if (aStat?.isDirectory() && bStat?.isDirectory()) return sameTree(a, b);
    return Boolean(aStat?.isFile() && bStat?.isFile()) && readFileSync(a).equals(readFileSync(b));
  });
}

export type Mode = "materialize" | "check" | "edit";

/** Materialize, verify, or lay out an edit repo; returns the line to print. */
export async function bootstrap(mode: Mode, paths: Layout): Promise<string> {
  const pin = readPin(paths);
  const { output } = paths;
  mkdirSync(paths.cache, { recursive: true });
  const scratch = mkdtempSync(join(paths.cache, "gpui-"));
  try {
    await materialize(pin, scratch, paths, mode === "edit");
    if (mode === "edit" && existsSync(output) && !existsSync(join(output, ".git"))) {
      if (!sameTree(output, scratch)) {
        throw new Failure(`${output} has unrecorded edits; preserve them and investigate`);
      }
      rmSync(output, { recursive: true, force: true });
    }
    if (existsSync(output)) {
      if (!sameTree(output, scratch)) {
        throw new Failure(
          `${output} differs from the pinned, patched source; preserve it and ` +
            "investigate (move it aside to rebuild)",
        );
      }
      return `verified pinned GPUI source at ${output}`;
    }
    if (mode === "check") throw new Failure(`${output} is missing; run just desktop-source`);
    renameSync(scratch, output);
  } finally {
    rmSync(scratch, { recursive: true, force: true });
  }
  const versions = Object.entries(pin.crates).map(([name, { version }]) => `${name} ${version}`);
  const count = Object.keys(pin.patches).length;
  return `materialized ${versions.join(", ")} with ${count} patch(es) at ${output}`;
}
