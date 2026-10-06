import { describe, expect, test } from "bun:test";
import { readFileSync } from "node:fs";
import { runInNewContext } from "node:vm";

// Exercise the actual browser script, not a second copy of its parser.
const page = readFileSync(new URL("../src/pages/connect.astro", import.meta.url), "utf8");
const script = page.match(/<script is:inline>([\s\S]*?)<\/script>/)![1]!;
const token = "a".repeat(64);
const fp = "b".repeat(64);

function render(query: string) {
  const elements = new Map<string, any>();
  for (const id of ["lede", "bad", "details", "install", "name-row", "fp-row", "server", "name", "fp", "open", "recover", "pairing-link"]) {
    elements.set(id, {
      hidden: true, textContent: "", value: "", attributes: {} as Record<string, string>,
      setAttribute(key: string, value: string) { this.attributes[key] = value; },
      addEventListener(_: string, listener: (event: any) => void) { this.submit = listener; },
    });
  }
  const location = { search: query, pathname: "/connect", href: "" };
  const history: string[] = [];
  runInNewContext(script, {
    URL, URLSearchParams,
    document: { getElementById: (id: string) => elements.get(id) },
    window: { location, history: { replaceState: (_: unknown, __: string, path: string) => history.push(path) } },
  });
  return { elements, location, history };
}

function query(fields: Record<string, string>) {
  return `?${new URLSearchParams({ token, fp, ...fields })}`;
}

describe("pairing page", () => {
  const supported: Record<string, string>[] = [
    { url: "wss://mac.example:8787" },
    { url: "ws://127.0.0.1:8787" },
    { url: "wss://mac.example:8787", quic: "quic://mac.example:8788" },
    { quic: "quic://relay.example:4433", sni: "studio" },
    { quic: "quic://[2001:db8::1]:4433", sni: "studio" },
  ];
  test.each(supported)("opens supported pairing payload %j without dropping any fields", (fields) => {
    const search = query(fields);
    const { elements, history } = render(search);
    expect(elements.get("details").hidden).toBe(false);
    expect(elements.get("bad").hidden).toBe(true);
    expect(elements.get("server").textContent).toBe(fields.quic ?? fields.url);
    const handoff = new URL(elements.get("open").attributes.href);
    expect(handoff.searchParams.toString()).toBe(new URLSearchParams(search).toString());
    expect(history).toEqual(["/connect"]);
  });

  test.each([
    "", `?token=${token}`, "?url=wss://mac.example:8787",
    query({ url: "https://mac.example" }), query({ quic: "quic://" }),
    query({ quic: "quic://user:password@mac.example:8788" }),
    query({ quic: "quic://mac.example:8788" }) + "&quic=quic://evil.example:8788",
    query({ url: "wss://mac.example:8787" }) + "&token=other",
    query({ quic: "quic://relay.example", sni: "studio" }),
    query({ quic: "quic://relay.example:0", sni: "studio" }),
    query({ quic: "quic://relay.example:4433/path", sni: "studio" }),
    query({ quic: "quic://relay.example:4433" }),
    query({ quic: "quic://relay.example:4433", sni: "studio" }) + "&sni=other",
    query({ quic: "quic://relay.example:4433", sni: "127.0.0.1" }),
    query({ quic: "quic://relay.example:4433", sni: "bad/route" }),
    query({ quic: "quic://relay.example:4433", sni: "   " }),
    query({ quic: "quic://relay.example:4433", sni: "-studio" }),
    query({ url: "wss://mac.example:8787", sni: "studio" }),
    query({ url: "https://mac.example", quic: "quic://mac.example:8788" }),
  ])("rejects incomplete or ambiguous payload and still scrubs secrets: %s", (search) => {
    const { elements, history } = render(search);
    expect(elements.get("bad").hidden).toBe(false);
    expect(elements.get("details").hidden).toBe(true);
    expect(elements.get("open").attributes.href).toBeUndefined();
    expect(history).toEqual(["/connect"]);
  });

  test.each(["https://phux.sh/connect", "phux://connect"])("recovers a wrapped %s link locally", (base) => {
    const { elements, location } = render("");
    const search = query({ quic: "quic://relay.example:4433", sni: "studio" });
    elements.get("pairing-link").value = `${base}${search}`.replace("&fp", "\n&fp");
    elements.get("recover").submit({ preventDefault() {} });
    expect(location.href).toBe(`phux://connect${search}`);
    expect(elements.get("pairing-link").value).toBe("");
    expect(elements.get("bad").hidden).toBe(true);
  });

  test("does not open arbitrary pasted URLs", () => {
    const { elements, location } = render("");
    elements.get("pairing-link").value = `https://evil.example/connect${query({ url: "wss://mac.example:8787" })}`;
    elements.get("recover").submit({ preventDefault() {} });
    expect(location.href).toBe("");
    expect(elements.get("bad").hidden).toBe(false);
  });

  test("preserves CLI percent encoding and literal plus signs", () => {
    const search = `?url=wss://mac.example:8787&name=studio%20mini+lab&token=${token}&fp=${fp}`;
    const { elements } = render(search);
    expect(elements.get("name").textContent).toBe("studio mini+lab");
    expect(elements.get("open").attributes.href).toBe(`phux://connect${search}`);
  });
});
