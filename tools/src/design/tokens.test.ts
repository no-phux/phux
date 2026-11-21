import { describe, expect, test } from "bun:test";
import { readFile } from "node:fs/promises";
import { resolve } from "node:path";
import { ROOT } from "../lib/repo.ts";
import {
  TONES,
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
  type Theme,
} from "./derive.ts";
import { springAt, springConstants, springSamples, springSettle, settleTime } from "./emit/css.ts";
import { lowerCamel } from "./emit/model.ts";
import { committed, tokenExports } from "./exports.ts";
import { fitParams, paramsSha256 } from "./fit.ts";
import { validate } from "./schema.ts";
import { loadSources } from "./sources.ts";
import { plural, toneOf } from "./status.ts";
import { resolveTokens, rolesDiff } from "./tokens.ts";
import { EDGES, vectors as goldenVectors } from "./vectors.ts";
import { type Violation } from "./derive.ts";
import { verify } from "./verify.ts";

const S = await loadSources(ROOT);
const R = resolveTokens(S);
// phux commits neither the resolved document nor the full vectors; the tests read them as JSON would.
const doc = JSON.parse(JSON.stringify(R.doc));
const vectors = JSON.parse(JSON.stringify(goldenVectors(R, S)));
const lut = decodeLut(R.params.srgb_lut);

describe("schema", () => {
  test("the resolved document validates and the schema is closed", () => {
    expect(validate(S.schema, doc)).toEqual([]);
    const extra = structuredClone(doc);
    extra.color.dark.layer.invented = "#000000";
    extra.space.made_up = 3;
    expect(validate(S.schema, extra)).toEqual([
      "$.color.dark.layer.invented: not allowed",
      "$.space.made_up: not allowed",
    ]);
    const missing = structuredClone(doc);
    delete missing.color.light.border.control;
    expect(validate(S.schema, missing)).toEqual(["$.color.light.border: missing control"]);
  });
  test("every role exists in both modes, for the house and both variants", () => {
    const keys = (r: any) =>
      JSON.stringify(
        Object.keys(r).map((g) => [g, Array.isArray(r[g]) ? r[g].length : Object.keys(r[g])]),
      );
    for (const set of [doc.color, doc.variants.reader.color, doc.variants.contrast_more.color])
      expect(keys(set.dark)).toBe(keys(set.light));
  });
});

describe("verify", () => {
  test("house, reader and contrast_more raise no errors in either mode", () => {
    for (const [name, list] of Object.entries(R.verify))
      expect({ name, errors: (list as Violation[]).filter((v) => v.severity === "error") }).toEqual(
        { name, errors: [] },
      );
  });
  test("contrast_more lifts every word to 7:1 on every wash and controls to 4.5:1", () => {
    for (const mode of ["dark", "light"] as const) {
      const r = R.contrastMore[mode];
      const v = verify(
        r,
        {
          ...R.params,
          bars: {
            ...R.params.bars,
            text_floor: 7,
            control: 4.5,
            border_strong: 3,
            base_floor: 4.5,
          },
        },
        { context: "house", warn: ["V3"] },
      );
      expect(v.filter((x) => x.severity === "error")).toEqual([]);
      expect(parse(r.border.strong)[3]).toBe(255);
    }
  });
  test("the house glass, pulse and elevation checks run and pass", () => {
    const ids = new Set(R.verify.house.map((v) => v.id));
    for (const id of ["V14", "V15", "V16", "V17"]) expect(ids.has(id)).toBe(false);
    for (const mode of ["dark", "light"] as const) {
      expect(S.house.material.glass.tint[mode].alpha).toBeGreaterThanOrEqual(
        R.params.glass.tint_min[mode],
      );
      for (const tier of ["raised", "floating", "overlay"])
        expect(S.house.elevation[mode][tier].length).toBeLessThanOrEqual(4);
    }
  });
  test("a broken house is reported, not silently accepted", () => {
    const r = structuredClone(R.house.dark);
    r.text.faint = r.layer.overlay;
    r.status.needs_you = r.layer.sidebar;
    r.wash.press = r.wash.hover;
    const ids = verify(r, R.params, { context: "house" }).map((v) => `${v.id} ${v.role}`);
    expect(ids).toContain("V2 text.faint");
    expect(ids).toContain("V1 status.needs_you");
    expect(ids).toContain("V9 wash.press");
  });
});

