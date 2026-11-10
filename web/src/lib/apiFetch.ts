"use client";

/**
 * Same-origin API client — the browser side of the middleware protections.
 *
 * - Attaches `x-csrf-token` (read from the JS-readable XSRF-TOKEN cookie the
 *   middleware issues) on POST/PUT/PATCH/DELETE. Foreign-origin code can't
 *   read the cookie, so the matching pair proves the call came from our JS.
 * - On 401 + `x-session-expired` (or code TOKEN_EXPIRED) the session is dead —
 *   redirects to /login instead of leaving stale UI.
 * - apiJson() unwraps the { error } envelope into a typed ApiError.
 */

import { requiresSignature, signRequest } from "./signing";
import { signingKey } from "./session-keys";

const MUTATING = new Set(["POST", "PUT", "PATCH", "DELETE"]);

export class ApiError extends Error {
  readonly status?: number;
  readonly code?: string;
  constructor(message: string, status?: number, code?: string) {
    super(message);
    this.name = "ApiError";
    this.status = status;
    this.code = code;
  }
}

/** Current XSRF-TOKEN cookie value — for non-fetch callers (e.g. XHR uploads). */
export function csrfToken(): string | undefined {
  if (typeof document === "undefined") return undefined;
  return document.cookie.match(/XSRF-TOKEN=([^;]+)/)?.[1];
}

export async function apiFetch(url: string, init: RequestInit = {}): Promise<Response> {
  const method = (init.method ?? "GET").toUpperCase();
  const headers = new Headers(init.headers);
  if (MUTATING.has(method)) {
    const token = csrfToken();
    if (token) headers.set("x-csrf-token", token);

    // HMAC request signature — tamper/replay protection on top of CSRF.
    // Skipped when the server hasn't issued a key (signing disabled) and on
    // exempt paths (auth bootstrap, multipart uploads — see signing.ts).
    const pathname = url.split("?")[0];
    if (requiresSignature(method, pathname)) {
      const key = await signingKey();
      if (key) {
        const body = typeof init.body === "string" ? init.body : "";
        const sig = await signRequest(key, method, pathname, body);
        headers.set("x-signature", sig["x-signature"]);
        headers.set("x-signature-ts", sig["x-signature-ts"]);
        headers.set("x-signature-nonce", sig["x-signature-nonce"]);
      }
    }
  }
  const res = await fetch(url, { ...init, headers });

  if (res.status === 401 && res.headers.get("x-session-expired") === "true") {
    // Dead session — bounce to login (preserve the attempted path).
    const back = encodeURIComponent(location.pathname + location.search);
    location.href = `/login?next=${back}`;
    throw new ApiError("Session expired.", 401, "TOKEN_EXPIRED");
  }
  return res;
}

/** Parse a JSON body or throw an ApiError carrying the server's message. */
export async function apiJson<T>(res: Response): Promise<T> {
  const body = (await res.json().catch(() => ({}))) as { error?: string; code?: string };
  if (!res.ok) throw new ApiError(body.error ?? `Request failed (${res.status}).`, res.status, body.code);
  return body as T;
}
