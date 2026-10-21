import { existsSync, readdirSync, readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

import { defineExtension, section, type Extension } from "@earendil-works/pi-durable";

import type { ChiefConfig, ChiefPaths } from "./config.js";

/** `assets/` beside `src/` and `dist/src/`. */
export const ASSETS = join(dirname(fileURLToPath(import.meta.url)), ...(import.meta.url.includes("/dist/") ? ["..", "..", "assets"] : ["..", "assets"]));

export interface PersonaFacts {
  readonly owner: string;
  readonly pane: string | undefined;
  readonly host: string;
}

/** Shipped persona with `{{field}}` filled in, plus the user's addendum. */
export function renderPersona(config: ChiefConfig, paths: ChiefPaths, facts: PersonaFacts, assets = ASSETS): string {
  const fields: Record<string, string> = {
    owner: facts.owner,
    session: config.session,
    pane: facts.pane ?? "your own pane",
    owned_repos: config.ownedRepos.length > 0 ? config.ownedRepos.join(", ") : "none configured (ask before pushing anywhere)",
    host: facts.host,
  };
  const fill = (text: string) => text.replace(/\{\{(\w+)\}\}/g, (match, key: string) => fields[key] ?? match);
  const parts = [fill(readFileSync(join(assets, "persona.md"), "utf8").trim())];
  if (existsSync(paths.persona)) parts.push(fill(readFileSync(paths.persona, "utf8").trim()));
  parts.push(`You run on host ${facts.host}${facts.pane === undefined ? "" : ` in pane ${facts.pane}`}. Messages the phone sent while locked and later released are marked "[sent while the phone was locked]".`);
  return parts.join("\n\n");
}

/** Every skill file, shipped first, then the user's; a user file with the same name wins. */
export function loadSkills(paths: ChiefPaths, assets = ASSETS): string {
  const files = new Map<string, string>();
  for (const dir of [join(assets, "skills"), paths.skills]) {
    if (!existsSync(dir)) continue;
    for (const name of readdirSync(dir).filter((file) => file.endsWith(".md")).sort()) files.set(name, join(dir, name));
  }
  return [...files.entries()].map(([name, path]) => `## ${name.replace(/\.md$/, "")}\n${readFileSync(path, "utf8").replace(/^---[\s\S]*?---\n/, "").trim()}`).join("\n\n");
}

/** The persona and skills as prompt sections, re-read before each request so edits apply on the next turn. */
export function createPersona(config: ChiefConfig, paths: ChiefPaths, facts: PersonaFacts): Extension {
  return defineExtension({
    name: "chief-persona",
    sections: [
      section("persona", () => renderPersona(config, paths, facts)),
      section("skills", () => {
        const skills = loadSkills(paths).replaceAll("{{session}}", config.session);
        return skills.length > 0 ? skills : undefined;
      }),
    ],
  });
}