describe("house terminal", () => {
  test("derive(terminal) reproduces the anchor roles and the exported difference list is current", () => {
    for (const mode of ["dark", "light"] as const) {
      const t = S.house.terminal[mode];
      const derived = derive(
        {
          bg: t.bg,
          fg: t.fg,
          cursor: t.cursor,
          ansi: t.ansi,
          accent: null,
          selection_bg: t.selection_bg,
          dark: mode === "dark",
        },
        R.params,
      );
      const house = R.house[mode];
      expect(rolesDiff(house, derived)).toEqual(doc.results.terminal_diff[mode]);
      for (const path of [
        "layer.ground",
        "layer.scrim",
        "text.text",
        "text.inverse",
        "border.muted",
        "border.base",
        "border.strong",
        "wash.hover",
        "wash.press",
        "wash.current",
        "wash.selected",
        "status.needs_you",
        "status.done",
        "status.bell",
        "selection.bg",
        "match.bg",
        "match.current_bg",
        "match.current_fg",
        "diff.add",
        "diff.remove",
      ]) {
        const at = (r: any) => path.split(".").reduce((o, k) => o[k], r);
        expect({ mode, path, value: at(derived) }).toEqual({ mode, path, value: at(house) });
      }
    }
  });
  test("the catalog's phux and phux-light are the house terminal", () => {
    const [dark, light] = R.catalog.themes;
    expect([dark!.id, light!.id]).toEqual(["phux", "phux-light"]);
    expect(dark!.ansi).toEqual(S.house.terminal.dark.ansi);
    expect(light!.bg).toBe(S.house.color.light.layer.ground);
  });
});

describe("golden vectors", () => {
  test("the vectors are for these params", () => {
    expect(vectors.params_sha256).toBe(paramsSha256(R.params));
    expect(vectors.params).toEqual(doc.derive);
    expect(vectors.solver).toBe("phux-derive/2");
  });
  test("every case round-trips: roles exact, verify lists equal", () => {
    expect(vectors.cases.length).toBe(R.catalog.themes.length + EDGES.length);
    for (const c of vectors.cases) {
      const roles = derive(c.input as Theme, vectors.params);
      expect({ id: c.id, roles }).toEqual({ id: c.id, roles: c.expect });
      const v = verify(roles, vectors.params, {
        context: "derived",
        cursor: c.input.cursor,
        bg: c.input.bg,
      });
      expect(v.map(({ id, role, surface }) => ({ id, role, surface }))).toEqual(
        c.verify.map(({ id, role, surface }: any) => ({ id, role, surface })),
      );
      v.forEach((x, i) => expect(Math.abs(x.value - c.verify[i].value)).toBeLessThan(1e-9));
    }
  });
  test("every case carries cursor, selection_bg and dark explicitly", () => {
    for (const c of vectors.cases)
      for (const k of ["cursor", "selection_bg", "dark", "accent"]) expect(k in c.input).toBe(true);
  });
  test("primitives match", () => {
    const P = vectors.primitives;
    for (const c of P.contrast)
      expect(Math.abs(contrast(rgb(c.a), rgb(c.b), lut) - c.expect)).toBeLessThan(1e-12);
    for (const c of P.mix) expect(hex(mix(rgb(c.a), rgb(c.b), c.t))).toBe(c.expect);
    for (const c of P.over) expect(hex(over(parse(c.top), rgb(c.bottom)))).toBe(c.expect);
    for (const c of P.reach)
      expect(hex(reach(rgb(c.c), rgb(c.toward), c.bar, c.S.map(rgb), lut))).toBe(c.expect);
    for (const c of P.fade)
      expect(hex(fade(rgb(c.c), rgb(c.toward), c.target, c.S.map(rgb), lut))).toBe(c.expect);
    for (const c of P.on_fill) expect(hex(onFill(rgb(c.f), lut))).toBe(c.expect);
  });
  test("the spec's worked examples hold", () => {
    expect(
      Math.abs(contrast(rgb("#777777"), rgb("#FFFFFF"), lut) - 4.478089453577214),
    ).toBeLessThan(1e-12);
    expect(hex(mix(rgb("#0D1017"), rgb("#D8DEE9"), 0.065))).toBe("#1A1D25");
    expect(hex(over(parse("#D8DEE924"), rgb("#15181F")))).toBe("#31343C");
    expect(
      hex(
        reach(
          rgb("#C34043"),
          rgb("#FFFFFF"),
          5.0,
          ["#1F1F28", "#212129", "#25252D", "#28282F"].map(rgb),
          lut,
        ),
      ),
    ).toBe("#D77E80");
    expect(R.params.srgb_lut[1]).toBe("3F33E45677C176F7");
  });
  test("tone_of and plural cases match", () => {
    for (const c of vectors.tone_of) expect(toneOf(c.input)).toBe(c.expect);
    for (const c of vectors.plural)
      expect(plural(S.vocabulary.plurals[c.key], c.count)).toBe(c.expect);
  });
});

