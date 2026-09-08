export type DemoMode = "demo" | "portfolio" | "native" | "native-fallback";

export interface GithubAppEnv {
  GITHUB_APP_ID?: string;
  GITHUB_APP_PRIVATE_KEY?: string;
}

export interface PortfolioRun {
  workflow: string;
  branch: string;
  status: string;
  conclusion: string | null;
  url: string;
  updated_at: string;
}

export interface PortfolioRepo {
  name: string;
  description: string | null;
  url: string;
  homepage: string | null;
  language: string | null;
  stars: number;
  forks: number;
  open_issues: number;
  pushed_at: string;
  latest_run: PortfolioRun | null;
}

export interface PortfolioSnapshot {
  schema: 1;
  owner: "phall1";
  fetched_at: string;
  repos: PortfolioRepo[];
  error?: string;
}

type GithubFetch = (input: string, init?: RequestInit) => Promise<Response>;
type SnapshotFetcher = (env: GithubAppEnv) => Promise<PortfolioSnapshot>;
type SnapshotCache = Pick<Cache, "match" | "put">;

const OWNER = "phall1";
const SHOWCASE_TOPIC = "phall-showcase";
const CACHE_KEY = "https://phux-demo.internal/portfolio-v1";
const CACHE_SECONDS = 300;
const MAX_REPOS = 10;
const TOKEN_EXPIRY_MARGIN_MS = 60_000;

const baseHeaders = {
  Accept: "application/vnd.github+json",
  "User-Agent": "phall1-portfolio-console",
  "X-GitHub-Api-Version": "2022-11-28",
};

const clean = (value: unknown, max: number): string =>
  (typeof value === "string" ? value : "")
    .replace(
      /[\u0000-\u0008\u000b\u000c\u000e-\u001f\u007f-\u009f\u202a-\u202e\u2066-\u2069]/g,
      "",
    )
    .slice(0, max);

const count = (value: unknown): number => {
  const parsed = Number(value);
  return Number.isFinite(parsed) && parsed > 0
    ? Math.min(Math.trunc(parsed), Number.MAX_SAFE_INTEGER)
    : 0;
};

const httpsUrl = (value: unknown, max = 256): string => {
  const candidate = clean(value, max);
  try {
    const url = new URL(candidate);
    return url.protocol === "https:" ? url.toString() : "";
  } catch {
    return "";
  }
};

function appConfigured(env: GithubAppEnv): boolean {
  const hasId = Boolean(env.GITHUB_APP_ID?.trim());
  const hasKey = Boolean(env.GITHUB_APP_PRIVATE_KEY?.trim());
  if (hasId !== hasKey) {
    throw new Error(
      "GitHub App configuration is incomplete: set both GITHUB_APP_ID and GITHUB_APP_PRIVATE_KEY",
    );
  }
  return hasId;
}

function base64Url(value: string | ArrayBuffer): string {
  const bytes =
    typeof value === "string"
      ? new TextEncoder().encode(value)
      : new Uint8Array(value);
  let binary = "";
  for (const byte of bytes) binary += String.fromCharCode(byte);
  return btoa(binary)
    .replaceAll("=", "")
    .replaceAll("+", "-")
    .replaceAll("/", "_");
}

function derLength(length: number): Uint8Array {
  if (length < 128) return Uint8Array.of(length);
  const bytes: number[] = [];
  for (let value = length; value > 0; value >>>= 8)
    bytes.unshift(value & 0xff);
  return Uint8Array.of(0x80 | bytes.length, ...bytes);
}

function der(tag: number, value: Uint8Array): Uint8Array {
  const length = derLength(value.length);
  const result = new Uint8Array(1 + length.length + value.length);
  result[0] = tag;
  result.set(length, 1);
  result.set(value, 1 + length.length);
  return result;
}

function concat(...values: Uint8Array[]): Uint8Array {
  const result = new Uint8Array(
    values.reduce((sum, value) => sum + value.length, 0),
  );
  let offset = 0;
  for (const value of values) {
    result.set(value, offset);
    offset += value.length;
  }
  return result;
}

