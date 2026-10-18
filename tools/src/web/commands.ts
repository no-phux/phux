import type { Command } from "../lib/command.ts";
import { webBrowser } from "./browser.ts";

export const commands: Record<string, Command> = {
  "web-browser": {
    summary: "Real headless Chrome tests against an isolated demo server (nix develop .#browser).",
    run: () => webBrowser(),
  },
};
