// Short-lived session token (HMAC-SHA256). Minted by the Worker after it has
// (a) passed rate-limit and (b) reserved a global concurrency slot, then handed
// to the SessionDO which verifies it before running the session. This makes the
// DO endpoint unspawnable except through the Worker's front door, even though
// the DO is internal-only — defense in depth.

const enc = new TextEncoder();

export interface SessionClaims {
  sid: string; // session id (also the DO name)
  exp: number; // epoch ms expiry
}

function b64urlEncode(bytes: Uint8Array): string {
  let s = "";
  for (const b of bytes) s += String.fromCharCode(b);
  return btoa(s).replace(/\+/g, "-").replace(/\//g, "_").replace(/=+$/, "");
}

function b64urlDecode(str: string): Uint8Array {
  const pad = str.length % 4 === 0 ? "" : "=".repeat(4 - (str.length % 4));
  const b64 = str.replace(/-/g, "+").replace(/_/g, "/") + pad;
  const bin = atob(b64);
  const out = new Uint8Array(bin.length);
  for (let i = 0; i < bin.length; i++) out[i] = bin.charCodeAt(i);
  return out;
}

async function importKey(secret: string): Promise<CryptoKey> {
  return crypto.subtle.importKey(
    "raw",
    enc.encode(secret),
    { name: "HMAC", hash: "SHA-256" },
    false,
    ["sign", "verify"],
  );
}

// Constant-time comparison to avoid signature-timing oracles.
function timingSafeEqual(a: Uint8Array, b: Uint8Array): boolean {
  if (a.length !== b.length) return false;
  let diff = 0;
  for (let i = 0; i < a.length; i++) diff |= a[i] ^ b[i];
  return diff === 0;
}

export async function mintToken(secret: string, claims: SessionClaims): Promise<string> {
  const payload = b64urlEncode(enc.encode(JSON.stringify(claims)));
  const key = await importKey(secret);
  const sig = new Uint8Array(await crypto.subtle.sign("HMAC", key, enc.encode(payload)));
  return `${payload}.${b64urlEncode(sig)}`;
}

export async function verifyToken(secret: string, token: string): Promise<SessionClaims | null> {
  const dot = token.indexOf(".");
  if (dot < 0) return null;
  const payload = token.slice(0, dot);
  const sigPart = token.slice(dot + 1);
  const key = await importKey(secret);
  const expected = new Uint8Array(await crypto.subtle.sign("HMAC", key, enc.encode(payload)));
  let provided: Uint8Array;
  try {
    provided = b64urlDecode(sigPart);
  } catch {
    return null;
  }
  if (!timingSafeEqual(expected, provided)) return null;
  let claims: SessionClaims;
  try {
    claims = JSON.parse(new TextDecoder().decode(b64urlDecode(payload)));
  } catch {
    return null;
  }
  if (typeof claims.exp !== "number" || Date.now() > claims.exp) return null;
  if (typeof claims.sid !== "string" || claims.sid.length === 0) return null;
  return claims;
}