describe("solver", () => {
  test("the shipped LUT is the sRGB formula (to one ulp of this runtime's pow)", () => {
    for (let v = 0; v < 256; v++) {
      const c = v / 255,
        f = c <= 0.04045 ? c / 12.92 : ((c + 0.055) / 1.055) ** 2.4;
      expect(Math.abs(lut[v]! - f)).toBeLessThanOrEqual(f * Number.EPSILON);
    }
  });
  test("every catalog theme derives with no errors (audit row 5)", () => {
    for (const t of R.catalog.themes) {
      const errors = verify(derive(t, R.params), R.params, {
        context: "derived",
        cursor: t.cursor,
        bg: t.bg,
      }).filter((v) => v.severity === "error");
      expect({ id: t.id, errors }).toEqual({ id: t.id, errors: [] });
    }
  });
  test("an explicit dark flag decides the mode, luminance only without one", () => {
    const grey: Theme = { bg: "#777777", fg: "#FFFFFF", ansi: Array(16).fill("#808080") };
    expect(derive({ ...grey, dark: true }, R.params).mode).toBe("dark");
    expect(derive({ ...grey, dark: false }, R.params).mode).toBe("light");
    expect(derive(grey, R.params).mode).toBe("light");
  });
  test("tones reach the floor on their own wash, not only on the input tone's", () => {
    for (const c of vectors.cases.filter((x: any) => !x.id.startsWith("edge/")))
      expect(c.verify.filter((v: any) => v.id === "V13")).toEqual([]);
  });
  test("flat forms cover every translucent role on every layer", () => {
    for (const r of [R.house.dark, R.contrastMore.light, R.reader.dark]) {
      for (const l of ["ground", "sidebar", "raised", "overlay"] as const) {
        expect(Object.keys(r.flat[l]).sort()).toEqual([
          "border",
          "diff",
          "layer",
          "match",
          "status",
          "wash",
        ]);
        expect(r.flat[l].wash!.selected).toBe(hex(over(parse(r.wash.selected), rgb(r.layer[l]))));
        expect(Object.keys(r.flat[l].status!).length).toBe(TONES.length * 3);
      }
    }
  });
});

describe("fit", () => {
  test("is deterministic and is what the tokens export", () => {
    const again = fitParams(S, R.house);
    expect(again).toEqual(R.params);
    expect(again).toEqual(doc.derive);
    expect(doc.derive.wash_selected_source).toBe("fg");
  });
  test("translucent alphas round-trip to the authored 8-bit values", () => {
    for (const mode of ["dark", "light"] as const) {
      const a = R.params.alpha[mode];
      expect(Math.round(a.border.strong * 255)).toBe(parse(S.house.color[mode].border.strong)[3]);
      expect(Math.round(a.wash.selected * 255)).toBe(parse(S.house.color[mode].wash.selected)[3]);
    }
  });
});

