/**
 * TestFlight access requests for phux-mobile.
 *
 * POST /api/beta adds the requester to an external TestFlight group through
 * the App Store Connect API; Apple then e-mails the invite itself. The key is
 * the same team API key the phux-mobile release lane uploads with, held here
 * as Worker secrets. Without them the request is still recorded through the
 * member pipeline (host/analytics.ts) for a manual invite.
 */
import { SignJWT, importPKCS8 } from "jose";

export interface TestFlightEnv {
  /** App Store Connect team API key id (the `AuthKey_<id>.p8` suffix). */
  ASC_KEY_ID?: string;
  /** App Store Connect issuer id (Users and Access → Integrations). */
  ASC_ISSUER_ID?: string;
  /** Contents of the `.p8` private key (PKCS#8 PEM). */
  ASC_PRIVATE_KEY?: string;
  /** Optional: pin the external group instead of resolving it by name. */
  ASC_BETA_GROUP_ID?: string;
  /** Optional: external group name to resolve; first external group if unset. */
  ASC_BETA_GROUP?: string;
}

export type InviteOutcome = "invited" | "unconfigured" | "failed";
export type Fetch = (input: Request) => Promise<Response>;

const API = "https://api.appstoreconnect.apple.com/v1";
const BUNDLE_ID = "dev.phux.mobile";
const TOKEN_TTL_SECONDS = 15 * 60;
const REQUEST_TIMEOUT_MS = 8_000;

const tokens = new Map<string, { token: string; expiresAt: number }>();
const groups = new Map<string, string>();

class AscError extends Error {
  constructor(readonly status: number, path: string) {
    super(`App Store Connect ${path} returned ${status}`);
  }
}

/** Add `email` to the external TestFlight group. Never throws. */
export async function inviteTester(
  env: TestFlightEnv,
  email: string,
  fetchImpl: Fetch = (request) => fetch(request),
): Promise<InviteOutcome> {
  if (!env.ASC_KEY_ID || !env.ASC_ISSUER_ID || !env.ASC_PRIVATE_KEY) {
    return "unconfigured";
  }
  try {
    const asc = client(env, fetchImpl);
    const groupId = env.ASC_BETA_GROUP_ID || (await resolveGroup(env, asc));
    await addTester(asc, groupId, email);
    return "invited";
  } catch (error) {
    console.error("testflight invite failed", error);
    return "failed";
  }
}

type Asc = (path: string, init?: { method?: string; body?: unknown }) => Promise<Response>;

function client(env: TestFlightEnv, fetchImpl: Fetch): Asc {
  return async (path, init = {}) => {
    const response = await fetchImpl(
      new Request(`${API}${path}`, {
        method: init.method ?? "GET",
        headers: {
          authorization: `Bearer ${await token(env)}`,
          "content-type": "application/json",
        },
        body: init.body === undefined ? undefined : JSON.stringify(init.body),
        signal: AbortSignal.timeout(REQUEST_TIMEOUT_MS),
      }),
    );
    // 409 is a meaningful answer for tester creation; callers handle it.
    if (!response.ok && response.status !== 409) {
      throw new AscError(response.status, path);
    }
    return response;
  };
}

async function token(env: TestFlightEnv): Promise<string> {
  const cacheKey = `${env.ASC_ISSUER_ID}:${env.ASC_KEY_ID}`;
  const now = Math.floor(Date.now() / 1000);
  const cached = tokens.get(cacheKey);
  if (cached && cached.expiresAt - 60 > now) return cached.token;
  // Secrets pasted through a single-line UI may carry literal "\n" escapes.
  const pem = env.ASC_PRIVATE_KEY!.replace(/\\n/g, "\n");
  const key = await importPKCS8(pem, "ES256");
  const expiresAt = now + TOKEN_TTL_SECONDS;
  const signed = await new SignJWT({})
    .setProtectedHeader({ alg: "ES256", kid: env.ASC_KEY_ID!, typ: "JWT" })
    .setIssuer(env.ASC_ISSUER_ID!)
    .setIssuedAt(now)
    .setExpirationTime(expiresAt)
    .setAudience("appstoreconnect-v1")
    .sign(key);
  tokens.set(cacheKey, { token: signed, expiresAt });
  return signed;
}

interface Resource<A> {
  id: string;
  attributes: A;
}

async function resolveGroup(env: TestFlightEnv, asc: Asc): Promise<string> {
  const cacheKey = `${env.ASC_ISSUER_ID}:${env.ASC_BETA_GROUP ?? ""}`;
  const cached = groups.get(cacheKey);
  if (cached) return cached;
  const apps = (await (
    await asc(`/apps?filter[bundleId]=${BUNDLE_ID}&fields[apps]=bundleId&limit=1`)
  ).json()) as { data: Resource<unknown>[] };
  const appId = apps.data[0]?.id;
  if (!appId) throw new Error(`no App Store Connect app for ${BUNDLE_ID}`);
  const list = (await (
    await asc(`/apps/${appId}/betaGroups?fields[betaGroups]=name,isInternalGroup&limit=200`)
  ).json()) as { data: Resource<{ name: string; isInternalGroup: boolean }>[] };
  const group = list.data.find(
    (candidate) =>
      !candidate.attributes.isInternalGroup &&
      (!env.ASC_BETA_GROUP || candidate.attributes.name === env.ASC_BETA_GROUP),
  );
  if (!group) {
    throw new Error(`no external TestFlight group${env.ASC_BETA_GROUP ? ` named ${env.ASC_BETA_GROUP}` : ""}`);
  }
  groups.set(cacheKey, group.id);
  return group.id;
}

async function addTester(asc: Asc, groupId: string, email: string): Promise<void> {
  const created = await asc("/betaTesters", {
    method: "POST",
    body: {
      data: {
        type: "betaTesters",
        attributes: { email },
        relationships: {
          betaGroups: { data: [{ type: "betaGroups", id: groupId }] },
        },
      },
    },
  });
  if (created.status !== 409) return;
  // The address is already a tester of this team (another group, or a
  // repeat request): attach the existing tester to the group instead.
  const found = (await (
    await asc(`/betaTesters?filter[email]=${encodeURIComponent(email)}&fields[betaTesters]=email&limit=1`)
  ).json()) as { data: Resource<unknown>[] };
  const testerId = found.data[0]?.id;
  if (!testerId) throw new AscError(409, "/betaTesters");
  const attached = await asc(`/betaGroups/${groupId}/relationships/betaTesters`, {
    method: "POST",
    body: { data: [{ type: "betaTesters", id: testerId }] },
  });
  if (attached.status === 409) throw new AscError(409, "/betaGroups/relationships");
}
