// Scratch repositories for the file-token gate's tests: real staged blobs and
// real conflicted or clean Git merges, never a mocked Git.
import { expect } from "bun:test";
import {
  appendFileSync,
  mkdirSync,
  mkdtempSync,
  readFileSync,
  rmSync,
  writeFileSync,
} from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { capture } from "../lib/process.ts";
import { oversizedFiles } from "./file-tokens.ts";

const CONFIG = [
  "user.name=t",
  "user.email=t@t",
  "commit.gpgsign=false",
  "rerere.enabled=false",
  "core.autocrlf=false",
];

export class Repo {
  constructor(readonly dir: string) {}

  g(...args: string[]): string {
    return capture(["git", "-C", this.dir, ...CONFIG.flatMap((pair) => ["-c", pair]), ...args]);
  }

  /** Run git where failure is the expected outcome; true when it succeeded. */
  tries(...args: string[]): boolean {
    try {
      this.g(...args);
      return true;
    } catch {
      return false;
    }
  }

  path(file: string): string {
    return join(this.dir, file);
  }

  write(file: string, content: string): void {
    writeFileSync(this.path(file), content);
  }

  /** `bytes` of filler, like `head -c N /dev/zero | tr '\0' x`. */
  fill(file: string, bytes: number): void {
    this.write(file, "x".repeat(bytes));
  }

  append(file: string, content: string): void {
    appendFileSync(this.path(file), content);
  }

  read(file: string): string {
    return readFileSync(this.path(file), "utf8");
  }

  show(rev: string, file: string): string {
    return this.g("show", `${rev}:${file}`);
  }

  add(...files: string[]): void {
    this.g("add", ...files);
  }

  commit(message: string): void {
    this.g("commit", "-q", "-m", message);
  }

  blob(rev: string): string {
    return this.g("rev-parse", rev).trim();
  }

  /** The gate's rejection lines; empty when it passes. */
  gate(base?: string): string[] {
    return oversizedFiles(this.dir, base);
  }

  passes(base?: string): void {
    expect(this.gate(base)).toEqual([]);
  }

  /** The gate fails and names at least each of `named`. */
  rejects(named: string[], base?: string): void {
    const lines = this.gate(base);
    expect(lines.length).toBeGreaterThan(0);
    const names = lines.map((line) => line.split("  ")[1]);
    for (const name of named) expect(names).toContain(name);
  }

  notNamed(file: string): void {
    expect(this.gate().some((line) => line.startsWith(`  ${file}  ~`))).toBe(false);
  }

  /** Staged `file` is byte-identical to `rev:file`. */
  sameBlob(file: string, rev: string): void {
    expect(this.blob(`:${file}`)).toBe(this.blob(`${rev}:${file}`));
  }

  /** Staged `file` matches neither merge parent. */
  novelBlob(file: string): void {
    for (const parent of ["HEAD", "MERGE_HEAD"]) {
      expect(this.blob(`:${file}`)).not.toBe(this.blob(`${parent}:${file}`));
    }
  }

  /** Stage `rev`'s big.md as the resolution. */
  resolveFrom(rev: string): void {
    this.write("big.md", this.show(rev, "big.md"));
    this.add("big.md");
    this.sameBlob("big.md", rev);
  }
}

/** One scratch directory per test, holding an empty hook template. */
export function scratch(run: (newRepo: (name: string) => Repo) => void): void {
  const work = mkdtempSync(join(tmpdir(), "phux-file-tokens-test-"));
  try {
    mkdirSync(join(work, "template"));
    run((name) => {
      const repo = new Repo(join(work, name));
      mkdirSync(repo.dir);
      repo.g("init", "-q", "-b", "main", `--template=${join(work, "template")}`);
      repo.write("README.md", "seed\n");
      repo.add("README.md");
      repo.commit("seed");
      return repo;
    });
  } finally {
    rmSync(work, { recursive: true, force: true });
  }
}

/** Branches that conflict on the last line of an oversized text file. */
export function conflictingMerge(repo: Repo): string {
  repo.fill("big.md", 25000);
  repo.append("big.md", "\nbase\n");
  repo.add("big.md");
  repo.commit("base");
  repo.g("checkout", "-q", "-b", "feature");
  const base = repo.show("HEAD", "big.md");
  repo.write("big.md", `${base.slice(0, 25001)}feature\n`);
  repo.add("big.md");
  repo.commit("feature");
  repo.g("checkout", "-q", "main");
  repo.write("big.md", `${base.slice(0, 25001)}main\n`);
  repo.add("big.md");
  repo.commit("main");
  expect(repo.tries("merge", "--no-ff", "--no-commit", "feature")).toBe(false);
  repo.g("rev-parse", "--verify", "MERGE_HEAD");
  expect(repo.g("ls-files", "-u", "--", "big.md")).not.toBe("");
  return base;
}