function privateKeyDer(pem: string): Uint8Array {
  const pkcs1 = pem.includes("-----BEGIN RSA PRIVATE KEY-----");
  const encoded = pem
    .replace(/-----BEGIN (?:RSA )?PRIVATE KEY-----/, "")
    .replace(/-----END (?:RSA )?PRIVATE KEY-----/, "")
    .replace(/\s/g, "");
  if (!encoded) throw new Error("GitHub App private key is not valid PEM");

  const binary = atob(encoded);
  const key = Uint8Array.from(binary, (character) => character.charCodeAt(0));
  if (!pkcs1) return key;

  const version = Uint8Array.of(0x02, 0x01, 0x00);
  const rsaAlgorithm = Uint8Array.of(
    0x30,
    0x0d,
    0x06,
    0x09,
    0x2a,
    0x86,
    0x48,
    0x86,
    0xf7,
    0x0d,
    0x01,
    0x01,
    0x01,
    0x05,
    0x00,
  );
  return der(0x30, concat(version, rsaAlgorithm, der(0x04, key)));
}

async function appJwt(
  appId: string,
  privateKey: string,
  now: number,
): Promise<string> {
  const issuedAt = Math.floor(now / 1000) - 60;
  const header = base64Url(JSON.stringify({ alg: "RS256", typ: "JWT" }));
  const payload = base64Url(
    JSON.stringify({ iat: issuedAt, exp: issuedAt + 600, iss: appId }),
  );
  const unsigned = `${header}.${payload}`;
  const key = await crypto.subtle.importKey(
    "pkcs8",
    privateKeyDer(privateKey),
    { name: "RSASSA-PKCS1-v1_5", hash: "SHA-256" },
    false,
    ["sign"],
  );
  const signature = await crypto.subtle.sign(
    "RSASSA-PKCS1-v1_5",
    key,
    new TextEncoder().encode(unsigned),
  );
  return `${unsigned}.${base64Url(signature)}`;
}

async function responseJson(response: Response): Promise<unknown> {
  if (!response.ok) throw new Error(`GitHub returned ${response.status}`);
  return response.json();
}

export function createGithubApi(fetcher: GithubFetch = fetch, clock = Date.now) {
  let tokenCache:
    | { appId: string; token: string; expiresAt: number }
    | undefined;

  async function installationToken(env: GithubAppEnv): Promise<string> {
    const appId = env.GITHUB_APP_ID!.trim();
    const now = clock();
    if (
      tokenCache?.appId === appId &&
      now < tokenCache.expiresAt - TOKEN_EXPIRY_MARGIN_MS
    ) {
      return tokenCache.token;
    }

    const jwt = await appJwt(appId, env.GITHUB_APP_PRIVATE_KEY!, now);
    const installation = (await responseJson(
      await fetcher(`https://api.github.com/users/${OWNER}/installation`, {
        headers: { ...baseHeaders, Authorization: `Bearer ${jwt}` },
      }),
    )) as { id?: unknown };
    if (!Number.isSafeInteger(installation.id) || Number(installation.id) <= 0) {
      throw new Error(`GitHub App is not installed on ${OWNER}`);
    }

    const value = (await responseJson(
      await fetcher(
        `https://api.github.com/app/installations/${installation.id}/access_tokens`,
        {
          method: "POST",
          headers: { ...baseHeaders, Authorization: `Bearer ${jwt}` },
        },
      ),
    )) as { token?: unknown; expires_at?: unknown };
    const token = typeof value.token === "string" ? value.token : "";
    const expiresAt = Date.parse(
      typeof value.expires_at === "string" ? value.expires_at : "",
    );
    if (!token || !Number.isFinite(expiresAt)) {
      throw new Error("GitHub returned an invalid installation token");
    }
    tokenCache = { appId, token, expiresAt };
    return token;
  }

  async function authenticatedHeaders(env: GithubAppEnv): Promise<HeadersInit> {
    return {
      ...baseHeaders,
      Authorization: `Bearer ${await installationToken(env)}`,
    };
  }

  async function discoverRepositories(env: GithubAppEnv): Promise<unknown[]> {
    const appMode = appConfigured(env);
    const repositories: unknown[] = [];
    for (let page = 1; ; page += 1) {
      const url = appMode
        ? `https://api.github.com/installation/repositories?per_page=100&page=${page}`
        : `https://api.github.com/users/${OWNER}/repos?per_page=100&type=owner&sort=pushed&page=${page}`;
      const headers = appMode ? await authenticatedHeaders(env) : baseHeaders;
      const value = await responseJson(await fetcher(url, { headers }));
      const pageRepos = appMode
        ? ((value as { repositories?: unknown[] }).repositories ?? [])
        : (value as unknown[]);
      if (!Array.isArray(pageRepos))
        throw new Error("GitHub returned invalid repositories");
      repositories.push(...pageRepos);

      const total = appMode
        ? Number((value as { total_count?: unknown }).total_count)
        : NaN;
      if (
        pageRepos.length < 100 ||
        (Number.isFinite(total) && repositories.length >= total)
      )
        break;
    }
    return repositories;
  }

  async function latestRun(
    env: GithubAppEnv,
    name: string,
  ): Promise<PortfolioRun | null> {
    try {
      const headers = appConfigured(env)
        ? await authenticatedHeaders(env)
        : baseHeaders;
      const value = (await responseJson(
        await fetcher(
          `https://api.github.com/repos/${OWNER}/${encodeURIComponent(name)}/actions/runs?per_page=1`,
          { headers },
        ),
      )) as { workflow_runs?: unknown[] };
      const run = value.workflow_runs?.[0] as
        | Record<string, unknown>
        | undefined;
      if (!run) return null;
      return {
        workflow: clean(run.name, 48) || "workflow",
        branch: clean(run.head_branch, 64),
        status: clean(run.status, 24),
        conclusion: run.conclusion === null ? null : clean(run.conclusion, 24),
        url: httpsUrl(run.html_url),
        updated_at: clean(run.updated_at, 40),
      };
    } catch {
      return null;
    }
  }

  return { discoverRepositories, latestRun };
}

