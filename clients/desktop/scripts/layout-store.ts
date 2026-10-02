import { randomUUID } from "node:crypto";
import {
  closeSync,
  fsyncSync,
  mkdirSync,
  openSync,
  readFileSync,
  renameSync,
  rmSync,
  writeFileSync,
} from "node:fs";
import { dirname } from "node:path";

export function fileLayoutStore(path: string, legacyPath?: string) {
  return {
    read(): unknown {
      try {
        return parseJson(readFileSync(path, "utf8"));
      } catch (error) {
        if (isMissing(error)) return legacyPath ? fileLayoutStore(legacyPath).read() : undefined;
        // Present but unreadable is not first launch. The consumer refuses
        // writes for this sentinel, leaving malformed or inaccessible data intact.
        return null;
      }
    },
    write(layout: unknown): void {
      mkdirSync(dirname(path), { recursive: true });
      // Concurrent app processes must never share/truncate a staging file.
      const next = `${path}.${randomUUID()}.next`;
      const fd = openSync(next, "wx", 0o600);
      try {
        writeFileSync(fd, `${JSON.stringify(layout)}\n`);
        fsyncSync(fd);
      } catch (error) {
        rmSync(next, { force: true });
        throw error;
      } finally {
        closeSync(fd);
      }
      try {
        renameSync(next, path);
      } catch (error) {
        rmSync(next, { force: true });
        throw error;
      }
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
