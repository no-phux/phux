import { defineConfig } from "tsdown";

export default defineConfig({
  entry: ["src/index.ts"],
  format: "esm",
  platform: "node",
  fixedExtension: false,
  outExtensions: () => ({ js: ".js", dts: ".d.ts" }),
  dts: {
    tsconfig: "../tsconfig.opencode-dts.json",
  },
  clean: true,
  deps: {
    neverBundle: ["@opencode-ai/plugin"],
  },
});
