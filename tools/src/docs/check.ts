// The docs gate: the checks that catch a doc drifting away from the code.
//
//   links     every relative link and #anchor in a tracked Markdown file
//             resolves (headings are slugged the way GitHub slugs them)
//   commands  every `phux <command>` and `just <recipe>` in a shell code
//             block or an inline code span names a command the binary has
//             and a recipe the justfiles define
//   crates    docs/architecture.md names every crates/* directory, and
//             names none that is gone
//   citations every `docs/...md` path (and #anchor) that a code comment
//             under crates/ or clients/ cites exists, and no comment cites
//             a retired spec page (L1.md, proto.md, consumers/tui.md, ...)
//   nav       every flat docs/*.md opens with frontmatter that places it in
//             the site nav (`group:` and numeric `order:`), or opts out with
//             `publish: false`. The website reads this; there is no catalog.
//
// The command list comes from docs/reference/cli.md, which a unit test keeps
// identical to the binary, so this needs no build.
import { existsSync, readdirSync, statSync } from "node:fs";
import { find } from "./files.ts";
import {
  Paths,
  SPACE,
  lines,
  matches,
  prose,
  readText,
  splitAnchor,
  withoutCode,
} from "./markdown.ts";

export const GATES = ["links", "commands", "crates", "citations", "nav"] as const;
export type Gate = (typeof GATES)[number];

export interface Violation {
  readonly gate: Gate;
  /** Absolute path of the file the problem is in. */
  readonly file: string;
  readonly message: string;
}

/** `[gate] path: message`, the path relative to `root`. */
export function format(root: string, v: Violation): string {
  const file = v.file.startsWith(`${root}/`) ? v.file.slice(root.length + 1) : v.file;
  return `[${v.gate}] ${file}: ${v.message}`;
}

/**
 * Generated changelogs, vendored notices, the Beads database, and the skills
 * package README in packaging/skills/package (its links resolve in the
 * exported no-phux/skills repository, not here).
 */
const SKIPPED_DOCS =
  /(^|\/)(CHANGELOG|THIRD-PARTY-NOTICES)\.md$|\/\.beads\/|\/node_modules\/|\/packaging\/skills\/package\//;

/** Tracked and untracked-but-not-ignored Markdown; outside a Git checkout, every *.md. Sorted, absolute. */
export function markdownFiles(root: string): string[] {
  const tracked = find(root, ["*.md"]);
  const files =
    tracked === undefined
      ? walk(root).filter((rel) => rel.endsWith(".md") && !rel.includes("node_modules/"))
      : tracked.filter((rel) => isFile(`${root}/${rel}`));
  return files
    .map((rel) => `${root}/${rel}`)
    .filter((path) => !SKIPPED_DOCS.test(path))
    .toSorted(byteOrder);
}

function walk(dir: string): string[] {
  try {
    return readdirSync(dir, { recursive: true }).map(String);
  } catch {
    return [];
  }
}

function isFile(path: string): boolean {
  try {
    return statSync(path).isFile();
  } catch {
    return false;
  }
}

function isDir(path: string): boolean {
  try {
    return statSync(path).isDirectory();
  } catch {
    return false;
  }
}

function byteOrder(left: string, right: string): number {
  if (left === right) return 0;
  return left < right ? -1 : 1;
}

function dirname(path: string): string {
  return path.slice(0, path.lastIndexOf("/"));
}

function isExternal(link: string): boolean {
  return link.includes("://") || /^(mailto|tel):/.test(link);
}

function isMarkdownFile(path: string): boolean {
  return path.endsWith(".md") && isFile(path);
}

/** `[line, target]` for every inline link outside code; a `"title"` is dropped. */
export function links(text: string): [number, string][] {
  const found: [number, string][] = [];
  for (const [line, content] of prose(text)) {
    for (const m of matches(withoutCode(content), /\]\(([^) ]+)( "[^"]*")?\)/)) {
      found.push([line, m[1] ?? ""]);
    }
  }
  return found;
}

class Gates {
  readonly violations: Violation[] = [];
  readonly paths = new Paths();

  constructor(
    readonly root: string,
    readonly files: readonly string[],
  ) {}

  private violate(gate: Gate, file: string, message: string): void {
    this.violations.push({ gate, file, message });
  }

  links(): void {
    for (const file of this.files) {
      for (const [line, link] of links(readText(file) ?? "")) this.link(file, line, link);
    }
  }

