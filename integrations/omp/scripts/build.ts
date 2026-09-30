import { copyFile, mkdir } from "node:fs/promises";
import { resolve } from "node:path";

const root = resolve(import.meta.dir, "..");
const result = await Bun.build({
  entrypoints: [resolve(root, "src/index.ts")],
  outdir: resolve(root, "dist"),
  target: "bun",
  format: "esm",
});
if (!result.success) throw new AggregateError(result.logs, "Unable to bundle native OMP extension");
const skillDirectory = resolve(root, "skills/using-phux-tools");
await mkdir(skillDirectory, { recursive: true });
await copyFile(
  resolve(root, "../../.agents/skills/using-phux-tools/SKILL.md"),
  resolve(skillDirectory, "SKILL.md"),
);