describe("motion", () => {
  /** Settle time of mass-1 x'' = k(1 - x) - c x', integrated with RK4 at 0.05 ms: Compose's model, independently. */
  function composeSettle(zeta: number, stiffness: number, horizon: number): number {
    const c = 2 * zeta * Math.sqrt(stiffness),
      h = 5e-5;
    let x = 0,
      v = 0,
      last = 0;
    const f = (x: number, v: number) => stiffness * (1 - x) - c * v;
    for (let t = 0; t <= horizon; t += h) {
      if (Math.abs(1 - x) >= 0.001) last = t;
      const k1x = v,
        k1v = f(x, v),
        k2x = v + (h / 2) * k1v,
        k2v = f(x + (h / 2) * k1x, v + (h / 2) * k1v);
      const k3x = v + (h / 2) * k2v,
        k3v = f(x + (h / 2) * k2x, v + (h / 2) * k2v),
        k4x = v + h * k3v,
        k4v = f(x + h * k3x, v + h * k3v);
      x += (h / 6) * (k1x + 2 * k2x + 2 * k3x + k4x);
      v += (h / 6) * (k1v + 2 * k2v + 2 * k3v + k4v);
    }
    return last + h;
  }
  const springs = Object.entries<any>(doc.motion.spring).flatMap(([name, pair]) => [
    [`${name}.enter`, pair.enter],
    [`${name}.exit`, pair.exit],
  ]);
  test.each(springs)(
    "%s: Compose and CSS settle within one 60 Hz frame of the SwiftUI spring",
    (_name, spring) => {
      const settle = springSettle(spring);
      const { zeta, stiffness } = springConstants(spring);
      expect(Math.abs(composeSettle(zeta, stiffness, spring.duration * 20) - settle)).toBeLessThan(
        1 / 60,
      );
      const samples = springSamples(spring);
      expect(samples).toHaveLength(24);
      const lin = (t: number) => {
        const p = Math.min(t / settle, 1) * 23,
          i = Math.min(Math.floor(p), 22);
        return samples[i]! + (samples[i + 1]! - samples[i]!) * (p - i);
      };
      expect(Math.abs(settleTime(lin, settle * 2) - settle)).toBeLessThan(1 / 60);
      expect(Math.abs(springAt(spring, settle) - 1)).toBeLessThan(0.001);
    },
  );
  test("exits never bounce", () => {
    for (const pair of Object.values<any>(doc.motion.spring)) expect(pair.exit.bounce).toBe(0);
  });
});

describe("catalog", () => {
  test("one id namespace with the collision fixes and icon flags", () => {
    const ids = R.catalog.themes.map((t) => t.id);
    expect(new Set(ids).size).toBe(ids.length);
    for (const id of [
      "rose-pine",
      "rose-pine-dawn",
      "gruvbox-material",
      "gruvbox-dark",
      "catppuccin-mocha",
      "windows-98",
      "phux",
      "phux-light",
    ])
      expect(ids).toContain(id);
    expect(R.catalog.themes.find((t) => t.id === "rose-pine")!.dark).toBe(true);
    expect(R.catalog.themes.find((t) => t.id === "rose-pine-dawn")!.dark).toBe(false);
    expect(
      R.catalog.themes
        .filter((t) => t.icon)
        .map((t) => t.id)
        .sort(),
    ).toEqual([
      "catppuccin-latte",
      "catppuccin-mocha",
      "everforest-dark",
      "flexoki-light",
      "gruvbox-material",
      "kanagawa",
      "nord",
      "rose-pine-dawn",
      "tokyo-night",
      "windows-98",
    ]);
    for (const [from, to] of Object.entries({ ...R.catalog.aliases, ...R.catalog.legacy.mobile }))
      expect({ from, found: ids.includes(to as string) }).toEqual({ from, found: true });
  });
  test("every theme mobile bundled before still ships on mobile", () => {
    const mobile = new Set(
      R.catalog.themes.filter((t) => t.ships.includes("mobile")).map((t) => t.id),
    );
    for (const id of [
      "catppuccin",
      "catppuccin-latte",
      "everforest",
      "flexoki-light",
      "gruvbox",
      "kanagawa",
      "nord",
      "rose-pine",
      "tokyo-night",
      "windows-98",
    ])
      expect(mobile.has(R.catalog.legacy.mobile[id] ?? R.catalog.aliases[id] ?? id)).toBe(true);
  });
});