export async function buildPortfolioSnapshot(
  values: unknown[],
  resolveLatestRun: (name: string) => Promise<PortfolioRun | null>,
  fetchedAt = new Date().toISOString(),
): Promise<PortfolioSnapshot> {
  const discovered = values
    .filter(
      (value): value is Record<string, unknown> =>
        typeof value === "object" && value !== null,
    )
    .filter((repo) => {
      const owner = repo.owner as Record<string, unknown> | undefined;
      return (
        typeof repo.name === "string" &&
        repo.name.length > 0 &&
        owner?.login === OWNER &&
        repo.private === false &&
        repo.visibility === "public" &&
        repo.archived === false &&
        repo.disabled === false &&
        Array.isArray(repo.topics) &&
        repo.topics.includes(SHOWCASE_TOPIC)
      );
    })
    .sort((left, right) =>
      clean(right.pushed_at, 40).localeCompare(clean(left.pushed_at, 40)),
    )
    .slice(0, MAX_REPOS);

  const repos = await Promise.all(
    discovered.map(async (repo): Promise<PortfolioRepo> => {
      const name = clean(repo.name, 64);
      return {
        name,
        description:
          repo.description === null ? null : clean(repo.description, 160),
        url: `https://github.com/${OWNER}/${encodeURIComponent(name)}`,
        homepage: repo.homepage ? httpsUrl(repo.homepage) || null : null,
        language: repo.language ? clean(repo.language, 32) : null,
        stars: count(repo.stargazers_count),
        forks: count(repo.forks_count),
        open_issues: count(repo.open_issues_count),
        pushed_at: clean(repo.pushed_at, 40),
        latest_run: await resolveLatestRun(name),
      };
    }),
  );

  return { schema: 1, owner: OWNER, fetched_at: fetchedAt, repos };
}

const github = createGithubApi();

async function fetchSnapshot(env: GithubAppEnv): Promise<PortfolioSnapshot> {
  const values = await github.discoverRepositories(env);
  return buildPortfolioSnapshot(values, (name) => github.latestRun(env, name));
}

export function createPortfolioSnapshotLoader(
  fetchFresh: SnapshotFetcher = fetchSnapshot,
  cacheProvider: () => SnapshotCache = () => caches.default,
) {
  let inFlight: Promise<PortfolioSnapshot> | undefined;

  return async function loadPortfolioSnapshot(
    ctx: Pick<ExecutionContext, "waitUntil">,
    env: GithubAppEnv,
  ): Promise<PortfolioSnapshot> {
    try {
      // A partial App configuration must not be masked by a cached snapshot.
      appConfigured(env);
      const cache = cacheProvider();
      const cached = await cache.match(CACHE_KEY);
      if (cached) return cached.json<PortfolioSnapshot>();

      if (!inFlight) {
        inFlight = fetchFresh(env).finally(() => {
          inFlight = undefined;
        });
      }
      const snapshot = await inFlight;
      const response = new Response(JSON.stringify(snapshot), {
        headers: {
          "Content-Type": "application/json",
          "Cache-Control": `public, max-age=${CACHE_SECONDS}`,
        },
      });
      ctx.waitUntil(cache.put(CACHE_KEY, response));
      return snapshot;
    } catch (error) {
      console.error("Portfolio discovery failed", error);
      return {
        schema: 1,
        owner: OWNER,
        fetched_at: new Date().toISOString(),
        repos: [],
        error: "GitHub data temporarily unavailable",
      };
    }
  };
}

export const loadPortfolioSnapshot = createPortfolioSnapshotLoader();
