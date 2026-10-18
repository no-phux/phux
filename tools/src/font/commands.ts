import { readFileSync, statSync, writeFileSync } from "node:fs";
import { UsageError, type Command } from "../lib/command.ts";
import { emit, parseBdf } from "./bitmap.ts";

export const commands: Record<string, Command> = {
  "gen-bitmap-font": {
    summary: "Regenerate the Spleen glyph table from its BDF face.",
    usage: "IN.bdf OUT.rs",
    run(args) {
      const [source, destination] = args;
      if (args.length !== 2 || source === undefined || destination === undefined) {
        throw new UsageError("expected IN.bdf and OUT.rs");
      }
      // Undecodable bytes become U+FFFD rather than failing the read.
      const glyphs = parseBdf(new TextDecoder("utf-8").decode(readFileSync(source)));
      writeFileSync(destination, emit(glyphs));
      console.log(`${destination}: ${glyphs.size} glyphs, ${statSync(destination).size} bytes`);
      return 0;
    },
  },
};
