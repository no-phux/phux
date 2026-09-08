import { describe, expect, test } from "bun:test";
import {
  buildPortfolioSnapshot,
  createGithubApi,
  createPortfolioSnapshotLoader,
  type PortfolioSnapshot,
} from "./portfolio";

const repo = {
  name: "phui",
  owner: { login: "phall1" },
  private: false,
  visibility: "public",
  archived: false,
  disabled: false,
  topics: ["phall-showcase"],
  description: "GitHub\u001b portfolio",
  homepage: "https://phall.io",
  language: "TypeScript",
  stargazers_count: 2,
  forks_count: 1,
  open_issues_count: 3,
  pushed_at: "2026-07-11T00:00:00Z",
};

const emptySnapshot: PortfolioSnapshot = {
  schema: 1,
  owner: "phall1",
  fetched_at: "2026-07-11T00:00:00Z",
  repos: [],
};

function json(value: unknown, status = 200): Response {
  return Response.json(value, { status });
}

async function appKey() {
  const pair = (await crypto.subtle.generateKey(
    {
      name: "RSASSA-PKCS1-v1_5",
      modulusLength: 2048,
      publicExponent: Uint8Array.of(1, 0, 1),
      hash: "SHA-256",
    },
    true,
    ["sign", "verify"],
  )) as CryptoKeyPair;
  const der = new Uint8Array(await crypto.subtle.exportKey("pkcs8", pair.privateKey));
  let binary = "";
  for (const byte of der) binary += String.fromCharCode(byte);
  const encoded = btoa(binary).match(/.{1,64}/g)?.join("\n") ?? "";
  return {
    pem: `-----BEGIN PRIVATE KEY-----\n${encoded}\n-----END PRIVATE KEY-----`,
    publicKey: pair.publicKey,
  };
}

function decodeBase64Url(value: string): Uint8Array {
  const padded = value.replaceAll("-", "+").replaceAll("_", "/").padEnd(
    Math.ceil(value.length / 4) * 4,
    "=",
  );
  return Uint8Array.from(atob(padded), (character) => character.charCodeAt(0));
}

describe("portfolio snapshots", () => {
  test("filters every publication boundary before sorting, truncating, and fetching runs", async () => {
    const eligible = Array.from({ length: 12 }, (_, index) => ({
      ...repo,
      name: `repo-${index}`,
      pushed_at: `2026-07-${String(index + 1).padStart(2, "0")}T00:00:00Z`,
    }));
    const runCalls: string[] = [];
    const snapshot = await buildPortfolioSnapshot(
      [
        ...eligible,
        { ...repo, name: "private", private: true },
        { ...repo, name: "non-public", visibility: "internal" },
        { ...repo, name: "archived", archived: true },
        { ...repo, name: "disabled", disabled: true },
        { ...repo, name: "untagged", topics: [] },
        { ...repo, name: "other-owner", owner: { login: "someone-else" } },
      ],
      async (name) => {
        runCalls.push(name);
        return null;
      },
      "2026-07-11T00:00:00Z",
    );

    expect(snapshot.repos.map(({ name }) => name)).toEqual([
      "repo-11",
      "repo-10",
      "repo-9",
      "repo-8",
      "repo-7",
      "repo-6",
      "repo-5",
      "repo-4",
      "repo-3",
      "repo-2",
    ]);
    expect(runCalls).toEqual(snapshot.repos.map(({ name }) => name));
    expect(snapshot.repos[0]?.description).toBe("GitHub portfolio");
    expect(snapshot.repos[0]?.url).toBe("https://github.com/phall1/repo-11");
  });

  test("rejects non-HTTPS homepages and normalizes invalid counts", async () => {
    const snapshot = await buildPortfolioSnapshot(
      [
        {
          ...repo,
          homepage: "javascript:alert(1)",
          stargazers_count: -1,
          forks_count: "not-a-number",
        },
      ],
      async () => null,
    );

    expect(snapshot.repos[0]?.homepage).toBeNull();
    expect(snapshot.repos[0]?.stars).toBe(0);
    expect(snapshot.repos[0]?.forks).toBe(0);
  });
});

