#!/usr/bin/env bun
/**
 * sync-agent-skills.ts — publish the repo's product skills to the site.
 *
 * Copies `.agents/skills/{using-phux,using-phux-mcp}/SKILL.md` into
 * `public/.well-known/agent-skills/<name>/` and regenerates
 * `public/.well-known/agent-skills/index.json` (Agent Skills Discovery
 * RFC v0.2.0) with a sha256 digest per skill. Like `_synced/`, the copied
 * files are build inputs, not hand-editable artifacts — fix the source skill.
 *
 * `beads` is maintainer task-tracking tooling, not a product skill, so it is
 * intentionally not published.
 */
import { createHash } from "node:crypto";
import { copyFile, mkdir, readdir, readFile, writeFile } from "node:fs/promises";
import { dirname, join, resolve } from "node:path";

const ROOT = resolve(import.meta.dirname, "../../.."); // the phux repo root
const SKILLS_DIR = join(ROOT, ".agents", "skills");
const OUT_DIR = resolve(import.meta.dirname, "../public/.well-known/agent-skills");
const PUBLISHED = ["using-phux", "using-phux-mcp"];

async function main() {
  const skills = [];
  for (const name of PUBLISHED) {
    const source = join(SKILLS_DIR, name, "SKILL.md");
    const body = await readFile(source, "utf8");
    const digest = createHash("sha256").update(body).digest("hex");
    const description = frontmatterDescription(body) ?? `${name} skill`;
    const outPath = join(OUT_DIR, name, "SKILL.md");
    await mkdir(dirname(outPath), { recursive: true });
    await copyFile(source, outPath);
    skills.push({
      name,
      type: "skill-md",
      description,
      url: `/.well-known/agent-skills/${name}/SKILL.md`,
      digest: `sha256:${digest}`,
    });
    console.log(`sync-agent-skills: ${name} -> ${digest.slice(0, 12)}…`);
  }

  const index = {
    $schema: "https://schemas.agentskills.io/discovery/0.2.0/schema.json",
    skills,
  };
  await mkdir(OUT_DIR, { recursive: true });
  await writeFile(
    join(OUT_DIR, "index.json"),
    `${JSON.stringify(index, null, 2)}\n`,
    "utf8",
  );
  console.log(`sync-agent-skills: published ${skills.length} skills -> index.json`);
}

function frontmatterDescription(body: string): string | null {
  if (!body.startsWith("---\n")) return null;
  const end = body.indexOf("\n---", 4);
  if (end === -1) return null;
  const line = body
    .slice(4, end)
    .split("\n")
    .find((candidate) => candidate.startsWith("description:"));
  return line?.slice("description:".length).trim().replace(/^"(.*)"\s*$/, "$1") ?? null;
}

await readdir(SKILLS_DIR).then(main);
