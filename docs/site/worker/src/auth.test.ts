import { describe, expect, test } from "bun:test";
import { exportJWK, generateKeyPair, SignJWT } from "jose";
import {
  createAuthRequestHandler,
  PUBLIC_APP_ORIGIN,
  PUBLIC_AUTH_ORIGIN,
  verifySyntheticBearer,
  verifySessionCookie,
  type AuthEnv,
} from "./auth";

const env: AuthEnv = {
  GITHUB_OAUTH_CLIENT_ID: "github-client",
  GITHUB_OAUTH_CLIENT_SECRET: "github-secret",
  GOOGLE_OIDC_CLIENT_ID: "google-client",
  GOOGLE_OIDC_CLIENT_SECRET: "google-secret",
  AUTH_COOKIE_SECRET: "test-cookie-secret-that-is-long-and-random",
};

function setCookies(response: Response): string[] {
  const headers = response.headers as Headers & { getSetCookie?: () => string[] };
  return headers.getSetCookie?.() ?? (response.headers.get("Set-Cookie")?.split(", ") ?? []);
}

function cookiePair(setCookie: string): string {
  return setCookie.split(";", 1)[0]!;
}

async function start(
  provider: "github" | "google",
  handler: ReturnType<typeof createAuthRequestHandler>,
  returnTo = "/",
) {
  const response = await handler(
    new Request(`${PUBLIC_AUTH_ORIGIN}/auth/${provider}?return_to=${encodeURIComponent(returnTo)}`),
    env,
  );
  expect(response?.status).toBe(302);
  return {
    authorization: new URL(response!.headers.get("Location")!),
    cookie: cookiePair(setCookies(response!)[0]!),
    response: response!,
  };
}

function requestWithCookie(url: string, cookie: string, init?: RequestInit): Request {
  const headers = new Headers(init?.headers);
  headers.set("Cookie", cookie);
  return new Request(url, { ...init, headers });
}

describe("OAuth starts", () => {
  test("uses exact-origin callbacks, S256 PKCE, no GitHub scopes, and exact transaction flags", async () => {
    const handler = createAuthRequestHandler(async () => {
      throw new Error("unexpected fetch");
    });
    const result = await start("github", handler, "/embed");

    expect(result.authorization.origin).toBe("https://github.com");
    expect(result.authorization.searchParams.get("redirect_uri")).toBe(
      `${PUBLIC_AUTH_ORIGIN}/auth/github/callback`,
    );
    expect(result.authorization.searchParams.get("code_challenge_method")).toBe("S256");
    expect(result.authorization.searchParams.get("code_challenge")?.length).toBe(43);
    expect(result.authorization.searchParams.has("scope")).toBe(false);
    expect(setCookies(result.response)[0]).toMatch(
      /^__Host-phux_oauth_github=.*; Path=\/; Max-Age=600; HttpOnly; Secure; SameSite=Lax$/,
    );
  });

  test("rejects untrusted request origins and open redirects", async () => {
    const handler = createAuthRequestHandler();
    expect((await handler(new Request("https://attacker.example/auth/github"), env))?.status).toBe(400);
    expect(
      (await handler(new Request(`${PUBLIC_AUTH_ORIGIN}/auth/google?return_to=https://attacker.example`), env))
        ?.status,
    ).toBe(400);
    expect((await handler(new Request(`${PUBLIC_AUTH_ORIGIN}/auth/github?return_to=//attacker.example`), env))?.status)
      .toBe(400);
  });
});

