// Drift gate for docs/llms.txt and the Build on phux hub.
//
// The hosted file https://docs.phux.sh/llms.txt lives in no-phux/website
// (public/llms.txt). This repository does not generate it. docs/llms.txt is
// the builder index checked in here. The hub's "## Entry points" links and
// this index list the same targets, and every relative target resolves.
import { existsSync, statSync } from "node:fs";
import { basename, dirname, resolve } from "node:path";
import {
  headingAnchors,
  lines,
  matches,
  prose,
  readText,
  splitAnchor,
  withoutCode,
} from "./markdown.ts";

const LINK = /\]\(([^) ]+)\)/;
const EXTERNAL = /^(https?|mailto):/;

/** `[line, target]` for every relative link outside fences; no `"title"` form. */
export function relativeLinks(text: string): [number, string][] {
  const found: [number, string][] = [];
  for (const [line, content] of prose(text)) {
    for (const m of matches(withoutCode(content), LINK)) {
      const target = m[1] ?? "";
      if (!EXTERNAL.test(target)) found.push([line, target]);
    }
  }
  return found;
}

/** Relative link targets in the first "## Entry points" section, anchors kept. */
export function entryLinks(text: string): string[] {
  const found: string[] = [];
  let grab = false;
  for (const line of lines(text)) {
    if (line === "## Entry points") {
      grab = true;
      continue;
    }
    if (!grab) continue;
    if (line.startsWith("## ")) break;
    for (const m of matches(withoutCode(line), LINK)) {
      const target = m[1] ?? "";
      if (!EXTERNAL.test(target)) found.push(target);
    }
  }
  return found;
}

function isDir(path: string): boolean {
  try {
    return statSync(path).isDirectory();
  } catch {
    return false;
  }
}

/** Where a relative target lands from `dir`, or undefined when it does not exist. */
function resolveTarget(dir: string, rel: string): string | undefined {
  const parent = resolve(dir, dirname(rel));
  const name = basename(rel);
  if (!isDir(parent) || name === "" || !existsSync(`${parent}/${name}`)) return undefined;
  return `${parent}/${name}`;
}

/** What is wrong with one link in `file`, or undefined when it resolves. */
function linkProblem(file: string, link: string): string | undefined {
  const [path, anchor] = splitAnchor(link);
  // A bare #anchor is this file, the same rule as the links gate.
  if (path === "" && anchor === "") return "empty link";
  const target = path === "" ? file : resolveTarget(dirname(file), path);
  if (target === undefined) return `${link} does not exist`;
  if (anchor === "" || !target.endsWith(".md")) return undefined;
  const anchors = headingAnchors(readText(target) ?? "", false);
  return anchors.includes(anchor) ? undefined : `${link} has no heading #${anchor}`;
}

const REQUIRED_HUB_LINKS = [
  "./scripting.md#json-output-and-errors",
  "./scripting.md#watching-events",
  "./agents.md#mcp",
  "./reference/parity.md",
  "./reference/cli.md",
  "./protocol.md",
  "./compatibility.md",
  "./spec/wire-schema.json",
  "./program-protocol.md",
  "./plugins.md",
  "./agents.md#agent-setup",
  "./agent-quickstart.md#call-phux-from-a-program",
  "./embedding.md",
];

const HUB_PHRASES = [
  "evidence-ranked; screen is the floor",
  "It has not been measured on a live Claude Code, Codex, OpenCode, or pi session, with or without an integration installed.",
  "There is no published SDK or client library yet.",
  "There is no JSON socket.",
  "There is no example application.",
  "The static plugin index is built from the GitHub topic `phux-plugin`",
];

// This gate does not require every file under docs/; libghostty.md is not an entry point.
const INDEX_PHRASES = [
  "evidence-ranked; screen is the floor",
  "It has not been measured on a live Claude Code, Codex, OpenCode, or pi session",
  "There is no public SDK.",
  "There is no JSON socket.",
  "There is no example application.",
  "The static plugin index is built from the GitHub topic `phux-plugin`",
  "https://docs.phux.sh/llms.txt",
];

