/** The golden vectors (spec §2.7): the full set the tests check, and the subset phux commits. */
import { createHash } from "node:crypto";
import {
  SOLVER,
  contrast,
  decodeLut,
  derive,
  fade,
  hex,
  mix,
  onFill,
  over,
  parse,
  reach,
  rgb,
  type Rgb,
  type Theme,
} from "./derive.ts";
import { HEADER } from "./emit/model.ts";
import { paramsSha256, v10 } from "./fit.ts";
import type { Sources } from "./sources.ts";
import { plural, toneOf, type ToneInput } from "./status.ts";
import type { Resolved } from "./tokens.ts";
import { verify } from "./verify.ts";

/** The synthetic edges of design/ref/build_themes.py; every case carries cursor, selection_bg and dark. */
const GREY = Array(16).fill("#808080");
const SOLARIZED = [
  "#073642",
  "#DC322F",
  "#859900",
  "#B58900",
  "#268BD2",
  "#D33682",
  "#2AA198",
  "#EEE8D5",
  "#002B36",
  "#CB4B16",
  "#586E75",
  "#657B83",
  "#839496",
  "#6C71C4",
  "#93A1A1",
  "#FDF6E3",
];
const TOKYO = [
  "#15161E",
  "#F7768E",
  "#9ECE6A",
  "#E0AF68",
  "#7AA2F7",
  "#BB9AF7",
  "#7DCFFF",
  "#A9B1D6",
];
export const EDGES: (Theme & { id: string })[] = [
  {
    id: "edge/black-white",
    bg: "#000000",
    fg: "#FFFFFF",
    cursor: null,
    accent: null,
    selection_bg: null,
    dark: null,
    ansi: GREY,
  },
  {
    id: "edge/white-black",
    bg: "#FFFFFF",
    fg: "#000000",
    cursor: null,
    accent: null,
    selection_bg: null,
    dark: null,
    ansi: GREY,
  },
  {
    id: "edge/threshold-grey",
    bg: "#777777",
    fg: "#FFFFFF",
    cursor: null,
    accent: null,
    selection_bg: null,
    dark: null,
    ansi: GREY,
  },
  {
    id: "edge/fg-equals-bg",
    bg: "#202020",
    fg: "#202020",
    cursor: null,
    accent: null,
    selection_bg: null,
    dark: null,
    ansi: GREY,
  },
  {
    id: "edge/low-contrast-solarized",
    bg: "#002B36",
    fg: "#586E75",
    cursor: null,
    accent: null,
    selection_bg: null,
    dark: null,
    ansi: SOLARIZED,
  },
  {
    id: "edge/accent-equals-working",
    bg: "#1A1B26",
    fg: "#C0CAF5",
    cursor: null,
    accent: "#7AA2F7",
    selection_bg: null,
    dark: null,
    ansi: [...TOKYO, ...TOKYO],
  },
  {
    id: "edge/explicit-selection",
    bg: "#FDF6E3",
    fg: "#657B83",
    cursor: null,
    accent: null,
    selection_bg: "#073642",
    dark: null,
    ansi: SOLARIZED,
  },
];

const TONE_CASES: ToneInput[] = [
  { agent: "Working" },
  { agent: "Blocked" },
  { agent: "Idle" },
  { agent: "Done" },
  { agent: "Done", seen: true },
  { agent: "Unknown" },
  {},
  { agent: "Idle", attention: true },
  { agent: "Working", asked: true },
  { agent: "Done", exit: 1 },
  { agent: "Blocked", exit: 2 },
  { agent: "Working", crashed: true },
  { agent: "Done", exit: 0 },
  { agent: "Done", exit: 0, seen: true },
  { exit: 0 },
  { agent: "Unknown", exit: 0 },
  { agent: "Working", link: "lost" },
  { agent: "Blocked", link: "lost" },
  { agent: "Working", link: "ok" },
  { agent: "Idle", exit: null },
];

