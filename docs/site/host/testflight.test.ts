import { describe, expect, test } from "bun:test";
import { decodeJwt, decodeProtectedHeader } from "jose";
import { inviteTester, type TestFlightEnv } from "./testflight";

async function privateKeyPem(): Promise<string> {
  const pair = (await crypto.subtle.generateKey(
    { name: "ECDSA", namedCurve: "P-256" },
    true,
    ["sign", "verify"],
  )) as CryptoKeyPair;
  const der = new Uint8Array(await crypto.subtle.exportKey("pkcs8", pair.privateKey));
  const base64 = btoa(String.fromCharCode(...der));
  return `-----BEGIN PRIVATE KEY-----\n${base64.match(/.{1,64}/g)!.join("\n")}\n-----END PRIVATE KEY-----`;
}

const PEM = await privateKeyPem();
let issuers = 0;

/** Fresh issuer per test so the isolate-level token and group caches never leak. */
function env(extra: Partial<TestFlightEnv> = {}): TestFlightEnv {
  issuers += 1;
  return {
    ASC_KEY_ID: "KEY123",
    ASC_ISSUER_ID: `issuer-${issuers}`,
    ASC_PRIVATE_KEY: PEM,
    ...extra,
  };
}

type Route = (request: Request) => Response | Promise<Response>;

function fakeAsc(routes: Record<string, Route>) {
  const calls: { method: string; path: string; body: unknown; auth: string }[] = [];
  const fetchImpl = async (request: Request) => {
    const url = new URL(request.url);
    const path = decodeURIComponent(url.pathname + url.search).replace("/v1", "");
    const body = request.body ? await request.json() : undefined;
    calls.push({
      method: request.method,
      path,
      body,
      auth: request.headers.get("authorization") ?? "",
    });
    const route = routes[`${request.method} ${path.split("?")[0]}`];
    return route ? route(request) : new Response("unexpected", { status: 500 });
  };
  return { calls, fetchImpl };
}

describe("inviteTester", () => {
  test("is unconfigured without the API key secrets", async () => {
    const { calls, fetchImpl } = fakeAsc({});
    expect(await inviteTester({}, "a@example.com", fetchImpl)).toBe("unconfigured");
    expect(calls).toHaveLength(0);
  });

  test("resolves the external group and creates the tester in it", async () => {
    const { calls, fetchImpl } = fakeAsc({
      "GET /apps": () => Response.json({ data: [{ id: "app-1", attributes: {} }] }),
      "GET /apps/app-1/betaGroups": () =>
        Response.json({
          data: [
            { id: "internal", attributes: { name: "Team", isInternalGroup: true } },
            { id: "public", attributes: { name: "Public", isInternalGroup: false } },
          ],
        }),
      "POST /betaTesters": () => new Response("{}", { status: 201 }),
    });
    const config = env();
    expect(await inviteTester(config, "a@example.com", fetchImpl)).toBe("invited");

    expect(calls.map((call) => `${call.method} ${call.path.split("?")[0]}`)).toEqual([
      "GET /apps",
      "GET /apps/app-1/betaGroups",
      "POST /betaTesters",
    ]);
    expect(calls[0]!.path).toContain("filter[bundleId]=dev.phux.mobile");
    expect(calls[2]!.body).toEqual({
      data: {
        type: "betaTesters",
        attributes: { email: "a@example.com" },
        relationships: { betaGroups: { data: [{ type: "betaGroups", id: "public" }] } },
      },
    });

    const jwt = calls[0]!.auth.replace("Bearer ", "");
    expect(decodeProtectedHeader(jwt)).toMatchObject({ alg: "ES256", kid: "KEY123", typ: "JWT" });
    const claims = decodeJwt(jwt);
    expect(claims.iss).toBe(config.ASC_ISSUER_ID);
    expect(claims.aud).toBe("appstoreconnect-v1");
    expect(claims.exp! - claims.iat!).toBeLessThanOrEqual(20 * 60);

    // The group and token are cached for the isolate's lifetime.
    await inviteTester(config, "b@example.com", fetchImpl);
    expect(calls.slice(3).map((call) => call.path)).toEqual(["/betaTesters"]);
    expect(calls[3]!.auth).toBe(calls[0]!.auth);
  });

  test("selects the named group and accepts an escaped single-line key", async () => {
    const { calls, fetchImpl } = fakeAsc({
      "GET /apps": () => Response.json({ data: [{ id: "app-1", attributes: {} }] }),
      "GET /apps/app-1/betaGroups": () =>
        Response.json({
          data: [
            { id: "first", attributes: { name: "Friends", isInternalGroup: false } },
            { id: "named", attributes: { name: "Site", isInternalGroup: false } },
          ],
        }),
      "POST /betaTesters": () => new Response("{}", { status: 201 }),
    });
    const config = env({ ASC_BETA_GROUP: "Site", ASC_PRIVATE_KEY: PEM.replace(/\n/g, "\\n") });
    expect(await inviteTester(config, "a@example.com", fetchImpl)).toBe("invited");
    expect(calls.at(-1)!.body).toMatchObject({
      data: { relationships: { betaGroups: { data: [{ id: "named" }] } } },
    });
  });

  test("attaches an existing tester to the group on conflict", async () => {
    const { calls, fetchImpl } = fakeAsc({
      "POST /betaTesters": () => new Response("{}", { status: 409 }),
      "GET /betaTesters": () => Response.json({ data: [{ id: "tester-9", attributes: {} }] }),
      "POST /betaGroups/group-1/relationships/betaTesters": () => new Response(null, { status: 204 }),
    });
    const outcome = await inviteTester(env({ ASC_BETA_GROUP_ID: "group-1" }), "a+b@example.com", fetchImpl);
    expect(outcome).toBe("invited");
    expect(calls[1]!.path).toContain("filter[email]=a+b@example.com");
    expect(calls[2]!.body).toEqual({ data: [{ type: "betaTesters", id: "tester-9" }] });
  });

  test("reports failure without throwing", async () => {
    const quiet = console.error;
    console.error = () => {};
    try {
      const { fetchImpl } = fakeAsc({
        "POST /betaTesters": () => new Response("{}", { status: 403 }),
      });
      expect(await inviteTester(env({ ASC_BETA_GROUP_ID: "g" }), "a@example.com", fetchImpl)).toBe("failed");
      const noGroup = fakeAsc({
        "GET /apps": () => Response.json({ data: [{ id: "app-1", attributes: {} }] }),
        "GET /apps/app-1/betaGroups": () =>
          Response.json({ data: [{ id: "i", attributes: { name: "T", isInternalGroup: true } }] }),
      });
      expect(await inviteTester(env(), "a@example.com", noGroup.fetchImpl)).toBe("failed");
      expect(await inviteTester(env({ ASC_PRIVATE_KEY: "not a key" }), "a@example.com", noGroup.fetchImpl)).toBe("failed");
    } finally {
      console.error = quiet;
    }
  });
});