describe("GitHub App discovery", () => {
  test("signs a short-lived JWT, resolves phall1, paginates, and reuses the installation token", async () => {
    let now = Date.parse("2026-07-11T00:00:00Z");
    const key = await appKey();
    let installationCalls = 0;
    let tokenCalls = 0;
    const repositoryPages: number[] = [];
    let jwt = "";

    const api = createGithubApi(async (url, init) => {
      const authorization = new Headers(init?.headers).get("Authorization") ?? "";
      if (url.endsWith("/users/phall1/installation")) {
        installationCalls += 1;
        jwt = authorization.replace("Bearer ", "");
        return json({ id: 42 });
      }
      if (url.endsWith("/app/installations/42/access_tokens")) {
        tokenCalls += 1;
        expect(init?.method).toBe("POST");
        expect(authorization).toBe(`Bearer ${jwt}`);
        return json({
          token: "installation-token",
          expires_at: new Date(now + 60 * 60 * 1_000).toISOString(),
        });
      }
      if (url.includes("/installation/repositories")) {
        expect(authorization).toBe("Bearer installation-token");
        const page = Number(new URL(url).searchParams.get("page"));
        repositoryPages.push(page);
        return page === 1
          ? json({ total_count: 101, repositories: Array(100).fill({}) })
          : json({ total_count: 101, repositories: [repo] });
      }
      if (url.includes("/actions/runs")) {
        expect(authorization).toBe("Bearer installation-token");
        return json({ workflow_runs: [] });
      }
      return json({}, 404);
    }, () => now);

    const env = { GITHUB_APP_ID: "12345", GITHUB_APP_PRIVATE_KEY: key.pem };
    const repositories = await api.discoverRepositories(env);
    await api.latestRun(env, "phui");

    const [header, payload, signature] = jwt.split(".");
    expect(JSON.parse(new TextDecoder().decode(decodeBase64Url(header!)))).toEqual({
      alg: "RS256",
      typ: "JWT",
    });
    expect(JSON.parse(new TextDecoder().decode(decodeBase64Url(payload!)))).toEqual({
      iat: Math.floor(now / 1000) - 60,
      exp: Math.floor(now / 1000) + 540,
      iss: "12345",
    });
    expect(
      await crypto.subtle.verify(
        "RSASSA-PKCS1-v1_5",
        key.publicKey,
        decodeBase64Url(signature!),
        new TextEncoder().encode(`${header}.${payload}`),
      ),
    ).toBe(true);
    expect(repositories).toHaveLength(101);
    expect(repositoryPages).toEqual([1, 2]);
    expect(installationCalls).toBe(1);
    expect(tokenCalls).toBe(1);

    now += 59 * 60 * 1_000;
    await api.discoverRepositories(env);
    expect(installationCalls).toBe(2);
    expect(tokenCalls).toBe(2);
  });

  test("uses public discovery only when both App bindings are absent", async () => {
    const urls: string[] = [];
    const api = createGithubApi(async (url) => {
      urls.push(url);
      return json([repo]);
    });

    expect(await api.discoverRepositories({})).toEqual([repo]);
    expect(urls).toEqual([
      "https://api.github.com/users/phall1/repos?per_page=100&type=owner&sort=pushed&page=1",
    ]);
  });
});

describe("portfolio loading", () => {
  test("fails closed before reading cache when either App binding is missing", async () => {
    let cacheReads = 0;
    let fetches = 0;
    const errors: unknown[][] = [];
    const originalError = console.error;
    console.error = (...values) => errors.push(values);
    try {
      const loader = createPortfolioSnapshotLoader(
        async () => {
          fetches += 1;
          return emptySnapshot;
        },
        () => ({
          match: async () => {
            cacheReads += 1;
            return Response.json(emptySnapshot);
          },
          put: async () => undefined,
        }),
      );
      const ctx = { waitUntil: () => undefined };

      for (const env of [
        { GITHUB_APP_ID: "123" },
        { GITHUB_APP_PRIVATE_KEY: "private-key" },
      ]) {
        const snapshot = await loader(ctx, env);
        expect(snapshot.repos).toEqual([]);
        expect(snapshot.error).toBe("GitHub data temporarily unavailable");
      }
    } finally {
      console.error = originalError;
    }

    expect(cacheReads).toBe(0);
    expect(fetches).toBe(0);
    expect(errors).toHaveLength(2);
    expect(String(errors[0]?.[1])).toContain("configuration is incomplete");
  });

  test("deduplicates concurrent cache misses", async () => {
    let fetches = 0;
    let resolveFetch: ((snapshot: PortfolioSnapshot) => void) | undefined;
    const fresh = new Promise<PortfolioSnapshot>((resolve) => {
      resolveFetch = resolve;
    });
    const loader = createPortfolioSnapshotLoader(
      async () => {
        fetches += 1;
        return fresh;
      },
      () => ({
        match: async () => undefined,
        put: async () => undefined,
      }),
    );
    const writes: Promise<unknown>[] = [];
    const ctx = { waitUntil: (promise: Promise<unknown>) => writes.push(promise) };

    const first = loader(ctx, {});
    const second = loader(ctx, {});
    await Promise.resolve();
    expect(fetches).toBe(1);
    resolveFetch!(emptySnapshot);

    expect(await first).toEqual(emptySnapshot);
    expect(await second).toEqual(emptySnapshot);
    await Promise.all(writes);
  });
});
