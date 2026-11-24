// Markdown reading shared by the docs gates: lines, fences, inline links,
// GitHub heading anchors, and a path check that is case-exact everywhere.
import { existsSync, readFileSync, readdirSync } from "node:fs";

/** POSIX `[[:space:]]`: what the awk-era gate matched, not JavaScript's Unicode `\s`. */
export const SPACE = "[ \\t\\n\\r\\f\\v]";

/** A fence opens or closes on a line that starts with ``` or ~~~. */
export const FENCE = new RegExp(`^${SPACE}*(\`\`\`|~~~)`);

/** A file's lines, as awk reads records: a trailing newline ends the last line. */
export function lines(text: string): string[] {
  const parts = text.split("\n");
  if (parts.at(-1) === "") parts.pop();
  return parts;
}

/** Each match of `re` scanning left to right, the rest re-matched from its own start (awk's `match` loop). */
export function* matches(text: string, re: RegExp): Generator<RegExpExecArray> {
  let rest = text;
  for (;;) {
    const found = re.exec(rest);
    if (found === null) return;
    yield found;
    rest = rest.slice(found.index + found[0].length);
  }
}

/** A line with its inline code spans dropped, so `[a](b)` in backticks is not a link. */
export function withoutCode(line: string): string {
  return line.replace(/`[^`]*`/g, "");
}

/** Each line outside a ``` or ~~~ fence, with its 1-based line number. */
export function* prose(text: string): Generator<[number, string]> {
  let fence = false;
  for (const [index, line] of lines(text).entries()) {
    if (FENCE.test(line)) {
      fence = !fence;
      continue;
    }
    if (!fence) yield [index + 1, line];
  }
}

/** A heading's GitHub anchor before de-duplication: link text kept, lowercase, punctuation dropped. */
function slug(heading: string): string {
  let h = heading.replace(/^#+ +/, "").replace(/ +#+ *$/, "");
  for (;;) {
    const link = /\[[^\]]*\]\([^)]*\)/.exec(h);
    if (link === null) break;
    const text = link[0].slice(1, link[0].indexOf("]("));
    h = h.slice(0, link.index) + text + h.slice(link.index + link[0].length);
  }
  return h
    .replace(/[A-Z]/g, (c) => c.toLowerCase())
    .replace(/[^a-z0-9 _-]/g, "")
    .replace(/ /g, "-");
}

/**
 * Every heading anchor in a Markdown text the way GitHub slugs it, with a -N
 * suffix for repeats. `explicit` also counts `<a name="...">` and `<a id="...">`.
 */
export function headingAnchors(text: string, explicit: boolean): string[] {
  const seen = new Map<string, number>();
  const anchors: string[] = [];
  for (const [, line] of prose(text)) {
    if (/^#{1,6} /.test(line)) {
      const h = slug(line);
      const count = seen.get(h);
      seen.set(h, count === undefined ? 0 : count + 1);
      anchors.push(count === undefined ? h : `${h}-${count + 1}`);
    }
    if (explicit && /<a (name|id)="[^"]+"/.test(line)) {
      anchors.push(line.replace(/^.*<a (name|id)="/s, "").replace(/".*$/s, ""));
    }
  }
  return anchors;
}

export function readText(path: string): string | undefined {
  try {
    return readFileSync(path, "utf8");
  } catch {
    return undefined;
  }
}

/**
 * Path existence where every component must match a directory entry exactly.
 * `existsSync` alone passes `INSTALL.md` for `install.md` on a
 * case-insensitive filesystem; Linux CI and GitHub would not.
 */
export class Paths {
  private readonly known = new Set<string>();
  private readonly listings = new Map<string, Set<string>>();
  private readonly anchors = new Map<string, Set<string>>();

  private entries(dir: string): Set<string> {
    let names = this.listings.get(dir);
    if (names === undefined) {
      try {
        names = new Set(readdirSync(dir));
      } catch {
        names = new Set();
      }
      this.listings.set(dir, names);
    }
    return names;
  }

  /** Concatenates rather than normalizes: `a/b/..` must resolve through `b` the way the OS does. */
  exists(path: string): boolean {
    if (this.known.has(path)) return true;
    let parent = "/";
    for (const component of path.split("/")) {
      if (component === "" || component === ".") continue;
      const child = `${parent === "/" ? "" : parent}/${component}`;
      if (component !== ".." && !this.hasEntry(parent, component, child)) return false;
      parent = child;
    }
    if (!existsSync(parent)) return false;
    this.known.add(path);
    return true;
  }

  /** `dir` lists `name` exactly and `child` (its path) exists, symlinks followed. */
  private hasEntry(dir: string, name: string, child: string): boolean {
    return this.entries(dir).has(name) && existsSync(child);
  }

  /** Whether a Markdown file has a heading (or explicit) anchor; an unreadable file has none. */
  hasAnchor(file: string, anchor: string): boolean {
    let set = this.anchors.get(file);
    if (set === undefined) {
      set = new Set(headingAnchors(readText(file) ?? "", true));
      this.anchors.set(file, set);
    }
    return set.has(anchor);
  }
}

/** A link target split at its first `#`: the path, and the anchor when there is a `#`. */
export function splitAnchor(target: string): [string, string] {
  const hash = target.indexOf("#");
  return hash === -1 ? [target, ""] : [target.slice(0, hash), target.slice(hash + 1)];
}
