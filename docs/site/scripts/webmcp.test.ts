import { describe, expect, test } from "bun:test";
import { registerSiteTools, siteModelContext, type ModelContextTool } from "../src/webmcp.ts";

const facts = {
  productName: "phux",
  cliInstall: "curl -fsSL https://phux.sh/install | sh",
  desktopInstall: "curl -fsSL https://phux.sh/install-desktop | sh",
  cockpitInstall: "curl -fsSL https://phux.sh/install-cockpit | sh",
  docsUrl: "https://docs.phux.sh",
  releaseTag: "v0.9.0",
  releaseUrl: "https://github.com/no-phux/phux/releases/tag/v0.9.0",
};

function recordingContext() {
  const tools: ModelContextTool[] = [];
  return {
    tools,
    registerTool(tool: ModelContextTool) {
      tools.push(tool);
      return Promise.resolve();
    },
  };
}

describe("site WebMCP registration", () => {
  test("registers one ModelContextTool dictionary per tool", async () => {
    const context = recordingContext();
    await registerSiteTools(context, facts);
    expect(context.tools.map((tool) => tool.name)).toEqual([
      "get_install_command",
      "open_docs",
      "get_latest_release",
    ]);
    for (const tool of context.tools) {
      expect(typeof tool).toBe("object");
      expect(typeof tool.name).toBe("string");
      expect(tool.description.length).toBeGreaterThan(0);
      expect(tool.inputSchema.type).toBe("object");
      expect(tool.annotations.readOnlyHint).toBe(true);
      expect(tool.execute({})).toBeInstanceOf(Promise);
    }
  });

  test("install, docs, and release tools answer from the page facts", async () => {
    const context = recordingContext();
    await registerSiteTools(context, facts);
    const [install, docs, release] = context.tools;
    expect(await install.execute({})).toEqual({ command: facts.cliInstall });
    expect(await install.execute({ target: "cockpit" })).toEqual({
      command: facts.cockpitInstall,
    });
    expect(await install.execute({ target: "skills" })).toEqual({
      command: "npx skills add no-phux/skills",
    });
    expect(await docs.execute({ topic: "Wire" })).toEqual({
      url: "https://docs.phux.sh/wire/proto",
    });
    expect(await release.execute({})).toEqual({
      tag: facts.releaseTag,
      url: facts.releaseUrl,
    });
  });

  test("prefers document.modelContext and ignores a context that cannot register", () => {
    const page = { registerTool: () => undefined };
    const legacy = { registerTool: () => undefined };
    expect(
      siteModelContext({
        document: { modelContext: page },
        navigator: { modelContext: legacy },
      }),
    ).toBe(page);
    expect(
      siteModelContext({
        document: { modelContext: null },
        navigator: { modelContext: legacy },
      }),
    ).toBe(legacy);
    expect(siteModelContext({ document: {}, navigator: {} })).toBeNull();
  });
});
