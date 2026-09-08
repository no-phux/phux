import { createLocalJWKSet, jwtVerify, type JSONWebKeySet } from "jose";

export const PUBLIC_AUTH_ORIGIN = "https://shell.phux.sh";
export const PUBLIC_APP_ORIGIN = "https://phux.sh";

const SESSION_COOKIE = "__Host-phux_session";
const TRANSACTION_COOKIE_PREFIX = "__Host-phux_oauth_";
const SESSION_MAX_AGE_SECONDS = 8 * 60 * 60;
const TRANSACTION_MAX_AGE_SECONDS = 10 * 60;
const GITHUB_MIN_ACCOUNT_AGE_MS = 7 * 24 * 60 * 60 * 1_000;
const encoder = new TextEncoder();

export interface AuthEnv {
  GITHUB_OAUTH_CLIENT_ID: string;
  GITHUB_OAUTH_CLIENT_SECRET: string;
  GOOGLE_OIDC_CLIENT_ID: string;
  GOOGLE_OIDC_CLIENT_SECRET: string;
  AUTH_COOKIE_SECRET: string;
}

export interface SessionIdentity {
  principal: string;
  provider: "github" | "google";
  display: string;
  email?: string;
}

export function verifySyntheticBearer(
  request: Request,
  secret: string | undefined,
): Pick<SessionIdentity, "principal"> | null {
  if (!secret) return null;
  const authorization = request.headers.get("Authorization") ?? "";
  if (!authorization.startsWith("Bearer ")) return null;
  const candidate = authorization.slice("Bearer ".length);
  let difference = candidate.length ^ secret.length;
  const length = Math.max(candidate.length, secret.length);
  for (let index = 0; index < length; index++) {
    difference |= (candidate.charCodeAt(index) || 0) ^ (secret.charCodeAt(index) || 0);
  }
  return difference === 0
    ? { principal: "synthetic:production-monitor" }
    : null;
}

interface SessionClaims extends SessionIdentity {
  exp: number;
}

interface TransactionClaims {
  provider: "github" | "google";
  state: string;
  verifier: string;
  nonce?: string;
  returnPath: "/" | "/embed";
  exp: number;
}

type Fetcher = (input: RequestInfo | URL, init?: RequestInit) => Promise<Response>;

function base64UrlEncode(bytes: Uint8Array): string {
  let binary = "";
  for (const byte of bytes) binary += String.fromCharCode(byte);
  return btoa(binary).replaceAll("+", "-").replaceAll("/", "_").replace(/=+$/, "");
}

function base64UrlDecode(value: string): Uint8Array | null {
  try {
    const base64 = value.replaceAll("-", "+").replaceAll("_", "/").padEnd(
      Math.ceil(value.length / 4) * 4,
      "=",
    );
    return Uint8Array.from(atob(base64), (character) => character.charCodeAt(0));
  } catch {
    return null;
  }
}

async function hmac(secret: string, value: string): Promise<Uint8Array> {
  const key = await crypto.subtle.importKey(
    "raw",
    encoder.encode(secret),
    { name: "HMAC", hash: "SHA-256" },
    false,
    ["sign"],
  );
  return new Uint8Array(await crypto.subtle.sign("HMAC", key, encoder.encode(value)));
}

function equalBytes(left: Uint8Array, right: Uint8Array): boolean {
  if (left.length !== right.length) return false;
  let difference = 0;
  for (let index = 0; index < left.length; index += 1) {
    difference |= left[index]! ^ right[index]!;
  }
  return difference === 0;
}

async function signClaims(secret: string, claims: object): Promise<string> {
  const payload = base64UrlEncode(encoder.encode(JSON.stringify(claims)));
  return `${payload}.${base64UrlEncode(await hmac(secret, payload))}`;
}

async function verifyClaims<T>(
  secret: string,
  value: string | undefined,
  now: number,
): Promise<T | null> {
  if (!value) return null;
  const parts = value.split(".");
  if (parts.length !== 2) return null;
  const provided = base64UrlDecode(parts[1]!);
  if (!provided || !equalBytes(await hmac(secret, parts[0]!), provided)) return null;

  const bytes = base64UrlDecode(parts[0]!);
  if (!bytes) return null;
  try {
    const claims = JSON.parse(new TextDecoder().decode(bytes)) as T & { exp?: unknown };
    if (typeof claims.exp !== "number" || claims.exp <= now) return null;
    return claims;
  } catch {
    return null;
  }
}

