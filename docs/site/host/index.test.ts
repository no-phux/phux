import { describe, expect, test } from "bun:test";
import handler from "./index";

const PAGE = `<!doctype html><html><head><title>phux</title></head>
<body><main><h1>phux</h1><p>you and your agents share the same terminals</p></main></body></html>`;

function assetsOf(body: string, contentType = "text/html") {
  return {
    ASSETS: {
      fetch: async () =>
        new Response(body, { headers: { "content-type": contentType } }),
    },
  };
}

describe("markdown negotiation", () => {
  test("Accept: text/markdown gets a markdown response with token count", async () => {
    const request = new Request("https://phux.sh/", {
      headers: { accept: "text/markdown" },
    });
    const response = await handler.fetch(request, assetsOf(PAGE));

    expect(response.status).toBe(200);
    expect(response.headers.get("content-type")).toBe(
      "text/markdown; charset=utf-8",
    );
    expect(response.headers.get("vary")).toContain("Accept");
    expect(Number(response.headers.get("x-markdown-tokens"))).toBeGreaterThan(0);
    const body = await response.text();
    expect(body).toContain("# phux");
    expect(body).toContain("you and your agents share the same terminals");
  });

  test("browser accepts keep HTML and declare Vary: Accept", async () => {
    const request = new Request("https://phux.sh/", {
      headers: { accept: "text/html,application/xhtml+xml" },
    });
    const response = await handler.fetch(request, assetsOf(PAGE));

    expect(response.headers.get("content-type")).toBe("text/html");
    expect(response.headers.get("vary")).toContain("Accept");
    expect(await response.text()).toContain("<main>");
  });

  test("no accept header keeps HTML as the default", async () => {
    const response = await handler.fetch(new Request("https://phux.sh/"), assetsOf(PAGE));
    expect(response.headers.get("content-type")).toBe("text/html");
  });

  test("non-HTML assets pass through untouched", async () => {
    const request = new Request("https://phux.sh/install.sh", {
      headers: { accept: "text/markdown" },
    });
    const response = await handler.fetch(
      request,
      assetsOf("#!/bin/sh\necho hi\n", "application/x-sh"),
    );
    expect(response.headers.get("content-type")).toBe("application/x-sh");
  });

  test("host redirects still fire before negotiation", async () => {
    const request = new Request("https://phux.sh/docs", {
      headers: { accept: "text/markdown" },
    });
    const response = await handler.fetch(request, assetsOf(PAGE));
    expect(response.status).toBe(301);
    expect(response.headers.get("location")).toBe("https://docs.phux.sh/docs");
  });
});
