// Pre-commit gate: reject staged files larger than the agent token budget.
//
// Tokens are estimated as bytes/4 of the staged blob. Only files agents author
// are checked: binaries, lockfiles, and generated or vendored files are exempt.
// Mark generated or vendored paths in .gitattributes (`linguist-generated=true`
// or `linguist-vendored=true`) to exempt them; files whose header says
// "@generated", "Code generated", or "DO NOT EDIT" are exempt automatically.
//
// The limit is a ratchet on growth. An oversized file that already existed may
// change as long as it does not get bigger, under its old path or after a
// rename, so a link fix or a module move is not an unplanned split. A new
// oversized file, or any growth of one, fails. A merge may also carry an
// oversized blob unchanged at the same path from either parent. New content,
// including manual resolutions and automatic combinations, must meet the limit.
//
// With a base commit it checks HEAD against that commit instead of the index,
// which is how CI enforces this on a pull request whose hooks never ran.
import { existsSync, mkdtempSync, readFileSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { isAbsolute, join } from "node:path";
import { Failure } from "../lib/command.ts";
import { capture, captureBytes } from "../lib/process.ts";

export const MAX_TOKENS = 5000;
export const MAX_BYTES = MAX_TOKENS * 4;
const HEADER_BYTES = 2048;
const GENERATED = /@generated|code generated|auto-?generated|do not edit/i;
const LOCKFILE =
  /(\.lock|\.lockb|-lock\.json|-lock\.yaml|\.lock\.json)$|^(go\.sum|Package\.resolved|npm-shrinkwrap\.json|gradle\.lockfile)$/;

/** Git in one checkout, optionally against a private index. */
class Git {
  constructor(
    readonly cwd: string,
    readonly index?: string,
  ) {}

  private options(): { cwd: string; env?: Record<string, string | undefined> } {
    if (this.index === undefined) return { cwd: this.cwd };
    return { cwd: this.cwd, env: { ...process.env, GIT_INDEX_FILE: this.index } };
  }

  run(args: string[]): string {
    return capture(["git", ...args], this.options());
  }

  bytes(args: string[]): Uint8Array {
    return captureBytes(["git", ...args], this.options());
  }

  /** The command's trimmed stdout, or undefined when it fails. */
  maybe(args: string[]): string | undefined {
    try {
      return this.run(args).trim();
    } catch {
      return undefined;
    }
  }
}

/** MERGE_HEAD holds full commit object IDs, never refs or abbreviated IDs. */
function isCommitOid(git: Git, oid: string): boolean {
  return (
    /^([0-9a-f]{40}|[0-9a-f]{64})$/.test(oid) && git.maybe(["cat-file", "-t", oid]) === "commit"
  );
}

/**
 * The commits a merge in progress may inherit blobs from: MERGE_HEAD's
 * parents and HEAD. Invalid or absent metadata grants none, not even HEAD.
 */
function mergeParents(git: Git): string[] {
  const relative = git.run(["rev-parse", "--git-path", "MERGE_HEAD"]).trim();
  const path = isAbsolute(relative) ? relative : join(git.cwd, relative);
  if (!existsSync(path)) return [];
  const text = readFileSync(path, "utf8");
  if (text === "") return [];
  const parents = (text.endsWith("\n") ? text.slice(0, -1) : text).split("\n");
  if (!parents.every((parent) => isCommitOid(git, parent))) return [];
  const head = git.maybe(["rev-parse", "--verify", "HEAD"]);
  if (head === undefined || !isCommitOid(git, head)) return [];
  return [...parents, head];
}

function nulFields(text: string): string[] {
  const fields = text.split("\0");
  if (fields.at(-1) === "") fields.pop();
  return fields;
}

/** Destination -> source for each rename in the staged change. */
function renames(git: Git, base: string): Map<string, string> {
  const fields = nulFields(
    git.run(["diff", "--cached", "--name-status", "-M", "--diff-filter=R", "-z", base]),
  );
  const found = new Map<string, string>();
  for (let i = 0; i + 2 < fields.length; i += 3) {
    found.set(fields[i + 2] ?? "", fields[i + 1] ?? "");
  }
  return found;
}

class Gate {
  private readonly parents: string[];
  private readonly renamed: Map<string, string>;
  private readonly base: string;

  constructor(
    private readonly git: Git,
    base: string,
  ) {
    this.parents = mergeParents(git);
    if (git.maybe(["rev-parse", "--verify", "--quiet", base]) === undefined) {
      // A repository's first commit has nothing to compare against.
      this.base = git.run(["hash-object", "-t", "tree", "/dev/null"]).trim();
      this.renamed = new Map();
    } else {
      this.base = base;
      this.renamed = renames(git, base);
    }
  }

  /** `  path  ~N tokens (B bytes)` for each staged file over the budget. */
  oversized(): string[] {
    const fields = nulFields(
      this.git.run([
        "diff",
        "--cached",
        "--numstat",
        "--no-renames",
        "-z",
        "--diff-filter=ACMR",
        this.base,
      ]),
    );
    return fields.flatMap((record) => {
      const [added = "", , path = ""] = record.split("\t");
      // numstat reports binary files as "-".
      if (added === "-") return [];
      const bytes = Number(this.git.run(["cat-file", "-s", `:${path}`]).trim());
      if (bytes <= MAX_BYTES || this.didNotGrow(path, bytes) || this.isExempt(path)) return [];
      return [`  ${path}  ~${Math.floor(bytes / 4)} tokens (${bytes} bytes)`];
    });
  }

  /** The path this file had at the base, following a rename; undefined when it is new. */
  private basePath(path: string): string | undefined {
    if (this.git.maybe(["cat-file", "-e", `${this.base}:${path}`]) !== undefined) return path;
    return this.renamed.get(path);
  }

  private didNotGrow(path: string, bytes: number): boolean {
    const previous = this.basePath(path);
    if (previous === undefined) return false;
    return bytes <= Number(this.git.run(["cat-file", "-s", `${this.base}:${previous}`]).trim());
  }

  private isExempt(path: string): boolean {
    return (
      LOCKFILE.test(path.slice(path.lastIndexOf("/") + 1)) ||
      this.hasExemptAttribute(path) ||
      this.hasGeneratedHeader(path) ||
      this.inheritedFromAParent(path)
    );
  }

  private hasExemptAttribute(path: string): boolean {
    const attributes = this.git.run([
      "check-attr",
      "--cached",
      "linguist-generated",
      "linguist-vendored",
      "--",
      path,
    ]);
    return attributes.split("\n").some((line) => /: (set|true)$/.test(line));
  }

  /** Only the blob's first 2048 bytes may grant the generated-header exemption. */
  private hasGeneratedHeader(path: string): boolean {
    let blob: Uint8Array;
    try {
      blob = this.git.bytes(["cat-file", "blob", `:${path}`]);
    } catch {
      return false;
    }
    return GENERATED.test(new TextDecoder("latin1").decode(blob.subarray(0, HEADER_BYTES)));
  }

  /** Only byte-identical staged content at the same path is inherited. */
  private inheritedFromAParent(path: string): boolean {
    if (this.parents.length === 0) return false;
    const staged = this.git.maybe(["rev-parse", "--verify", `:${path}`]);
    if (staged === undefined) return false;
    return this.parents.some(
      (parent) => this.git.maybe(["rev-parse", "--verify", `${parent}:${path}`]) === staged,
    );
  }
}

/** The staged files that are new or grew past the budget, in the index of `cwd` or HEAD against `base`. */
export function oversizedFiles(cwd: string, base?: string): string[] {
  if (base === undefined) return new Gate(new Git(cwd), "HEAD").oversized();
  const dir = mkdtempSync(join(tmpdir(), "phux-file-tokens-"));
  try {
    const git = new Git(cwd, join(dir, "index"));
    git.run(["read-tree", "HEAD"]);
    return new Gate(git, base).oversized();
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
}

/** HEAD's merge base with `ref`, for checking a branch the hooks never saw. */
export function mergeBase(cwd: string, ref: string): string {
  const base = new Git(cwd).maybe(["merge-base", ref, "HEAD"]);
  if (base === undefined || base === "") {
    throw new Failure(
      `file-tokens: no merge base between ${ref} and HEAD; fetch it (git fetch origin main)`,
    );
  }
  return base;
}

export function rejection(oversized: readonly string[]): string {
  return `pre-commit: ${oversized.length} staged file(s) are new or grew past the ${MAX_TOKENS}-token limit (tokens = bytes/4, so ${MAX_BYTES} bytes):
${oversized.join("\n")}

Split each file listed above before committing:
  1. Find its seams: group the contents by responsibility (types, parsing,
     I/O, state, rendering, CLI wiring, tests, fixtures, doc sections).
  2. Move each group into its own file beside the original, named for what it
     does (e.g. session.rs -> session/{mod,parse,store,render}.rs), keeping
     every resulting file under ${MAX_TOKENS} tokens (${MAX_BYTES} bytes).
  3. Keep the original path as a thin entry point that imports or re-exports
     the pieces, so callers and the public API do not change:
       Rust: \`mod parse; pub use parse::*;\`   TS/JS: \`export * from "./parse"\`
       Swift: new files in the same module      Python: package + __init__.py
       Shell: \`source\` helper files             Markdown: an index linking sub-pages
  4. Move tests with the code they cover; put large fixtures or data tables in
     their own files and load them instead of inlining them.
  5. Split by meaning. Do not minify, strip comments, or join lines to get
     under the limit.
  6. \`git add\` the new files and the slimmed original, then commit again.

If a flagged file is generated, vendored, or a lockfile rather than authored,
mark it in .gitattributes instead of splitting it:
  path/to/file linguist-generated=true
`;
}

/** Run the gate in `cwd`; prints the rejection and returns 1 when a file is over budget. */
export function fileTokensCheck(cwd: string, base?: string): number {
  const oversized = oversizedFiles(cwd, base);
  if (oversized.length === 0) return 0;
  process.stderr.write(rejection(oversized));
  return 1;
}
