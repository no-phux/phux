/** Load and validate the authored token sources (crates/phux-design/tokens/src). */
import { readFile } from "node:fs/promises";
import { resolve } from "node:path";
import {
  TONES,
  WASHES,
  parse,
  type CoreRoles,
  type Mode,
  type Roles,
  type Theme,
} from "./derive.ts";

export const MODES: Mode[] = ["dark", "light"];
export const SOURCES = "crates/phux-design/tokens/src";
const HEX = /^#[0-9A-F]{6}([0-9A-F]{2})?$/;

export interface Sources {
  version: string;
  house: any;
  deriveConstants: any;
  vocabulary: any;
  status: any;
  identity: any;
  reader: any;
  forcedColors: any;
  catalog: any;
  schema: any;
}

export interface CatalogTheme {
  id: string;
  name: string;
  bg: string;
  fg: string;
  cursor: string;
  accent: string | null;
  selection_bg: string | null;
  dark: boolean;
  ansi: string[];
  ships: string[];
  icon: boolean;
}

async function json(path: string): Promise<any> {
  return JSON.parse(await readFile(path, "utf8"));
}

export async function loadSources(root: string): Promise<Sources> {
  const src = resolve(root, SOURCES);
  return {
    version: (await readFile(resolve(src, "version.txt"), "utf8")).trim(),
    house: await json(resolve(src, "house.json")),
    deriveConstants: await json(resolve(src, "derive.json")),
    vocabulary: await json(resolve(src, "vocabulary.json")),
    status: await json(resolve(src, "status.json")),
    identity: await json(resolve(src, "identity.json")),
    reader: await json(resolve(src, "variants/reader.json")),
    forcedColors: await json(resolve(src, "variants/forced_colors.json")),
    catalog: await json(resolve(src, "themes.json")),
    schema: await json(resolve(src, "phux.tokens.schema.json")),
  };
}

// --- source validation ----------------------------------------------------------------

function colour(path: string, v: unknown): string {
  if (typeof v !== "string" || !HEX.test(v))
    throw Error(`${path}: expected #RRGGBB or #RRGGBBAA, got ${JSON.stringify(v)}`);
  return v;
}
function opaque(path: string, v: unknown): string {
  colour(path, v);
  if ((v as string).length !== 7) throw Error(`${path}: must be opaque #RRGGBB`);
  return v as string;
}

export const ROLE_KEYS = {
  layer: ["ground", "sidebar", "raised", "overlay", "scrim"],
  text: ["text", "dim", "faint", "inverse"],
  border: ["muted", "base", "strong", "control", "focus"],
  wash: ["hover", "press", "current", "selected"],
  accent: ["accent", "on_accent", "accent_text"],
  status: [...TONES],
  selection: ["bg", "fg"],
} as const;
const OPAQUE_GROUPS = new Set(["text", "accent", "status", "selection"]);

/** The authored house colours: every core role, closed, opaque where §1.2 requires it. */
export function validateHouseColor(mode: Mode, c: any): void {
  for (const [group, keys] of Object.entries(ROLE_KEYS)) {
    const g = c[group];
    if (!g) throw Error(`house.color.${mode}.${group} is missing`);
    const allowed = new Set<string>([...keys, ...(group === "accent" ? ["hi", "lo"] : [])]);
    for (const k of Object.keys(g))
      if (!allowed.has(k)) throw Error(`house.color.${mode}.${group}.${k} is not a schema role`);
    for (const k of keys) {
      const path = `house.color.${mode}.${group}.${k}`;
      const opaqueRole =
        OPAQUE_GROUPS.has(group) ||
        (group === "layer" && k !== "scrim") ||
        k === "control" ||
        k === "focus";
      (opaqueRole ? opaque : colour)(path, g[k]);
    }
  }
  for (const k of ["muted", "base", "strong"])
    if (parse(c.border[k])[3] === 255)
      throw Error(`house.color.${mode}.border.${k} must be translucent ink`);
  for (const k of WASHES)
    if (parse(c.wash[k])[3] === 255)
      throw Error(`house.color.${mode}.wash.${k} must be translucent`);
}

export function validateCatalog(catalog: any): CatalogTheme[] {
  if (catalog.schema !== 1 || !Array.isArray(catalog.themes))
    throw Error("themes.json: unsupported catalog");
  const ids = new Set<string>();
  for (const t of catalog.themes as CatalogTheme[]) {
    if (!/^[a-z0-9]+(-[a-z0-9]+)*$/.test(t.id) || ids.has(t.id))
      throw Error(`themes.json: bad or duplicate id ${t.id}`);
    ids.add(t.id);
    const keys = [
      "id",
      "name",
      "bg",
      "fg",
      "cursor",
      "accent",
      "selection_bg",
      "dark",
      "ansi",
      "ships",
      "icon",
    ];
    if (Object.keys(t).sort().join() !== [...keys].sort().join())
      throw Error(`themes.json ${t.id}: keys must be exactly ${keys.join(", ")}`);
    for (const k of ["bg", "fg", "cursor"] as const) opaque(`themes.json ${t.id}.${k}`, t[k]);
    for (const k of ["accent", "selection_bg"] as const)
      if (t[k] !== null) opaque(`themes.json ${t.id}.${k}`, t[k]);
    if (typeof t.dark !== "boolean" || typeof t.icon !== "boolean" || typeof t.name !== "string")
      throw Error(`themes.json ${t.id}: bad types`);
    if (t.ansi.length !== 16) throw Error(`themes.json ${t.id}: 16 ansi colours`);
    t.ansi.forEach((a, i) => opaque(`themes.json ${t.id}.ansi[${i}]`, a));
    if (!t.ships.length || t.ships.some((s) => !["mobile", "rune", "tui"].includes(s)))
      throw Error(`themes.json ${t.id}: bad ships`);
  }
  for (const [from, to] of Object.entries({
    ...catalog.aliases,
    ...Object.assign({}, ...Object.values(catalog.legacy ?? {})),
  }))
    if (!ids.has(to as string) && !["phux", "phux-light"].includes(to as string))
      throw Error(`themes.json: alias ${from} -> unknown ${String(to)}`);
  return catalog.themes;
}

// --- house roles ----------------------------------------------------------------------

/** The house look's core roles for one mode, from an authored colour block. */
export function coreOf(mode: Mode, c: any): CoreRoles {
  const pick = (g: string, keys: readonly string[]) =>
    Object.fromEntries(keys.map((k) => [k, c[g][k]]));
  return {
    mode,
    layer: pick("layer", ROLE_KEYS.layer) as Roles["layer"],
    text: pick("text", ROLE_KEYS.text) as Roles["text"],
    border: pick("border", ROLE_KEYS.border) as Roles["border"],
    wash: pick("wash", ROLE_KEYS.wash) as Roles["wash"],
    accent: {
      ...pick("accent", ROLE_KEYS.accent),
      ...(c.accent.hi ? { hi: c.accent.hi, lo: c.accent.lo } : {}),
    } as Roles["accent"],
    status: pick("status", TONES),
    selection: pick("selection", ROLE_KEYS.selection) as Roles["selection"],
  };
}

export function terminalTheme(t: any, mode: Mode): Theme {
  return {
    bg: t.bg,
    fg: t.fg,
    cursor: t.cursor,
    ansi: t.ansi,
    accent: null,
    selection_bg: t.selection_bg,
    dark: mode === "dark",
  };
}