  private link(file: string, line: number, link: string): void {
    if (isExternal(link)) return;
    const [target, anchor] = splitAnchor(link);
    const path = target.split("?")[0] ?? "";
    const resolved = this.resolve(file, path);
    if (path !== "" && !this.paths.exists(resolved)) {
      this.violate("links", file, `line ${line}: ${link} does not exist`);
    } else if (
      anchor !== "" &&
      isMarkdownFile(resolved) &&
      !this.paths.hasAnchor(resolved, anchor)
    ) {
      this.violate("links", file, `line ${line}: ${link} has no heading #${anchor}`);
    }
  }

  /** A link path from `file`: empty is the file itself, a leading `/` is the repository root. */
  private resolve(file: string, path: string): string {
    if (path === "") return file;
    return path.startsWith("/") ? `${this.root}${path}` : `${dirname(file)}/${path}`;
  }

  commands(): void {
    const cliPage = `${this.root}/docs/reference/cli.md`;
    const cli = lines(readText(cliPage) ?? "");
    const paths = new Set(cli.flatMap((l) => /^## `phux (.*)`$/.exec(l)?.[1] ?? []));
    if (paths.size === 0) {
      this.violate("commands", cliPage, "no command headings; run `just docs-gen`");
      return;
    }
    const groups = new Set(
      cli.flatMap((l) => /^Usage: phux ([a-z][a-z0-9-]*) <SUBCOMMAND>/.exec(l)?.[1] ?? []),
    );
    const recipes = justRecipes(this.root);
    for (const file of this.files) {
      for (const call of commandCalls(readText(file) ?? "")) {
        const problem = callProblem(call, { paths, groups, recipes });
        if (problem !== undefined) this.violate("commands", file, `line ${call.line}: ${problem}`);
      }
    }
  }

  crates(): void {
    const page = `${this.root}/docs/architecture.md`;
    const text = readText(page);
    if (text === undefined) return;
    const named = crateTable(text);
    const crates = `${this.root}/crates`;
    for (const name of listDirs(crates)) {
      if (!isFile(`${crates}/${name}/Cargo.toml`)) continue;
      if (!named.includes(name))
        this.violate("crates", page, `crates/${name} is not in the crate table`);
    }
    for (const name of named) {
      if (!isCrateName(name)) continue;
      if (isDir(`${crates}/${name}`) || isDir(`${this.root}/clients/${name}`)) continue;
      this.violate("crates", page, `${name} is not a crate in this tree`);
    }
  }

  citations(): void {
    for (const rel of sourceFiles(this.root)) {
      const file = `${this.root}/${rel}`;
      for (const [line, cite] of citations(readText(file) ?? "")) this.citation(file, line, cite);
    }
  }

  private citation(file: string, line: number, cite: string): void {
    if (cite.startsWith("retired:")) {
      const page = cite.slice("retired:".length);
      this.violate(
        "citations",
        file,
        `line ${line}: ${page} is a retired spec page; cite its live page under docs/`,
      );
      return;
    }
    const [path, anchor] = splitAnchor(cite);
    const target = `${this.root}/${path}`;
    if (!this.paths.exists(target)) {
      this.violate("citations", file, `line ${line}: ${path} does not exist`);
    } else if (anchor !== "" && !this.paths.hasAnchor(target, anchor)) {
      this.violate("citations", file, `line ${line}: ${path} has no heading #${anchor}`);
    }
  }

  nav(): void {
    const docs = `${this.root}/docs`;
    let names: string[] = [];
    try {
      names = readdirSync(docs);
    } catch {
      return;
    }
    const pages = names.filter((n) => n.endsWith(".md") && !n.startsWith(".")).toSorted(byteOrder);
    for (const name of pages) {
      const page = `${docs}/${name}`;
      if (isFile(page)) {
        for (const message of navProblems(readText(page) ?? "")) this.violate("nav", page, message);
      }
    }
  }
}

/** Recipes and aliases from the root justfile and its just/*.just modules. */
export function justRecipes(root: string): Set<string> {
  const modules = readdirSync(`${root}/just`, { withFileTypes: true })
    .filter((e) => e.name.endsWith(".just") && !e.name.startsWith("."))
    .map((e) => `${root}/just/${e.name}`)
    .toSorted(byteOrder);
  const recipes = new Set<string>();
  for (const file of [`${root}/justfile`, ...modules]) {
    for (const line of lines(readText(file) ?? "")) {
      if (/^alias [a-z]/.test(line)) {
        recipes.add(line.split(/[ \t]+/)[1] ?? "");
      } else if (/^[a-z][a-z0-9_-]*( [^:]*)?:([^=]|$)/.test(line)) {
        recipes.add(line.replace(/[ :].*$/s, ""));
      }
    }
  }
  return recipes;
}

export interface Call {
  readonly line: number;
  readonly tool: string;
  readonly first: string;
  readonly second: string;
}

const WORDS = new RegExp(`${SPACE}+`);
const BACKTICK_FENCE = new RegExp(`^${SPACE}*\`\`\``);
const SHELL_FENCE = new RegExp(`\`\`\`(sh|bash|shell|console|zsh)${SPACE}*$`);
const PROMPT = new RegExp(`^${SPACE}*\\$ `);
const WRAPPERS = new RegExp(`^${SPACE}*(sudo |exec |time |env [A-Z_]+=[^ ]* )*`);

function call(line: number, words: string[]): Call {
  return { line, tool: words[0] ?? "", first: words[1] ?? "", second: words[2] ?? "" };
}

/**
 * Each phux or just call in a sh/bash/shell/console/zsh block, or in an
 * inline code span in prose. A `$ ` prompt is dropped; comments are not commands.
 */
export function commandCalls(text: string): Call[] {
  const calls: Call[] = [];
  let fence = false;
  let shell = false;
  for (const [index, content] of lines(text).entries()) {
    if (BACKTICK_FENCE.test(content)) {
      shell = !fence && SHELL_FENCE.test(content);
      fence = !fence;
    } else if (!fence) {
      calls.push(...spanCalls(index + 1, content));
    } else if (shell) {
      calls.push(...shellCalls(index + 1, content));
    }
  }
  return calls;
}

/** Calls in inline code spans on a prose line. */
function spanCalls(line: number, content: string): Call[] {
  return [...matches(content, /`(just|phux) [^`]+`/)].map((m) =>
    call(line, m[0].slice(1, -1).split(WORDS)),
  );
}

/** Calls on one shell line: each `;`, `&`, `|` or `$(` segment, past wrappers and VAR=value. */
function shellCalls(line: number, content: string): Call[] {
  const code = content.replace(PROMPT, "").replace(/#.*$/s, "");
  const calls: Call[] = [];
  for (let segment of code.split(/[;&|]+|\$\(/)) {
    segment = segment.replace(WRAPPERS, "");
    while (/^[A-Z_][A-Z0-9_]*=[^ ]* /.test(segment)) segment = segment.replace(/^[^ ]* /, "");
    const words = segment.split(WORDS);
    if (words[0] === "phux" || words[0] === "just") calls.push(call(line, words));
  }
  return calls;
}

/** Hidden machine-invoked commands (crates/phux/src/tests/aliases.rs). */
const HIDDEN = new Set(["help", "gen-reference-docs", "stdio-bridge", "bootstrap"]);

interface Known {
  readonly paths: ReadonlySet<string>;
  readonly groups: ReadonlySet<string>;
  readonly recipes: ReadonlySet<string>;
}

/** What is wrong with one call, or undefined when it names a real recipe or command. */
export function callProblem({ tool, first, second }: Call, known: Known): string | undefined {
  if (!/^[a-z][a-z0-9-]*$/.test(first)) return undefined;
  if (tool === "just") {
    return known.recipes.has(first) ? undefined : `\`just ${first}\` is not a recipe`;
  }
  if (HIDDEN.has(first)) return undefined;
  if (!known.paths.has(first)) return `\`phux ${first}\` is not a command`;
  // A word after a command group must be one of its subcommands, unless the
  // group itself takes arguments.
  const group =
    known.groups.has(first) && [...known.paths].some((path) => path.startsWith(`${first} `));
  if (group && /^[a-z][a-z-]*$/.test(second) && !known.paths.has(`${first} ${second}`)) {
    return `\`phux ${first} ${second}\` is not a command`;
  }
  return undefined;
}

/** The names in the root-crate table under "## Crates", sorted and unique. Prose elsewhere may name client-workspace packages. */
export function crateTable(text: string): string[] {
  const named = new Set<string>();
  let inventory = false;
  for (const line of lines(text)) {
    if (line.startsWith("## ")) {
      inventory = line === "## Crates";
      continue;
    }
    if (!inventory || !new RegExp(`^${SPACE}*\\|${SPACE}*\``).test(line)) continue;
    const name = line.split("`")[1] ?? "";
    if (/^[a-z][a-z0-9-]*$/.test(name)) named.add(name);
  }
  return [...named].toSorted(byteOrder);
}

/** A table name that must be a crate: phux-* (but not the phux-dev executable), quictun, portable-pty-adopt. */
function isCrateName(name: string): boolean {
  if (name === "phux-dev") return false;
  return name.startsWith("phux-") || name === "quictun" || name === "portable-pty-adopt";
}

function listDirs(dir: string): string[] {
  try {
    return readdirSync(dir)
      .filter((name) => !name.startsWith(".") && isDir(`${dir}/${name}`))
      .toSorted(byteOrder);
  } catch {
    return [];
  }
}

const SOURCE = /\.(rs|h|c|m|mm|swift|ts|tsx|js|mjs|kt|java|py|sh|toml|zig|go)$/;
const SKIPPED_SOURCE = /(^|\/)(node_modules|target|vendor)\//;

/** Source files under crates/ and clients/ whose comments may cite a doc, relative to `root`. */
export function sourceFiles(root: string): string[] {
  const listed =
    find(root, ["crates", "clients"]) ??
    ["crates", "clients"].flatMap((dir) =>
      walk(`${root}/${dir}`)
        .map((rel) => `${dir}/${rel}`)
        .filter((rel) => isFile(`${root}/${rel}`))
        .toSorted(byteOrder),
    );
  return listed.filter(
    (rel) => SOURCE.test(rel) && !SKIPPED_SOURCE.test(rel) && existsSync(`${root}/${rel}`),
  );
}

const COMMENT_LINE = new RegExp(`^${SPACE}*(\\*|#)`);
const URL = /[a-z][a-z0-9+.-]*:\/\/[^ )>`"]*/g;
const DOC_PATH = /docs\/[A-Za-z0-9_./-]*[A-Za-z0-9_-]\.md(#[A-Za-z0-9_-]+)?/;
const RETIRED =
  /(^|[^A-Za-z0-9_/.-])(spec\/)?(L[0-9]|proto|input|workload-auth|appendix-[a-z]+|tui)\.md/;

/** The comment on a line: from the first `//` (not `://`) or `/*`, or a whole line that starts with `*` or `#`. */
function comment(line: string): string {
  if (COMMENT_LINE.test(line)) return line;
  let rest = line;
  for (;;) {
    const m = /\/\/|\/\*/.exec(rest);
    if (m === null) return "";
    if (m.index > 0 && rest[m.index - 1] === ":") {
      rest = rest.slice(m.index + 2);
      continue;
    }
    return rest.slice(m.index);
  }
}

/**
 * `[line, citation]` for each docs path, and `[line, "retired:<page>"]` for
 * each retired spec page name, in a comment. URLs are dropped first, so a
 * permalink into history is not a citation.
 */
export function citations(text: string): [number, string][] {
  const found: [number, string][] = [];
  for (const [index, content] of lines(text).entries()) {
    const line = index + 1;
    const text = comment(content).replace(URL, "");
    if (text === "") continue;
    for (const m of matches(text, DOC_PATH)) found.push([line, m[0]]);
    for (const m of matches(text, RETIRED))
      found.push([line, `retired:${m[0].replace(/^[^A-Za-z]/, "")}`]);
  }
  return found;
}

/** Nav groups. no-phux/website renders exactly these, in this order. */
export const NAV_GROUPS = [
  "start",
  "customize",
  "automate",
  "connect",
  "build",
  "help",
  "internals",
];

/** Each `key: value` of a leading `---` frontmatter block; a later key wins. */
export function frontmatter(text: string): Map<string, string> {
  const fields = new Map<string, string>();
  const all = lines(text);
  if (all[0] !== "---") return fields;
  for (const line of all.slice(1)) {
    if (line === "---") break;
    const m = new RegExp(`^[a-z]+:${SPACE}*`).exec(line);
    if (m === null) continue;
    const key = line.slice(0, line.indexOf(":"));
    fields.set(key, line.slice(m[0].length).replace(/^\t+|\t+$/g, ""));
  }
  return fields;
}

export function navProblems(text: string): string[] {
  const fields = frontmatter(text);
  if (fields.get("publish") === "false") return [];
  const problems: string[] = [];
  const group = fields.get("group") ?? "";
  const listed = NAV_GROUPS.join(" ");
  if (group === "") {
    problems.push(`no frontmatter group:; add one of ${listed} or publish: false`);
  } else if (!` ${listed} `.includes(` ${group} `)) {
    problems.push(`group ${group} is not one of ${listed}`);
  }
  if (!/^-?[0-9]+(\.[0-9]+)?$/.test(fields.get("order") ?? "")) {
    problems.push("frontmatter order: must be a number");
  }
  return problems;
}

export interface Report {
  readonly files: number;
  readonly violations: readonly Violation[];
}

/** Run every gate, or only `only`, over the tree at `root`. */
export function checkDocs(root: string, only?: Gate): Report {
  const files = markdownFiles(root);
  const gates = new Gates(root, files);
  for (const gate of GATES) {
    if (only === undefined || gate === only) gates[gate]();
  }
  return { files: files.length, violations: gates.violations };
}
