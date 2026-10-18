import { readFileSync, statSync, writeFileSync } from "node:fs";
import type { Command } from "../lib/command.ts";
import { FontError, emit, parseBdf } from "./bitmap.ts";

export const commands: Record<string, Command> = {
  "gen-bitmap-font": {
    summary: "Regenerate the Spleen glyph table: gen-bitmap-font <in.bdf> <out.rs>.",
    run(args) {
      const [source, destination] = args;
      if (args.length !== 2 || source === undefined || destination === undefined) {
        console.error("usage: bun tools/main.ts gen-bitmap-font <in.bdf> <out.rs>");
        return 2;
      }
      // Undecodable bytes become U+FFFD rather than failing the read.
      const text = new TextDecoder("utf-8").decode(readFileSync(source));
      let glyphs;
      try {
        glyphs = parseBdf(text);
      } catch (error) {
        if (!(error instanceof FontError)) throw error;
        console.error(error.message);
        return 1;
      }
      writeFileSync(destination, emit(glyphs));
      console.log(`${destination}: ${glyphs.size} glyphs, ${statSync(destination).size} bytes`);
      return 0;
    },
  },
};