function cookies(request: Request): Map<string, string> {
  const result = new Map<string, string>();
  for (const part of (request.headers.get("Cookie") ?? "").split(";")) {
    const separator = part.indexOf("=");
    if (separator > 0) result.set(part.slice(0, separator).trim(), part.slice(separator + 1).trim());
  }
  return result;
}

function cookie(name: string, value: string, maxAge: number): string {
  return `${name}=${value}; Path=/; Max-Age=${maxAge}; HttpOnly; Secure; SameSite=Lax`;
}

function clearCookie(name: string): string {
  return cookie(name, "", 0);
}

function randomValue(byteLength = 32): string {
  return base64UrlEncode(crypto.getRandomValues(new Uint8Array(byteLength)));
}

async function pkceChallenge(verifier: string): Promise<string> {
  return base64UrlEncode(new Uint8Array(await crypto.subtle.digest("SHA-256", encoder.encode(verifier))));
}

function safeText(value: unknown, maxLength = 200): string | null {
  if (typeof value !== "string") return null;
  const cleaned = value.replace(/[\u0000-\u001f\u007f]/g, "").trim().slice(0, maxLength);
  return cleaned || null;
}

function returnPath(url: URL): "/" | "/embed" | null {
  const value = url.searchParams.get("return_to") ?? "/";
  return value === "/" || value === "/embed" ? value : null;
}

function redirectAfterAuth(path: "/" | "/embed", result: "success" | "error"): Response {
  const url = new URL(path, PUBLIC_APP_ORIGIN);
  url.searchParams.set("auth", result);
  return new Response(null, { status: 302, headers: { Location: url.toString() } });
}

function json(value: unknown, status = 200, cors = false): Response {
  const headers = new Headers({ "Cache-Control": "no-store" });
  if (cors) {
    headers.set("Access-Control-Allow-Credentials", "true");
    headers.set("Access-Control-Allow-Origin", PUBLIC_APP_ORIGIN);
    headers.set("Vary", "Origin");
  }
  return Response.json(value, {
    status,
    headers,
  });
}

function formResponse(response: Response): Promise<Record<string, unknown>> {
  if (!response.ok) throw new Error(`OAuth endpoint returned ${response.status}`);
  return response.json() as Promise<Record<string, unknown>>;
}

function validTransaction(value: TransactionClaims, provider: "github" | "google"): boolean {
  return (
    value.provider === provider &&
    typeof value.state === "string" &&
    value.state.length >= 32 &&
    typeof value.verifier === "string" &&
    value.verifier.length >= 43 &&
    (value.returnPath === "/" || value.returnPath === "/embed") &&
    (provider === "github" || (typeof value.nonce === "string" && value.nonce.length >= 32))
  );
}

export async function verifySessionCookie(
  request: Request,
  secret: string,
  now = Date.now(),
): Promise<SessionIdentity | null> {
  const claims = await verifyClaims<SessionClaims>(secret, cookies(request).get(SESSION_COOKIE), now);
  if (
    !claims ||
    (claims.provider !== "github" && claims.provider !== "google") ||
    typeof claims.principal !== "string" ||
    !claims.principal.startsWith(`${claims.provider}:`) ||
    typeof claims.display !== "string" ||
    !claims.display
  ) {
    return null;
  }
  return {
    principal: claims.principal,
    provider: claims.provider,
    display: claims.display,
    ...(typeof claims.email === "string" ? { email: claims.email } : {}),
  };
}

