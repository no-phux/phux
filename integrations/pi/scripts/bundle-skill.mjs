import { copyFile, mkdir } from "node:fs/promises";

const destination = new URL("../skills/using-phux-tools/", import.meta.url);
await mkdir(destination, { recursive: true });
await copyFile(
  new URL("../../../.agents/skills/using-phux-tools/SKILL.md", import.meta.url),
  new URL("SKILL.md", destination),
);
