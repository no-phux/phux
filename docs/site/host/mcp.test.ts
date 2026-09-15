import { describe, expect, test } from "bun:test";
import { handleMcpRequest } from "./mcp";

const SEARCH_INDEX = {
  pages: [
    {
      title: "Remote access",
      url: "/remote-access",
      description: "Attach to a phux server over the network.",
      text: "pair a client with phux pair then attach remotely",
    },
    {
      title: "Install",
      url: "/quickstart/install",
      description: "Install phux.",
      text: "curl installer homebrew nix",
    },
  ],
  release: {
    phux: { tag: "v0.37.0", url: "https://example.test/v0.37.0", publishedAt: "2026-09-13" },
    cockpit: { tag: "cockpit-v0.24.0", url: "https://example.test/c0.24.0", publishedAt: "2026-09-13" },
  },
};

function envOf() {
  return {
    ASSETS: {
      fetch: async (input: Request) => {
        const url = new URL(input.url);
        if (url.pathname === "/api/mcp-search.json") {
          return new Response(JSON.stringify(SEARCH_INDEX), {
            headers: { "content-type": "application/json" },
          });
        }
        if (url.pathname === "/remote-access") {
          return new Response("<main><h1>Remote access</h1><p>pair then attach</p></main>", {
            headers: { "content-type": "text/html" },
          });
        }
        return new Response("not found", { status: 404 });
      },
    },
  };
}

async function rpc(method: string, params: unknown, id: number | string = 1) {
  const response = (await handleMcpRequest(
    new Request("https://phux.sh/mcp", {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify({ jsonrpc: "2.0", id, method, params }),
    }),
    envOf(),
  ))!;
  return { response, body: await response.json() };
}

describe("MCP endpoint routing", () => {
  test("ignores non-/mcp paths", async () => {
    const response = await handleMcpRequest(
      new Request("https://phux.sh/"),
      envOf(),
    );
    expect(response).toBeNull();
  });

  test("answers CORS preflight", async () => {
    const response = (await handleMcpRequest(
      new Request("https://phux.sh/mcp", { method: "OPTIONS" }),
      envOf(),
    ))!;
    expect(response.status).toBe(204);
    expect(response.headers.get("access-control-allow-origin")).toBe("*");
  });

  test("rejects GET (no SSE stream)", async () => {
    const response = (await handleMcpRequest(
      new Request("https://phux.sh/mcp"),
      envOf(),
    ))!;
    expect(response.status).toBe(405);
  });

  test("rejects malformed JSON", async () => {
    const response = (await handleMcpRequest(
      new Request("https://phux.sh/mcp", {
        method: "POST",
        body: "{not json",
      }),
      envOf(),
    ))!;
    expect(response.status).toBe(400);
  });
});

describe("MCP JSON-RPC", () => {
  test("initialize echoes a supported requested protocol version", async () => {
    const { body, response } = await rpc("initialize", {
      protocolVersion: "2025-03-26",
      capabilities: {},
      clientInfo: { name: "test", version: "0" },
    });
    expect(body.result.protocolVersion).toBe("2025-03-26");
    expect(response.headers.get("mcp-protocol-version")).toBe("2025-03-26");
    expect(body.result.serverInfo.name).toBe("phux-site");
    expect(body.result.capabilities.tools).toBeDefined();
  });

  test("initialize falls back to the default for unknown requested versions", async () => {
    // "2025-06-15" was never a released spec date; clients hard-reject it.
    const { body, response } = await rpc("initialize", {
      protocolVersion: "2025-06-15",
      capabilities: {},
      clientInfo: { name: "test", version: "0" },
    });
    expect(body.result.protocolVersion).toBe("2025-03-26");
    expect(response.headers.get("mcp-protocol-version")).toBe("2025-03-26");
  });

  test("initialize without a requested version serves the default", async () => {
    const { body } = await rpc("initialize", {});
    expect(body.result.protocolVersion).toBe("2025-03-26");
  });

  test("notifications get 202 with no body", async () => {
    const response = (await handleMcpRequest(
      new Request("https://phux.sh/mcp", {
        method: "POST",
        body: JSON.stringify({ jsonrpc: "2.0", method: "notifications/initialized" }),
      }),
      envOf(),
    ))!;
    expect(response.status).toBe(202);
  });

  test("unknown method is a JSON-RPC error", async () => {
    const { body } = await rpc("bogus/method", {});
    expect(body.error.code).toBe(-32601);
  });

  test("tools/list advertises the four tools", async () => {
    const { body } = await rpc("tools/list", {});
    const names = body.result.tools.map((tool: { name: string }) => tool.name);
    expect(names).toEqual([
      "search_docs",
      "get_page",
      "get_install_command",
      "latest_release",
    ]);
  });

  test("search_docs ranks title matches", async () => {
    const { body } = await rpc("tools/call", {
      name: "search_docs",
      arguments: { query: "remote pair attach" },
    });
    expect(body.result.isError).toBeUndefined();
    expect(body.result.content[0].text).toContain("Remote access");
    expect(body.result.content[0].text).toContain("/remote-access");
  });

  test("get_page returns markdown without chrome", async () => {
    const { body } = await rpc("tools/call", {
      name: "get_page",
      arguments: { path: "/remote-access" },
    });
    expect(body.result.content[0].text).toContain("# Remote access");
    expect(body.result.content[0].text).not.toContain("<main>");
  });

  test("get_page refuses foreign hosts", async () => {
    const { body } = await rpc("tools/call", {
      name: "get_page",
      arguments: { path: "https://evil.example/x" },
    });
    expect(body.result.content[0].text).toContain("Only phux.sh");
  });

  test("get_install_command and latest_release", async () => {
    const install = await rpc("tools/call", {
      name: "get_install_command",
      arguments: { target: "cockpit" },
    });
    expect(install.body.result.content[0].text).toBe(
      "curl -fsSL https://phux.sh/install-cockpit | sh",
    );
    const release = await rpc("tools/call", { name: "latest_release", arguments: {} });
    expect(release.body.result.content[0].text).toContain("v0.37.0");
    expect(release.body.result.content[0].text).toContain("cockpit-v0.24.0");
  });
});
