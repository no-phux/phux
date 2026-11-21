/**
 * tokens/ios/{AccentColor,LaunchBackground}.colorset/Contents.json: asset-catalog colours with
 * Any, Dark and High Contrast (Any and Dark) appearances, in Xcode's own layout.
 */
import { parse } from "../derive.ts";
import type { Resolved } from "../tokens.ts";

function color(c: string) {
  const [r, g, b] = parse(c),
    h = (v: number) => `0x${v.toString(16).toUpperCase().padStart(2, "0")}`;
  return {
    "color-space": "srgb",
    components: { alpha: "1.000", blue: h(b), green: h(g), red: h(r) },
  };
}

function colorset(light: string, dark: string, lightHigh: string, darkHigh: string): string {
  const dk = { appearance: "luminosity", value: "dark" },
    hi = { appearance: "contrast", value: "high" };
  const doc = {
    colors: [
      { color: color(light), idiom: "universal" },
      { appearances: [dk], color: color(dark), idiom: "universal" },
      { appearances: [hi], color: color(lightHigh), idiom: "universal" },
      { appearances: [dk, hi], color: color(darkHigh), idiom: "universal" },
    ],
    info: { author: "xcode", version: 1 },
  };
  return JSON.stringify(doc, null, 2).replaceAll('": ', '" : ') + "\n";
}

export function emitColorsets(R: Resolved): { path: string; bytes: string }[] {
  const h = R.house,
    cm = R.contrastMore;
  return [
    // The tint colour reads as text and icons, so high contrast takes the solved accent_text.
    {
      path: "ios/AccentColor.colorset/Contents.json",
      bytes: colorset(
        h.light.accent.accent,
        h.dark.accent.accent,
        cm.light.accent.accent_text,
        cm.dark.accent.accent_text,
      ),
    },
    {
      path: "ios/LaunchBackground.colorset/Contents.json",
      bytes: colorset(
        h.light.layer.ground,
        h.dark.layer.ground,
        cm.light.layer.ground,
        cm.dark.layer.ground,
      ),
    },
  ];
}