export function createAuthRequestHandler(
  fetcher: Fetcher = fetch,
  now: () => number = Date.now,
) {
  return async function authRequestHandler(request: Request, env: AuthEnv): Promise<Response | null> {
    const url = new URL(request.url);
    if (!url.pathname.startsWith("/auth/")) return null;
    if (url.origin !== PUBLIC_AUTH_ORIGIN) return json({ error: "invalid auth origin" }, 400);
    if (!env.AUTH_COOKIE_SECRET) return json({ error: "authentication is not configured" }, 500);

    if (url.pathname === "/auth/session" && request.method === "GET") {
      const origin = request.headers.get("Origin");
      if (origin && origin !== PUBLIC_APP_ORIGIN && origin !== PUBLIC_AUTH_ORIGIN) {
        return json({ error: "origin not allowed" }, 403);
      }
      const identity = await verifySessionCookie(request, env.AUTH_COOKIE_SECRET, now());
      return json(
        identity
          ? {
              authenticated: true,
              provider: identity.provider,
              display: identity.display,
              ...(identity.email ? { email: identity.email } : {}),
            }
          : { authenticated: false },
        200,
        origin === PUBLIC_APP_ORIGIN,
      );
    }

    if (url.pathname === "/auth/logout" && request.method === "POST") {
      const origin = request.headers.get("Origin");
      if (origin !== PUBLIC_APP_ORIGIN && origin !== PUBLIC_AUTH_ORIGIN) {
        return json({ error: "origin not allowed" }, 403);
      }
      const headers = new Headers({
        "Cache-Control": "no-store",
        "Set-Cookie": clearCookie(SESSION_COOKIE),
      });
      if (origin === PUBLIC_APP_ORIGIN) {
        headers.set("Access-Control-Allow-Credentials", "true");
        headers.set("Access-Control-Allow-Origin", PUBLIC_APP_ORIGIN);
        headers.set("Vary", "Origin");
      }
      return new Response(null, {
        status: 204,
        headers,
      });
    }

    const match = /^\/auth\/(github|google)(\/callback)?$/.exec(url.pathname);
    if (!match || request.method !== "GET") return json({ error: "not found" }, 404);
    const provider = match[1] as "github" | "google";
    const callback = Boolean(match[2]);
    if (
      provider === "github"
        ? !env.GITHUB_OAUTH_CLIENT_ID || !env.GITHUB_OAUTH_CLIENT_SECRET
        : !env.GOOGLE_OIDC_CLIENT_ID || !env.GOOGLE_OIDC_CLIENT_SECRET
    ) {
      return json({ error: "authentication is not configured" }, 500);
    }
    const transactionCookie = `${TRANSACTION_COOKIE_PREFIX}${provider}`;
    const redirectUri = `${url.origin}/auth/${provider}/callback`;

    if (!callback) {
      const destination = returnPath(url);
      if (!destination) return json({ error: "invalid return path" }, 400);
      const state = randomValue();
      const verifier = randomValue(48);
      const nonce = provider === "google" ? randomValue() : undefined;
      const transaction: TransactionClaims = {
        provider,
        state,
        verifier,
        ...(nonce ? { nonce } : {}),
        returnPath: destination,
        exp: now() + TRANSACTION_MAX_AGE_SECONDS * 1_000,
      };
      const authorization = new URL(
        provider === "github"
          ? "https://github.com/login/oauth/authorize"
          : "https://accounts.google.com/o/oauth2/v2/auth",
      );
      authorization.searchParams.set(
        "client_id",
        provider === "github" ? env.GITHUB_OAUTH_CLIENT_ID : env.GOOGLE_OIDC_CLIENT_ID,
      );
      authorization.searchParams.set("redirect_uri", redirectUri);
      authorization.searchParams.set("response_type", "code");
      authorization.searchParams.set("state", state);
      authorization.searchParams.set("code_challenge", await pkceChallenge(verifier));
      authorization.searchParams.set("code_challenge_method", "S256");
      if (provider === "google") {
        authorization.searchParams.set("nonce", nonce!);
        authorization.searchParams.set("scope", "openid email profile");
      }
      const response = new Response(null, {
        status: 302,
        headers: { Location: authorization.toString() },
      });
      response.headers.append(
        "Set-Cookie",
        cookie(transactionCookie, await signClaims(env.AUTH_COOKIE_SECRET, transaction), TRANSACTION_MAX_AGE_SECONDS),
      );
      response.headers.set("Cache-Control", "no-store");
      return response;
    }

    const transaction = await verifyClaims<TransactionClaims>(
      env.AUTH_COOKIE_SECRET,
      cookies(request).get(transactionCookie),
      now(),
    );
    if (
      !transaction ||
      !validTransaction(transaction, provider) ||
      url.searchParams.get("state") !== transaction.state
    ) {
      const response = json({ error: "invalid OAuth transaction" }, 400);
      response.headers.append("Set-Cookie", clearCookie(transactionCookie));
      return response;
    }
    if (!url.searchParams.get("code") || url.searchParams.has("error")) {
      const response = redirectAfterAuth(transaction.returnPath, "error");
      response.headers.append("Set-Cookie", clearCookie(transactionCookie));
      response.headers.set("Cache-Control", "no-store");
      return response;
    }

    let identity: SessionIdentity;
    try {
      if (provider === "github") {
        const tokenResponse = await fetcher("https://github.com/login/oauth/access_token", {
          method: "POST",
          headers: { Accept: "application/json", "Content-Type": "application/x-www-form-urlencoded" },
          body: new URLSearchParams({
            client_id: env.GITHUB_OAUTH_CLIENT_ID,
            client_secret: env.GITHUB_OAUTH_CLIENT_SECRET,
            code: url.searchParams.get("code")!,
            redirect_uri: redirectUri,
            code_verifier: transaction.verifier,
          }),
        });
        const token = safeText((await formResponse(tokenResponse)).access_token, 2_000);
        if (!token) throw new Error("GitHub access token missing");
        const userResponse = await fetcher("https://api.github.com/user", {
          headers: {
            Accept: "application/vnd.github+json",
            Authorization: `Bearer ${token}`,
            "User-Agent": "phux-shell",
          },
        });
        const user = await formResponse(userResponse);
        const login = safeText(user.login);
        const createdAt = Date.parse(safeText(user.created_at) ?? "");
        if (
          !Number.isSafeInteger(user.id) ||
          Number(user.id) <= 0 ||
          !login ||
          !Number.isFinite(createdAt) ||
          createdAt > now() - GITHUB_MIN_ACCOUNT_AGE_MS
        ) {
          throw new Error("Invalid GitHub identity");
        }
        identity = { principal: `github:${user.id}`, provider, display: login };
      } else {
        const tokenResponse = await fetcher("https://oauth2.googleapis.com/token", {
          method: "POST",
          headers: { "Content-Type": "application/x-www-form-urlencoded" },
          body: new URLSearchParams({
            client_id: env.GOOGLE_OIDC_CLIENT_ID,
            client_secret: env.GOOGLE_OIDC_CLIENT_SECRET,
            code: url.searchParams.get("code")!,
            grant_type: "authorization_code",
            redirect_uri: redirectUri,
            code_verifier: transaction.verifier,
          }),
        });
        const idToken = safeText((await formResponse(tokenResponse)).id_token, 20_000);
        if (!idToken) throw new Error("Google ID token missing");
        const jwksResponse = await fetcher("https://www.googleapis.com/oauth2/v3/certs");
        const jwks = await formResponse(jwksResponse) as unknown as JSONWebKeySet;
        const verified = await jwtVerify(idToken, createLocalJWKSet(jwks), {
          issuer: ["https://accounts.google.com", "accounts.google.com"],
          audience: env.GOOGLE_OIDC_CLIENT_ID,
          currentDate: new Date(now()),
        });
        const sub = safeText(verified.payload.sub);
        const email = safeText(verified.payload.email);
        const display = safeText(verified.payload.name) ?? email;
        if (!sub || !email || !display || verified.payload.email_verified !== true || verified.payload.nonce !== transaction.nonce) {
          throw new Error("Invalid Google identity");
        }
        identity = { principal: `google:${sub}`, provider, display, email };
      }
    } catch {
      const response = redirectAfterAuth(transaction.returnPath, "error");
      response.headers.append("Set-Cookie", clearCookie(transactionCookie));
      response.headers.set("Cache-Control", "no-store");
      return response;
    }

    const session = await signClaims(env.AUTH_COOKIE_SECRET, {
      ...identity,
      exp: now() + SESSION_MAX_AGE_SECONDS * 1_000,
    } satisfies SessionClaims);
    const response = redirectAfterAuth(transaction.returnPath, "success");
    response.headers.append("Set-Cookie", cookie(SESSION_COOKIE, session, SESSION_MAX_AGE_SECONDS));
    response.headers.append("Set-Cookie", clearCookie(transactionCookie));
    response.headers.set("Cache-Control", "no-store");
    return response;
  };
}

export const handleAuthRequest = createAuthRequestHandler();
