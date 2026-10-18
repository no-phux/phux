import solid from "@opentui/solid/bun-plugin";

for (const entry of ["index", "tui"]) {
  const result = await Bun.build({
    entrypoints: [`src/${entry}.${entry === "tui" ? "tsx" : "ts"}`],
    outdir: ".",
    target: "bun",
    format: "esm",
    external: ["@opencode/plugin", "@opencode/plugin/*", "solid-js", "solid-js/*", "@opentui/core", "@opentui/solid"],
    plugins: entry === "tui" ? [solid] : [],
    minify: entry === "tui",
  });
  if (!result.success) throw new AggregateError(result.logs, `phux ${entry} build failed`);
}