const BANNED =
  /no screen scrap|screen-scrap|never scrap|revolutionize|supercharge|seamless|[ \t\n\r\f\v]10x[ \t\n\r\f\v]|unlock/i;

class Llms {
  readonly problems: string[] = [];

  constructor(readonly root: string) {}

  private rel(file: string): string {
    return file.startsWith(`${this.root}/`) ? file.slice(this.root.length + 1) : file;
  }

  /** Every relative link in `file` resolves, and its #anchor names a heading. */
  links(file: string): void {
    for (const [line, link] of relativeLinks(readText(file) ?? "")) {
      const problem = linkProblem(file, link);
      if (problem !== undefined) this.problems.push(`${this.rel(file)}:${line} ${problem}`);
    }
  }

  require(file: string, phrase: string): void {
    if (!(readText(file) ?? "").includes(phrase)) {
      this.problems.push(`${this.rel(file)} is missing: ${phrase}`);
    }
  }

  forbid(file: string): void {
    const hits = lines(readText(file) ?? "").flatMap((line, i) =>
      BANNED.test(line) ? [`${i + 1}:${line}`] : [],
    );
    if (hits.length > 0) {
      this.problems.push(`${this.rel(file)} has a banned phrase\n  ${hits.join("\n  ")}`);
    }
  }

  entryPoints(hub: string, index: string): void {
    const hubLinks = new Set(entryLinks(readText(hub) ?? ""));
    const indexLinks = new Set(entryLinks(readText(index) ?? ""));
    const listed = new Set([...indexLinks].filter((link) => link !== "./building.md"));
    const onlyHub = [...hubLinks].filter((link) => !listed.has(link));
    const onlyIndex = [...listed].filter((link) => !hubLinks.has(link));
    if (onlyHub.length > 0 || onlyIndex.length > 0) {
      const diff = [
        ...onlyHub.map((l) => `  hub only: ${l}`),
        ...onlyIndex.map((l) => `  llms.txt only: ${l}`),
      ];
      this.problems.push(
        `docs/llms.txt Entry points and docs/building.md Entry points differ\n${diff.toSorted().join("\n")}`,
      );
    }
    if (!indexLinks.has("./building.md")) {
      this.problems.push("docs/llms.txt Entry points omit ./building.md");
    }
    for (const link of REQUIRED_HUB_LINKS) {
      if (!hubLinks.has(link)) this.problems.push(`hub Entry points omit ${link}`);
    }
  }
}

/** Problems with docs/llms.txt and docs/building.md under `root`, each without the `llms.txt: ` prefix. */
export function checkLlms(root: string): string[] {
  const check = new Llms(root);
  const hub = `${root}/docs/building.md`;
  const index = `${root}/docs/llms.txt`;
  for (const file of [hub, index]) {
    if (!existsSync(file)) check.problems.push(`missing ${file}`);
  }
  if (check.problems.length > 0) return check.problems;

  check.links(hub);
  check.links(index);
  check.entryPoints(hub, index);
  for (const phrase of HUB_PHRASES) check.require(hub, phrase);
  for (const phrase of INDEX_PHRASES) check.require(index, phrase);
  check.require(`${root}/docs/README.md`, "](./building.md)");
  check.require(`${root}/README.md`, "](./docs/building.md)");
  check.require(`${root}/AGENTS.md`, "## Building on phux");
  check.require(`${root}/AGENTS.md`, "](./docs/building.md)");
  for (const file of [hub, index, `${root}/AGENTS.md`]) check.forbid(file);
  return check.problems;
}

/** The links check alone, for one file (what the fixtures probe). */
export function checkLlmsLinks(root: string, file: string): string[] {
  const check = new Llms(root);
  check.links(file);
  return check.problems;
}
