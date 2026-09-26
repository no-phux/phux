import { mkdirSync, readFileSync, renameSync, writeFileSync } from "node:fs";
import { dirname } from "node:path";

export function fileLayoutStore(path: string) {
  return {
    read(): unknown {
      try {
        return parseJson(readFileSync(path, "utf8"));
      } catch (error) {
        if (isMissing(error)) return undefined;
        const damaged = `${path}.damaged`;
        renameSync(path, damaged);
        return undefined;
      }
    },
    write(layout: unknown): void {
      mkdirSync(dirname(path), { recursive: true });
      const next = `${path}.next`;
      writeFileSync(next, `${JSON.stringify(layout)}\n`);
      renameSync(next, path);
    },
  };
}

function parseJson(text: string): unknown {
  // SAFETY: JSON.parse is untyped. Callers validate the value before reading fields.
  return JSON.parse(text) as unknown;
}

function isMissing(error: unknown): boolean {
  return typeof error === "object" && error !== null && "code" in error && error.code === "ENOENT";
}
