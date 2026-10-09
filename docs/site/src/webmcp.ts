// WebMCP tool registration for the public site.
// The current report takes one ModelContextTool dictionary:
// https://webmachinelearning.github.io/webmcp

export type SiteFacts = {
  productName: string;
  cliInstall: string;
  cockpitInstall: string;
  docsUrl: string;
  releaseTag: string;
  releaseUrl: string;
};

export type ModelContextTool = {
  name: string;
  description: string;
  inputSchema: {
    type: "object";
    properties: Record<string, unknown>;
    required?: string[];
  };
  execute: (input: Record<string, unknown>) => Promise<unknown>;
  annotations: { readOnlyHint: true };
};

export type ModelContextLike = {
  registerTool: (tool: ModelContextTool) => unknown;
};

type ModelContextScope = {
  document?: { modelContext?: ModelContextLike | null };
  navigator?: { modelContext?: ModelContextLike | null };
};

const DOC_PATHS: Record<string, string> = {
  overview: "/overview",
  quickstart: "/quickstart/install",
  concepts: "/concepts",
  consumers: "/consumers",
  agents: "/consumers/agents",
  "remote-access": "/remote-access",
  wire: "/wire/proto",
  architecture: "/architecture",
  decisions: "/decisions",
};

function installCommand(facts: SiteFacts, target: unknown): string {
  if (target === "cockpit") return facts.cockpitInstall;
  if (target === "skills") return "npx skills add no-phux/skills";
  return facts.cliInstall;
}

function docPath(topic: unknown): string {
  const slug = String(topic ?? "")
    .toLowerCase()
    .replace(/[^a-z0-9/-]+/g, "");
  return DOC_PATHS[slug] ?? `/overview?search=${encodeURIComponent(slug)}`;
}

export function siteTools(facts: SiteFacts): ModelContextTool[] {
  return [
    {
      name: "get_install_command",
      description: `Get the installer for the ${facts.productName} CLI, Cockpit, or agent skills.`,
      inputSchema: {
        type: "object",
        properties: {
          target: {
            type: "string",
            enum: ["cli", "cockpit", "skills"],
            description: `Which installer to return. Defaults to the ${facts.productName} CLI.`,
          },
        },
      },
      annotations: { readOnlyHint: true },
      execute: async (input) => ({ command: installCommand(facts, input.target) }),
    },
    {
      name: "open_docs",
      description: `Get the canonical documentation URL for a ${facts.productName} topic.`,
      inputSchema: {
        type: "object",
        properties: {
          topic: {
            type: "string",
            description: 'Docs topic, e.g. "quickstart", "wire", "consumers".',
          },
        },
        required: ["topic"],
      },
      annotations: { readOnlyHint: true },
      execute: async (input) => ({ url: `${facts.docsUrl}${docPath(input.topic)}` }),
    },
    {
      name: "get_latest_release",
      description: `Get the latest ${facts.productName} release tag and URL.`,
      inputSchema: { type: "object", properties: {} },
      annotations: { readOnlyHint: true },
      execute: async () => ({ tag: facts.releaseTag, url: facts.releaseUrl }),
    },
  ];
}

// Prefer the page-scoped context. navigator.modelContext remains the older alias.
export function siteModelContext(scope: ModelContextScope): ModelContextLike | null {
  const candidates = [scope.document?.modelContext, scope.navigator?.modelContext];
  for (const candidate of candidates) {
    if (candidate && typeof candidate.registerTool === "function") return candidate;
  }
  return null;
}

export async function registerSiteTools(
  context: ModelContextLike,
  facts: SiteFacts,
): Promise<void> {
  for (const tool of siteTools(facts)) {
    await context.registerTool(tool);
  }
}