function inputOf(t: Theme): Theme {
  return {
    bg: t.bg,
    fg: t.fg,
    cursor: t.cursor ?? null,
    accent: t.accent ?? null,
    selection_bg: t.selection_bg ?? null,
    dark: t.dark ?? null,
    ansi: t.ansi,
  };
}

export function vectors(R: Resolved, S: Sources): any {
  const P = R.params,
    lut = decodeLut(P.srgb_lut);
  // OKLab leans on libm pow and cbrt, which differ by an ulp between platforms; the 1e-9 grid
  // (the published verify tolerance) keeps the committed vectors byte-identical everywhere.
  const cases = [...R.catalog.themes.map((t) => ({ ...t, id: t.id })), ...EDGES].map((t) => {
    const input = inputOf(t),
      roles = derive(input, P);
    return {
      id: t.id,
      input,
      expect: roles,
      verify: verify(roles, P, { context: "derived", cursor: input.cursor ?? null, bg: input.bg }),
      verify_oklab: v10(roles).map((v) => ({ ...v, value: Math.round(v.value * 1e9) / 1e9 })),
    };
  });
  const pal = (s: string) => rgb(s);
  const contrastCases = [
    ["#777777", "#FFFFFF"],
    ["#000000", "#FFFFFF"],
    ["#E8E5DF", "#272B31"],
    ["#17191C", "#EAE9E5"],
    ["#808080", "#808080"],
  ].map(([a, b]) => ({ a, b, expect: contrast(pal(a!), pal(b!), lut) }));
  const mixCases = [
    ["#0D1017", "#D8DEE9", 0.065],
    ["#F3F2EF", "#FFFFFF", 0.969],
    ["#141619", "#E8E5DF", 0.5],
    ["#000000", "#FFFFFF", 0.5],
  ].map(([a, b, t]) => ({
    a,
    b,
    t,
    expect: hex(mix(pal(a as string), pal(b as string), t as number)),
  }));
  const overCases = [
    ["#D8DEE924", "#15181F"],
    ["#E8E5DF0F", "#141619"],
    ["#17191C1C", "#EAE9E5"],
    ["#00000080", "#FFFFFF"],
  ].map(([top, bottom]) => ({ top, bottom, expect: hex(over(parse(top!), pal(bottom!))) }));
  const S4 = ["#1F1F28", "#212129", "#25252D", "#28282F"];
  const reachCases = [
    { c: "#C34043", toward: "#FFFFFF", bar: 5.0, S: S4 },
    { c: "#9C6200", toward: "#000000", bar: 4.5, S: ["#EAE9E5", "#D7D6D3"] },
    { c: "#80858C", toward: "#FFFFFF", bar: 4.5, S: ["#272B31"] },
    { c: "#808080", toward: "#FFFFFF", bar: 25, S: ["#000000"] },
  ].map((x) => ({
    ...x,
    expect: hex(reach(pal(x.c), pal(x.toward), x.bar, x.S.map(pal), lut, P.iterations)),
  }));
  const fadeCases = [
    { c: "#E8E5DF", toward: "#272B31", target: 8.0, S: ["#141619", "#272B31"] },
    { c: "#17191C", toward: "#EAE9E5", target: 6.3, S: ["#EAE9E5"] },
  ].map((x) => ({ ...x, expect: hex(fadeHex(x, lut, P.iterations)) }));
  const onFillCases = ["#DAD6CF", "#2A2D32", "#777777", "#767676", "#F2C477"].map((f) => ({
    f,
    expect: hex(onFill(pal(f), lut)),
  }));
  const words = { ...S.vocabulary.plurals };
  return {
    schema: 2,
    solver: SOLVER,
    params_sha256: paramsSha256(P),
    params: P,
    notes:
      "verify lists hold V1-V9, V11-V13 and V17 as every port computes them (context derived). V10 needs OKLab, which only the generator computes; it is listed apart in verify_oklab. Colours compare as exact strings; contrast within 1e-12; verify values within 1e-9.",
    primitives: {
      contrast: contrastCases,
      mix: mixCases,
      over: overCases,
      reach: reachCases,
      fade: fadeCases,
      on_fill: onFillCases,
    },
    tone_of: TONE_CASES.map((input) => ({ input, expect: toneOf(input) })),
    plural: Object.keys(words).flatMap((key) =>
      [0, 1, 2, 21].map((count) => ({ key, count, expect: plural(words[key], count) })),
    ),
    cases,
  };
}