describe("OAuth callbacks", () => {
  test("rejects CSRF state mismatch before contacting GitHub", async () => {
    let fetches = 0;
    const handler = createAuthRequestHandler(async () => {
      fetches += 1;
      return Response.json({});
    });
    const initiated = await start("github", handler);
    const response = await handler(
      requestWithCookie(
        `${PUBLIC_AUTH_ORIGIN}/auth/github/callback?code=code&state=wrong-state`,
        initiated.cookie,
      ),
      env,
    );

    expect(response?.status).toBe(400);
    expect(fetches).toBe(0);
    expect(setCookies(response!)).toContain(
      "__Host-phux_oauth_github=; Path=/; Max-Age=0; HttpOnly; Secure; SameSite=Lax",
    );
  });

  test("returns provider cancellation to the frontend without exposing provider details", async () => {
    const handler = createAuthRequestHandler(async () => {
      throw new Error("unexpected fetch");
    });
    const initiated = await start("github", handler, "/embed");
    const response = await handler(
      requestWithCookie(
        `${PUBLIC_AUTH_ORIGIN}/auth/github/callback?error=access_denied&error_description=private&state=${initiated.authorization.searchParams.get("state")}`,
        initiated.cookie,
      ),
      env,
    );

    expect(response?.status).toBe(302);
    expect(response?.headers.get("Location")).toBe(`${PUBLIC_APP_ORIGIN}/embed?auth=error`);
    expect(response?.headers.get("Location")).not.toContain("private");
  });

  test("maps a GitHub numeric id and login, discards tokens, and mints the exact session cookie", async () => {
    const requests: Array<{ url: string; init?: RequestInit }> = [];
    const handler = createAuthRequestHandler(async (input, init) => {
      const url = String(input);
      requests.push({ url, init });
      if (url.endsWith("/access_token")) return Response.json({ access_token: "transient-token" });
      return Response.json({
        id: 123456,
        login: "octocat",
        created_at: "2020-01-01T00:00:00Z",
      });
    });
    const initiated = await start("github", handler, "/embed");
    const state = initiated.authorization.searchParams.get("state")!;
    const response = await handler(
      requestWithCookie(
        `${PUBLIC_AUTH_ORIGIN}/auth/github/callback?code=one-time-code&state=${state}`,
        initiated.cookie,
      ),
      env,
    );

    expect(response?.status).toBe(302);
    expect(response?.headers.get("Location")).toBe(`${PUBLIC_APP_ORIGIN}/embed?auth=success`);
    expect(String(requests[0]?.init?.body)).toContain("code_verifier=");
    expect(new Headers(requests[1]?.init?.headers).get("Authorization")).toBe("Bearer transient-token");
    const sessionSetCookie = setCookies(response!).find((value) => value.startsWith("__Host-phux_session="))!;
    expect(sessionSetCookie).toMatch(
      /^__Host-phux_session=.*; Path=\/; Max-Age=28800; HttpOnly; Secure; SameSite=Lax$/,
    );
    const identity = await verifySessionCookie(
      requestWithCookie(`${PUBLIC_AUTH_ORIGIN}/`, cookiePair(sessionSetCookie)),
      env.AUTH_COOKIE_SECRET,
    );
    expect(identity).toEqual({ principal: "github:123456", provider: "github", display: "octocat" });
    expect(sessionSetCookie).not.toContain("transient-token");
  });

  test("rejects newly created GitHub accounts", async () => {
    const handler = createAuthRequestHandler(async (input) => {
      const url = String(input);
      if (url.endsWith("/access_token")) {
        return Response.json({ access_token: "transient-token" });
      }
      return Response.json({
        id: 123456,
        login: "new-account",
        created_at: new Date(Date.now() - 24 * 60 * 60 * 1_000).toISOString(),
      });
    });
    const initiated = await start("github", handler);
    const response = await handler(
      requestWithCookie(
        `${PUBLIC_AUTH_ORIGIN}/auth/github/callback?code=one-time-code&state=${initiated.authorization.searchParams.get("state")}`,
        initiated.cookie,
      ),
      env,
    );

    expect(response?.status).toBe(302);
    expect(response?.headers.get("Location")).toBe(`${PUBLIC_APP_ORIGIN}/?auth=error`);
    expect(setCookies(response!)).toHaveLength(1);
  });

  test("verifies Google signature, issuer, audience, expiry, nonce, and maps verified identity", async () => {
    const { privateKey, publicKey } = await generateKeyPair("RS256");
    const jwk = await exportJWK(publicKey);
    jwk.kid = "google-key";
    const handler = createAuthRequestHandler(async (input) => {
      const url = String(input);
      if (url.endsWith("/certs")) return Response.json({ keys: [jwk] });
      if (url.endsWith("/token")) return Response.json({ id_token: idToken });
      throw new Error(`unexpected fetch: ${url}`);
    });
    const initiated = await start("google", handler);
    const nonce = initiated.authorization.searchParams.get("nonce")!;
    const state = initiated.authorization.searchParams.get("state")!;
    const idToken = await new SignJWT({
      nonce,
      email: "person@example.com",
      email_verified: true,
      name: "Person Name",
    })
      .setProtectedHeader({ alg: "RS256", kid: "google-key" })
      .setIssuer("https://accounts.google.com")
      .setAudience(env.GOOGLE_OIDC_CLIENT_ID)
      .setSubject("google-subject")
      .setIssuedAt()
      .setExpirationTime("5m")
      .sign(privateKey);
    const response = await handler(
      requestWithCookie(
        `${PUBLIC_AUTH_ORIGIN}/auth/google/callback?code=one-time-code&state=${state}`,
        initiated.cookie,
      ),
      env,
    );

    expect(response?.status).toBe(302);
    const session = setCookies(response!).find((value) => value.startsWith("__Host-phux_session="))!;
    expect(
      await verifySessionCookie(
        requestWithCookie(`${PUBLIC_AUTH_ORIGIN}/`, cookiePair(session)),
        env.AUTH_COOKIE_SECRET,
      ),
    ).toEqual({
      principal: "google:google-subject",
      provider: "google",
      display: "Person Name",
      email: "person@example.com",
    });
  });

  test("rejects an unverified Google email", async () => {
    const { privateKey, publicKey } = await generateKeyPair("RS256");
    const jwk = await exportJWK(publicKey);
    jwk.kid = "google-key";
    let idToken = "";
    const handler = createAuthRequestHandler(async (input) => {
      if (String(input).endsWith("/certs")) return Response.json({ keys: [jwk] });
      return Response.json({ id_token: idToken });
    });
    const initiated = await start("google", handler);
    idToken = await new SignJWT({
      nonce: initiated.authorization.searchParams.get("nonce"),
      email: "person@example.com",
      email_verified: false,
    })
      .setProtectedHeader({ alg: "RS256", kid: "google-key" })
      .setIssuer("https://accounts.google.com")
      .setAudience(env.GOOGLE_OIDC_CLIENT_ID)
      .setSubject("google-subject")
      .setExpirationTime("5m")
      .sign(privateKey);
    const response = await handler(
      requestWithCookie(
        `${PUBLIC_AUTH_ORIGIN}/auth/google/callback?code=code&state=${initiated.authorization.searchParams.get("state")}`,
        initiated.cookie,
      ),
      env,
    );
    expect(response?.status).toBe(302);
    expect(response?.headers.get("Location")).toBe(`${PUBLIC_APP_ORIGIN}/?auth=error`);
  });
});

