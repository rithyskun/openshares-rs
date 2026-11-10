/**
 * HMAC-SHA256 request signing for mutating API calls — port of the accounting
 * frontend's requestSignature.ts.
 *
 * The per-session signing key is derived server-side as
 * `base64(HMAC-SHA256(REQUEST_SIGNING_SECRET, "signing:" + sid))` — stateless,
 * no extra storage: any request carrying a valid `os_session` cookie can have
 * its signature checked by recomputing the key from the session id. The key is
 * handed to the browser at login (and re-issued by /api/me); the client holds
 * it in sessionStorage and signs mutating requests with it.
 *
 * Signature format (same as the reference):
 *   sig = HMAC-SHA256(key, method + "\n" + path + "\n" + sha256(body) + "\n" + ts + "\n" + nonce)
 * sent as x-signature / x-signature-ts / x-signature-nonce. Timestamps more
 * than ±5 minutes off are rejected, bounding the replay window.
 *
 * Isomorphic: WebCrypto only — works in the browser, workerd, and plain Node.
 *
 * This copy keeps the client sign path plus the pure verify/derive helpers
 * (unit-tested in `signing.test.ts`). The env-reading server guard
 * (`verifySignedBody`, `signingKeyFor`, `signingEnabled`, `parseJsonBody`)
 * lives in the Rust API now — there are no route handlers here anymore.
 */
export interface SignatureHeaders {
  "x-signature": string;
  "x-signature-ts": string;
  "x-signature-nonce": string;
}

export const SIGN_MAX_AGE_MS = 5 * 60 * 1000;

const MUTATING = new Set(["POST", "PUT", "PATCH", "DELETE"]);

/**
 * Mutating paths that are NEVER signed: pre-session auth endpoints (no key
 * exists yet), server-to-server bot traffic, and multipart uploads — the
 * XHR/FormData boundary isn't knowable before send, so the wire bytes can't
 * be hashed. Uploads stay protected by CSRF + same-origin checks instead.
 */
const SIGN_EXEMPT = [/^\/api\/auth\//, /^\/api\/bot\//, /^\/api\/albums\/[^/]+\/files$/];

export function requiresSignature(method: string, pathname: string): boolean {
  if (!MUTATING.has(method.toUpperCase())) return false;
  const path = pathname.split("?")[0];
  if (!path.startsWith("/api/")) return false;
  return !SIGN_EXEMPT.some((re) => re.test(path));
}

function b64encode(bytes: Uint8Array | ArrayBuffer): string {
  const arr = bytes instanceof Uint8Array ? bytes : new Uint8Array(bytes);
  let binary = "";
  for (const b of arr) binary += String.fromCharCode(b);
  return btoa(binary);
}

function b64decode(b64: string): Uint8Array {
  return Uint8Array.from(atob(b64), (c) => c.charCodeAt(0));
}

async function hmacSha256(keyBytes: Uint8Array, message: string): Promise<ArrayBuffer> {
  const key = await crypto.subtle.importKey(
    "raw",
    keyBytes as BufferSource,
    { name: "HMAC", hash: "SHA-256" },
    false,
    ["sign"]
  );
  return crypto.subtle.sign("HMAC", key, new TextEncoder().encode(message));
}

async function sha256Hex(data: string): Promise<string> {
  const digest = await crypto.subtle.digest("SHA-256", new TextEncoder().encode(data));
  return Array.from(new Uint8Array(digest))
    .map((b) => b.toString(16).padStart(2, "0"))
    .join("");
}

function canonicalMessage(method: string, path: string, bodyHash: string, ts: string, nonce: string): string {
  return `${method.toUpperCase()}\n${path}\n${bodyHash}\n${ts}\n${nonce}`;
}

/** Client side: compute the signature headers for one request. */
export async function signRequest(
  signingKeyB64: string,
  method: string,
  pathname: string,
  body: string
): Promise<SignatureHeaders> {
  const ts = String(Date.now());
  const nonce = crypto.randomUUID();
  const bodyHash = await sha256Hex(body);
  const sig = await hmacSha256(b64decode(signingKeyB64), canonicalMessage(method, pathname, bodyHash, ts, nonce));
  return { "x-signature": b64encode(sig), "x-signature-ts": ts, "x-signature-nonce": nonce };
}

/** Server side: derive the per-session signing key (stateless — no table). */
export async function deriveSigningKey(secret: string, sid: string): Promise<string> {
  return b64encode(await hmacSha256(new TextEncoder().encode(secret), `signing:${sid}`));
}

/** Constant-time byte compare (isomorphic — no node:crypto dependency). */
function safeEq(a: Uint8Array, b: Uint8Array): boolean {
  if (a.length !== b.length) return false;
  let diff = 0;
  for (let i = 0; i < a.length; i++) diff |= a[i] ^ b[i];
  return diff === 0;
}

export type SignatureVerdict = "ok" | "missing" | "stale" | "bad";

/** Server side: verify the signature headers against the raw request body. */
export async function verifySignature(
  headers: { signature: string | null; ts: string | null; nonce: string | null },
  signingKeyB64: string,
  method: string,
  pathname: string,
  rawBody: string
): Promise<SignatureVerdict> {
  const { signature, ts, nonce } = headers;
  if (!signature || !ts || !nonce) return "missing";
  const tsNum = Number(ts);
  if (!Number.isFinite(tsNum) || Math.abs(Date.now() - tsNum) > SIGN_MAX_AGE_MS) return "stale";
  const bodyHash = await sha256Hex(rawBody);
  const expected = await hmacSha256(
    b64decode(signingKeyB64),
    canonicalMessage(method, pathname, bodyHash, ts, nonce)
  );
  let given: Uint8Array;
  try {
    given = b64decode(signature);
  } catch {
    return "bad";
  }
  return safeEq(given, new Uint8Array(expected)) ? "ok" : "bad";
}
