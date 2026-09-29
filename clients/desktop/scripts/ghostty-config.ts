/**
 * Locate and read the user's Ghostty configuration as one text blob for
 * `parseGhostty`: the named theme first (so the config's own lines override
 * it), then the main file, then its `config-file` includes, one level deep.
 * Returns undefined when Ghostty is not configured. Never throws.
 */
import { existsSync, readFileSync } from "node:fs";
import { homedir } from "node:os";
import { dirname, isAbsolute, join, resolve } from "node:path";

const home = homedir();
const configHome = process.env.XDG_CONFIG_HOME ?? join(home, ".config");
const CONFIGS = [
  join(configHome, "ghostty/config"),
  join(configHome, "ghostty/config.ghostty"),
  join(home, "Library/Application Support/com.mitchellh.ghostty/config"),
  join(home, "Library/Application Support/com.mitchellh.ghostty/config.ghostty"),
];
const THEME_DIRS = [
  join(configHome, "ghostty/themes"),
  "/Applications/Ghostty.app/Contents/Resources/ghostty/themes",
];

export function readGhosttyConfig(): string | undefined {
  const main = CONFIGS.find((path) => existsSync(path));
  if (!main) return undefined;
  const text = read(main);
  if (text === undefined) return undefined;
  const includes = [...text.matchAll(/^\s*config-file\s*=\s*"?(\??)([^"\n]+?)"?\s*$/gm)].flatMap(
    (match) => {
      const path = (match[2] ?? "").trim();
      const absolute = isAbsolute(path)
        ? path
        : resolve(dirname(main), path.replace(/^~\//, `${home}/`));
      return read(absolute) ?? [];
    },
  );
  const all = [text, ...includes].join("\n");
  const theme = themeText(all);
  return [theme ?? "", all].join("\n");
}

/** The last `theme =` wins; `light:x,dark:y` picks the dark variant. */
function themeText(config: string): string | undefined {
  const names = [...config.matchAll(/^\s*theme\s*=\s*"?([^"\n]+?)"?\s*$/gm)];
  const value = names.at(-1)?.[1]?.trim();
  if (!value) return undefined;
  const dark = /(?:^|,)\s*dark:([^,]+)/.exec(value)?.[1];
  const plain = value.includes(":") ? undefined : value;
  const name = (dark ?? plain)?.trim();
  if (!name) return undefined;
  if (isAbsolute(name)) return read(name);
  for (const dir of THEME_DIRS) {
    const text = read(join(dir, name));
    if (text !== undefined) return text;
  }
  return undefined;
}

function read(path: string): string | undefined {
  try {
    return readFileSync(path, "utf8");
  } catch {
    return undefined;
  }
}