function fadeHex(
  x: { c: string; toward: string; target: number; S: string[] },
  lut: Float64Array,
  n: number,
): Rgb {
  return fade(rgb(x.c), rgb(x.toward), x.target, x.S.map(rgb), lut, n);
}

/** The catalog cases the committed vector subset keeps: the house pair, a dark and a light catalog look, and four edges. */
const SUBSET_CASES = [
  "phux",
  "phux-light",
  "nord",
  "solarized-light",
  "edge/black-white",
  "edge/fg-equals-bg",
  "edge/low-contrast-solarized",
  "edge/explicit-selection",
];

/**
 * The golden vectors phux commits: every primitive, tone_of and plural case, and the SUBSET_CASES
 * derivations without their verify lists. The full set is recomputed by tokens.test.ts, never committed.
 */
/** `path=value` for every leaf of a role set, sorted: a layout-free form both ports can print. */
function leaves(v: unknown, path = ""): string[] {
  if (v === null || typeof v !== "object") return [`${path}=${String(v)}`];
  return Object.entries(v).flatMap(([k, x]) => leaves(x, path ? `${path}.${k}` : k));
}

/**
 * sha256 digests of every case's derived roles and verify list, so a port that checks only the
 * committed cases still checks all of them. Verify values print at 6 decimals.
 */
export function caseDigests(v: any): { cases: number; roles: string; verify: string } {
  const sha = (text: string) => createHash("sha256").update(text).digest("hex");
  const roles = v.cases
    .map((c: any) => `${c.id}\n${leaves(c.expect).sort().join("\n")}\n`)
    .join("");
  const checks = v.cases
    .map(
      (c: any) =>
        `${c.id}\n${c.verify.map((x: any) => `${x.id} ${x.severity} ${x.role} ${x.surface} ${x.value.toFixed(6)} ${x.bar}\n`).join("")}`,
    )
    .join("");
  return { cases: v.cases.length, roles: sha(roles), verify: sha(checks) };
}

export function vectorSubset(v: any): string {
  const rows = (xs: unknown[], pad: string) =>
    `[\n${xs.map((x) => pad + JSON.stringify(x)).join(",\n")}\n${pad.slice(2)}]`;
  const cases = SUBSET_CASES.map((id) => {
    const c = v.cases.find((x: any) => x.id === id);
    return { id, input: c.input, expect: c.expect };
  });
  const primitives = Object.entries(v.primitives).map(
    ([k, xs]) => `    ${JSON.stringify(k)}: ${rows(xs as unknown[], "      ")}`,
  );
  return [
    "{",
    `  "source": ${JSON.stringify(`${HEADER}; a subset of the golden vectors, which the generator tests check in full`)},`,
    `  "solver": ${JSON.stringify(v.solver)},`,
    `  "params_sha256": ${JSON.stringify(v.params_sha256)},`,
    `  "primitives": {\n${primitives.join(",\n")}\n  },`,
    `  "tone_of": ${rows(v.tone_of, "    ")},`,
    `  "plural": ${rows(v.plural, "    ")},`,
    `  "cases": ${rows(cases, "    ")},`,
    `  "edges": ${rows(
      EDGES.map(inputOf).map((input, i) => ({ id: EDGES[i]!.id, ...input })),
      "    ",
    )},`,
    `  "digests": ${JSON.stringify(caseDigests(v))}`,
    "}",
    "",
  ].join("\n");
}
