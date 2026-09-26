import { createPhuxTools, resolveTarget, type PhuxToolDefinition, type PhuxToolRuntime, type ToolContext } from "./tools-core.js";
import { isWriteTool, parentWriteError, samePane } from "./parent.js";

const WRITE_NOTE = " Refuses the pane this OpenCode process is running in. Create a sibling with phux_create and target that.";

/** The six phux tools, with writes into OpenCode's own pane refused before any CLI call. */
export function createGuardedTools(runtime: PhuxToolRuntime, parent: string | null): Record<string, PhuxToolDefinition<any>> {
  const tools = createPhuxTools(runtime);
  if (parent === null) return tools;
  return Object.fromEntries(
    Object.entries(tools).map(([name, tool]) => [name, isWriteTool(name) ? guardWrite(tool, runtime, parent) : tool]),
  );
}

function guardWrite(
  tool: PhuxToolDefinition<any>,
  runtime: PhuxToolRuntime,
  parent: string,
): PhuxToolDefinition<any> {
  return {
    ...tool,
    description: `${tool.description}${WRITE_NOTE}`,
    async execute(input, context: ToolContext) {
      const explicit = readTarget(input);
      const target = resolveTarget(explicit, runtime);
      if (samePane(target, parent)) throw parentWriteError(tool.name, parent);
      return tool.execute(input, context);
    },
  };
}

function readTarget(input: unknown): string | undefined {
  if (input === null || typeof input !== "object") return undefined;
  const target = (input as { readonly target?: unknown }).target;
  return typeof target === "string" ? target : undefined;
}
