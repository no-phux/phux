import { Type } from "@earendil-works/pi-ai";
import type { AgentToolResult, ExtensionAPI } from "@earendil-works/pi-coding-agent";
import { createDocumentTools, type PhuxToolDefinition, type ToolResult } from "@phux/integration-runtime/tools";

import type { PhuxCli } from "./adapter.js";
import type { ResourceEntry } from "./schemas.js";
import { boundedResult, callText, execution, renderSummary, toolResult, type PhuxToolDetails } from "./tool-support.js";

const STRICT = { additionalProperties: false } as const;
const LOCAL_TIMEOUT = Type.Optional(Type.Integer({ minimum: 1, maximum: 86_405_000, description: "Local subprocess deadline in milliseconds; defaults to 10s for short calls or operation timeout + 5s" }));
const DOCUMENT = Type.String({ minLength: 2, maxLength: 512, pattern: "^(?:[^/\\s]+/)?@[0-9]+$", description: "Exact @N or host/@N document id from phux_documents; not an alias or the parent pane" });

export const PhuxListParams = Type.Object({ local_timeout_ms: LOCAL_TIMEOUT }, STRICT);
export const PhuxDocumentsParams = Type.Object({ local_timeout_ms: LOCAL_TIMEOUT }, STRICT);
export const PhuxTreeSnapshotParams = Type.Object({ target: DOCUMENT, local_timeout_ms: LOCAL_TIMEOUT }, STRICT);
export const PhuxTreeWaitParams = Type.Object({
  target: DOCUMENT,
  until: Type.String({ minLength: 1, maxLength: 4096, description: "Plain substring of the tree's JSON text" }),
  timeout_seconds: Type.Optional(Type.Integer({ minimum: 1, maximum: 3600, description: "Wait deadline in seconds; defaults to 30" })),
  local_timeout_ms: LOCAL_TIMEOUT,
}, STRICT);
export const PhuxSurfaceSnapshotParams = Type.Object({ target: DOCUMENT, local_timeout_ms: LOCAL_TIMEOUT }, STRICT);

/** `phux ls --json` with every resource kind beside the session summary. */
export function registerPhuxListTool(pi: ExtensionAPI, cli: PhuxCli): void {
  pi.registerTool({
    name: "phux_list",
    label: "phux list",
    description: "List phux sessions and every addressable resource: terminals, agent sessions, accessibility trees, pixel surfaces, typed documents, and newer kinds marked opaque, each with kind, parent and name. Output is compact and bounded; this never changes phux focus.",
    promptSnippet: "List shared phux sessions and resources of every kind",
    parameters: PhuxListParams,
    async execute(_id, params, signal) {
      const result = await cli.ls(execution(params, signal));
      const sessions = result.sessions.map((session) =>
        `${session.name}\twindows=${String(session.windows)}\tattached=${String(session.attached)}`);
      const lines = [...sessions, ...result.resources.map(resourceLine)];
      const counts = `sessions=${String(result.sessions.length)} resources=${String(result.resources.length)}`;
      const output = boundedResult(counts, lines.join("\n") || "No phux sessions.");
      return toolResult(output.text, {
        operation: "list",
        summary: `${String(result.sessions.length)} session(s), ${String(result.resources.length)} resource(s)`,
        count: result.sessions.length,
        modelOutputTruncated: output.truncated,
      });
    },
    renderCall: (_args, theme) => callText(theme, "list sessions and resources"),
    renderResult: renderSummary,
  });
}

/** The shared runtime's document reads, so Pi and MCP agents see the same trees and surfaces. */
export function registerPhuxDocumentTools(pi: ExtensionAPI, cli: PhuxCli): void {
  const shared = new Map(createDocumentTools({ cli }).map((tool) => [tool.name, tool]));
  const definition = (name: string): PhuxToolDefinition<any> => shared.get(name)!;
  pi.registerTool({
    name: "phux_documents",
    label: "phux documents",
    description: definition("phux_documents").description,
    promptSnippet: "List accessibility trees, pixel surfaces and typed documents",
    parameters: PhuxDocumentsParams,
    execute: (id, params, signal) => runShared(definition("phux_documents"), id, params, signal),
    renderCall: (_args, theme) => callText(theme, "list documents"),
    renderResult: renderSummary,
  });
  pi.registerTool({
    name: "phux_tree_snapshot",
    label: "phux tree snapshot",
    description: definition("phux_tree_snapshot").description,
    promptSnippet: "Read an application's accessibility tree",
    parameters: PhuxTreeSnapshotParams,
    execute: (id, params, signal) => runShared(definition("phux_tree_snapshot"), id, params, signal),
    renderCall: (args, theme) => callText(theme, `tree snapshot ${args.target}`),
    renderResult: renderSummary,
  });
  pi.registerTool({
    name: "phux_tree_wait",
    label: "phux tree wait",
    description: definition("phux_tree_wait").description,
    promptSnippet: "Wait for text in an accessibility tree",
    parameters: PhuxTreeWaitParams,
    execute: (id, params, signal) => runShared(definition("phux_tree_wait"), id, params, signal),
    renderCall: (args, theme) => callText(theme, `tree wait ${args.target}`),
    renderResult: renderSummary,
  });
  pi.registerTool({
    name: "phux_surface_snapshot",
    label: "phux surface snapshot",
    description: definition("phux_surface_snapshot").description,
    promptSnippet: "See a mirrored window, page or display as an image",
    parameters: PhuxSurfaceSnapshotParams,
    execute: (id, params, signal) => runShared(definition("phux_surface_snapshot"), id, params, signal),
    renderCall: (args, theme) => callText(theme, `surface snapshot ${args.target}`),
    renderResult: renderSummary,
  });
}

async function runShared(
  definition: PhuxToolDefinition<any>,
  id: string,
  params: unknown,
  signal: AbortSignal | undefined,
): Promise<AgentToolResult<PhuxToolDetails>> {
  signal?.throwIfAborted();
  const context = { sessionID: "pi", agent: "pi", messageID: id, id, ...(signal === undefined ? {} : { signal }) };
  return piResult(await definition.execute(params, context));
}

function piResult(result: ToolResult): AgentToolResult<PhuxToolDetails> {
  const { metadata } = result;
  const images = (result.images ?? []).map((image) => ({ type: "image" as const, ...image }));
  return {
    content: [{ type: "text", text: result.content }, ...images],
    details: {
      operation: metadata.operation as PhuxToolDetails["operation"],
      summary: result.content.split("\n", 1)[0] ?? metadata.operation,
      ...(metadata.target === undefined ? {} : { target: metadata.target }),
      modelOutputTruncated: metadata.modelOutputTruncated === true,
      result: metadata,
    },
  };
}

function resourceLine(resource: ResourceEntry): string {
  const fields = [resource.id, `kind=${resource.kind}`, `parent=${resource.parent ?? "-"}`];
  if (resource.name !== null) fields.push(`name=${JSON.stringify(resource.name)}`);
  if ("opaque" in resource) return [...fields, "opaque"].join("\t");
  fields.push(`lifecycle=${resource.lifecycle}`);
  if (resource.producer !== undefined) fields.push(`producer=${resource.producer}`);
  if (resource.control !== undefined) fields.push(`control=${resource.control}`);
  return fields.join("\t");
}