describe("sessions", () => {
  async function githubSession(now = Date.now()) {
    const handler = createAuthRequestHandler(async (input) =>
      String(input).endsWith("/access_token")
        ? Response.json({ access_token: "token" })
        : Response.json({
            id: 42,
            login: "safe-login",
            created_at: new Date(now - 30 * 24 * 60 * 60 * 1_000).toISOString(),
          }), () => now);
    const initiated = await start("github", handler);
    const response = await handler(
      requestWithCookie(
        `${PUBLIC_AUTH_ORIGIN}/auth/github/callback?code=code&state=${initiated.authorization.searchParams.get("state")}`,
        initiated.cookie,
      ),
      env,
    );
    return {
      handler,
      session: cookiePair(setCookies(response!).find((value) => value.startsWith("__Host-phux_session="))!),
      now,
    };
  }

  test("rejects tampered and expired signed session cookies", async () => {
    const created = await githubSession(1_800_000_000_000);
    const request = requestWithCookie(`${PUBLIC_AUTH_ORIGIN}/`, created.session);
    expect(await verifySessionCookie(request, env.AUTH_COOKIE_SECRET, created.now)).not.toBeNull();
    expect(
      await verifySessionCookie(
        requestWithCookie(`${PUBLIC_AUTH_ORIGIN}/`, `${created.session.slice(0, -1)}x`),
        env.AUTH_COOKIE_SECRET,
        created.now,
      ),
    ).toBeNull();
    expect(
      await verifySessionCookie(request, env.AUTH_COOKIE_SECRET, created.now + 8 * 60 * 60 * 1_000 + 1),
    ).toBeNull();
  });

  test("session introspection is no-store and returns only safe identity fields", async () => {
    const created = await githubSession();
    const response = await created.handler(
      requestWithCookie(`${PUBLIC_AUTH_ORIGIN}/auth/session`, created.session, {
        headers: { Origin: PUBLIC_APP_ORIGIN },
      }),
      env,
    );
    expect(response?.headers.get("Cache-Control")).toBe("no-store");
    expect(response?.headers.get("Access-Control-Allow-Origin")).toBe(PUBLIC_APP_ORIGIN);
    expect(response?.headers.get("Access-Control-Allow-Credentials")).toBe("true");
    expect(await response?.json()).toEqual({
      authenticated: true,
      provider: "github",
      display: "safe-login",
    });
  });

  test("session introspection rejects unrelated browser origins", async () => {
    const handler = createAuthRequestHandler();
    const response = await handler(
      new Request(`${PUBLIC_AUTH_ORIGIN}/auth/session`, {
        headers: { Origin: "https://attacker.example" },
      }),
      env,
    );
    expect(response?.status).toBe(403);
    expect(response?.headers.has("Access-Control-Allow-Origin")).toBe(false);
  });

  test("logout requires the exact same Origin and clears the host cookie", async () => {
    const handler = createAuthRequestHandler();
    for (const origin of [null, "https://attacker.example", "https://shell.phux.sh.evil"]) {
      const headers = origin ? { Origin: origin } : undefined;
      expect(
        (await handler(new Request(`${PUBLIC_AUTH_ORIGIN}/auth/logout`, { method: "POST", headers }), env))?.status,
      ).toBe(403);
    }
    const response = await handler(
      new Request(`${PUBLIC_AUTH_ORIGIN}/auth/logout`, {
        method: "POST",
        headers: { Origin: PUBLIC_APP_ORIGIN },
      }),
      env,
    );
    expect(response?.status).toBe(204);
    expect(response?.headers.get("Access-Control-Allow-Origin")).toBe(PUBLIC_APP_ORIGIN);
    expect(response?.headers.get("Set-Cookie")).toBe(
      "__Host-phux_session=; Path=/; Max-Age=0; HttpOnly; Secure; SameSite=Lax",
    );
  });
});

describe("production monitor authentication", () => {
  test("accepts only the exact bearer secret without exposing it", () => {
    const secret = "monitor-secret-with-enough-randomness";
    const request = new Request(`${PUBLIC_AUTH_ORIGIN}/session?mode=native`, {
      headers: { Authorization: `Bearer ${secret}` },
    });
    expect(verifySyntheticBearer(request, secret)).toEqual({
      principal: "synthetic:production-monitor",
    });
    expect(verifySyntheticBearer(request, `${secret}x`)).toBeNull();
    expect(verifySyntheticBearer(request, undefined)).toBeNull();
    expect(JSON.stringify(verifySyntheticBearer(request, secret))).not.toContain(secret);
  });
});
