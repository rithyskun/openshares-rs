/**
 * SSR data access — the cutover half of the old `@/db` reads.
 *
 * Server components and generateMetadata call the Rust API over the
 * loopback (`API_INTERNAL_URL`), forwarding the request's cookies so
 * session auth resolves exactly like the browser's `/api/*` calls.
 * GET-only — SSR never mutates (mutations stay client-side via
 * apiFetch, which CSRF-protects and HMAC-signs).
 *
 * Error mapping mirrors the old `requireUser`/`requirePerm` page
 * contract: no session → redirect to /login; dead session →
 * unauthorized(); missing permission → forbidden(); missing row →
 * notFound(). With `experimental.authInterrupts` these throw real
 * status pages.
 */

import { cookies, headers } from "next/headers";
import { redirect } from "next/navigation";
import { forbidden, notFound, unauthorized } from "next/navigation";

const API = process.env.API_INTERNAL_URL ?? "http://127.0.0.1:8080";

/** Base fetch: API path → Response, cookies + tracing forwarded. */
export async function apiServerFetch(
  path: string,
  init?: RequestInit
): Promise<Response> {
  const [cookieStore, headerStore] = await Promise.all([cookies(), headers()]);
  const headersOut = new Headers(init?.headers);
  headersOut.set("cookie", cookieStore.toString());
  headersOut.set("x-correlation-id", headerStore.get("x-correlation-id") ?? crypto.randomUUID());
  headersOut.set("x-forwarded-proto", headerStore.get("x-forwarded-proto") ?? "http");
  return fetch(`${API}${path}`, {
    cache: "no-store",
    ...init,
    headers: headersOut,
  });
}

/** fetch + parse JSON, or throw the mapped error page (null when 404 and allow404). */
export async function apiServerJson<T>(path: string, opts?: { allow404?: boolean }): Promise<T> {
  const res = await apiServerFetch(path);
  if (res.status === 404 && opts?.allow404) notFound();
  if (res.status === 403) forbidden();
  if (res.status === 401) {
    if (res.headers.get("x-session-expired")) unauthorized();
    redirect("/login");
  }
  if (!res.ok) throw new Error(`apiServerFetch ${path} → ${res.status}`);
  return res.json() as Promise<T>;
}

/** requireUser equivalent — session user or the 401/login outcome. */
export async function requireApiUser<T = SessionUser>(): Promise<T> {
  return apiServerJson<T>("/api/me");
}

/**
 * requirePerm equivalent — fetch /api/me, check the permission set, and
 * hand back the session user (the old requirePerm returned SessionUser).
 */
export async function requireApiPerm<T = SessionUser>(key: string): Promise<T> {
  const me = await apiServerJson<T & { permissions?: string[] }>("/api/me");
  if (!canPerm(me.permissions ?? [], -1, key)) forbidden();
  return me;
}

/**
 * getSessionUser equivalent for pages where signed-out is a valid state
 * (layout, landing, login): the /api/me payload, or null on any 401 —
 * a dead cookie renders logged-out chrome, same as the old null return.
 */
export async function maybeApiUser<T = SessionUser>(): Promise<T | null> {
  const res = await apiServerFetch("/api/me");
  if (res.status === 401) return null;
  if (!res.ok) throw new Error(`apiServerFetch /api/me → ${res.status}`);
  return res.json() as Promise<T>;
}

/**
 * Server-side mirror of `can(userId, key, ownerId)` and the client
 * `usePermissions().can` — `*` or the exact key or `:any` always passes;
 * `:own` passes only when `ownerId === userId`.
 */
export function canPerm(
  permissions: Iterable<string>,
  userId: number,
  key: string,
  ownerId?: number | null
): boolean {
  const perms = permissions instanceof Set ? permissions : new Set(permissions);
  return (
    perms.has("*") ||
    perms.has(key) ||
    perms.has(`${key}:any`) ||
    (ownerId != null && ownerId === userId && perms.has(`${key}:own`))
  );
}

/**
 * Minimal session shape — keep in sync with GET /api/me, which returns
 * `{id, name, email, telegram_id, permissions, signingKey}`.
 * `sessionCreatedAt` has no API field yet — always undefined until the
 * `/api/me` response grows a `sessionCreatedAt` (sessions.created_at).
 */
export type SessionUser = {
  id: number;
  email: string | null;
  name: string;
  telegram_id: string | null;
  /** unix seconds — sessions.created_at ("signed in at"); API gap. */
  sessionCreatedAt?: number | null;
  permissions?: string[];
  signingKey?: string | null;
};