describe("generated exports", () => {
  test("committed bytes equal a fresh render (snapshot)", async () => {
    for (const e of committed(tokenExports(R, S)))
      expect({
        path: e.path,
        same: e.bytes === (await readFile(resolve(ROOT, e.path), "utf8")),
      }).toEqual({ path: e.path, same: true });
  });
  test("code exports carry the @generated header and end in one newline", () => {
    for (const e of tokenExports(R, S)) {
      if (/\.(rs|swift|kt|css|xml)$/.test(e.path))
        expect(e.bytes.slice(0, 200)).toContain("@generated by phux tools design-gen");
      expect(e.bytes.endsWith("\n") && !e.bytes.endsWith("\n\n")).toBe(true);
    }
  });
  test("names are lowerCamel per segment with keyword escaping", () => {
    expect(lowerCamel("needs_you")).toBe("needsYou");
    expect(lowerCamel("row_h_touch")).toBe("rowHTouch");
    const swift = tokenExports(R, S).find((e) => e.path === "swift/PhuxTokens.swift")!.bytes;
    expect(swift).toContain("public let `default`: String");
    expect(swift).toContain("public let `where`: [String]");
    const rust = tokenExports(R, S).find((e) => e.path === "rust/phux_tokens.rs")!.bytes;
    expect(rust).toContain("pub r#match: MatchRoles,");
    const kotlin = tokenExports(R, S).find((e) => e.path === "kotlin/PhuxTokens.kt")!.bytes;
    expect(kotlin).toContain("val needsYouWash: PhuxArgb,");
  });
  test("platform-valued tokens resolve per platform", () => {
    const ex = Object.fromEntries(tokenExports(R, S).map((e) => [e.path, e.bytes]));
    expect(ex["kotlin/PhuxTokens.kt"]).toContain("touchMin = 48.0f,");
    expect(ex["swift/PhuxTokens.swift"]).toContain("touchMin: 44.0");
    expect(ex["css/phux-tokens.css"]).toContain("--phux-type-body-size: 0.875rem;");
    expect(ex["rust/phux_tokens.rs"]).toContain("touch_min: 44.0,");
  });
  test("the CSS covers every mode and accessibility media query", () => {
    const css = tokenExports(R, S).find((e) => e.path === "css/phux-tokens.css")!.bytes;
    for (const needle of [
      "color-scheme: light dark",
      "@media (prefers-color-scheme: light)",
      '[data-theme="dark"], .dark',
      '[data-theme="light"], .light',
      '[data-design="reader"]',
      "@media (prefers-contrast: more)",
      "@media (forced-colors: active)",
      "@media (prefers-reduced-motion: reduce)",
      "@media (prefers-reduced-transparency: reduce)",
      "@theme inline",
      "--color-phux-status-needs-you: var(--phux-status-needs-you);",
    ])
      expect(css).toContain(needle);
  });
  test("plurals and words are exported for every platform", () => {
    const ex = Object.fromEntries(tokenExports(R, S).map((e) => [e.path, e.bytes]));
    expect(ex["android/values/phux_plurals.xml"]).toContain(
      '<item quantity="other">%d need you</item>',
    );
    const xc = JSON.parse(ex["swift/Localizable.xcstrings"]!);
    expect(
      xc.strings["count.needs_you"].localizations.en.variations.plural.one.stringUnit.value,
    ).toBe("%lld needs you");
    expect(plural(S.vocabulary.plurals["count.needs_you"], 1)).toBe("1 needs you");
    expect(plural(S.vocabulary.plurals["count.needs_you"], 0)).toBe("0 need you");
    expect(plural(S.vocabulary.plurals["count.needs_you"], 2)).toBe("2 need you");
  });
});

describe("status", () => {
  test("tone_of follows the pane facts", () => {
    expect(toneOf({ agent: "Blocked" })).toBe("needs_you");
    expect(toneOf({ agent: "Done", exit: 1 })).toBe("failed");
    expect(toneOf({ agent: "Working", link: "lost" })).toBe("lost");
    expect(toneOf({ exit: 0 })).toBe("ended");
    expect(toneOf({ agent: "Done", seen: true })).toBe("done_seen");
  });
  test("done (seen) and idle share a mark but not a word", () => {
    const by = Object.fromEntries(doc.status.map((m: any) => [m.state, m]));
    expect(by.done_seen.glyph).toBe(by.idle.glyph);
    expect(doc.vocabulary[by.done_seen.word]).not.toBe(doc.vocabulary[by.idle.word]);
    expect(
      doc.status.filter((m: any) => m.rank > 0).every((m: any) => /^[ -~]$/.test(m.ascii)),
    ).toBe(true);
  });
});
