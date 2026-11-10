/**
 * Instance metadata for SSR — the `siteName()` / `appUrl()` /
 * `getBotUsername()` half of the old settings/env/bot modules.
 *
 * These were DB/settings reads in the Next.js app; in the split
 * deployment only the Rust API can answer them. `GET /api/meta` is the
 * proposed endpoint (`{siteName, botUsername, appUrl}`) — until it
 * exists every helper falls back to build/env config so pages still
 * render: `NEXT_PUBLIC_SITE_NAME` (default "OpenShares") and
 * `NEXT_PUBLIC_APP_URL` (default http://localhost:3000, only used for
 * absolute og:image URLs).
 *
 * Results are cached for 60s like the old `siteName()` module cache —
 * every page render + generateMetadata calls these, and a 404 fallback
 * shouldn't cost a round-trip per render either.
 */

import { apiServerFetch } from "./api-server";

export type ApiMeta = {
  siteName?: string | null;
  botUsername?: string | null;
  appUrl?: string | null;
};

const FALLBACK_SITE_NAME = process.env.NEXT_PUBLIC_SITE_NAME?.trim() || "OpenShares";
const FALLBACK_APP_URL = (process.env.NEXT_PUBLIC_APP_URL ?? "http://localhost:3000").replace(/\/+$/, "");

const TTL_MS = 60_000;
let metaCache: { value: ApiMeta; at: number } | null = null;

/** GET /api/meta, tolerant of 404/unreachable — returns {} on any failure. */
async function apiMeta(): Promise<ApiMeta> {
  if (metaCache && Date.now() - metaCache.at < TTL_MS) return metaCache.value;
  let value: ApiMeta = {};
  try {
    const res = await apiServerFetch("/api/meta");
    if (res.ok) value = (await res.json()) as ApiMeta;
  } catch {
    /* API down or endpoint not ported yet — env fallbacks still render */
  }
  metaCache = { value, at: Date.now() };
  return value;
}

/** Display name for the whole app — navbar, metadata, public pages. */
export async function siteName(): Promise<string> {
  const name = (await apiMeta()).siteName?.trim();
  return name || FALLBACK_SITE_NAME;
}

/** Public origin for absolute URLs (og:image etc.) — no trailing slash. */
export async function appUrl(): Promise<string> {
  const url = (await apiMeta()).appUrl?.trim();
  return url ? url.replace(/\/+$/, "") : FALLBACK_APP_URL;
}

/** Telegram bot username for deep links, or null when unknown/offline. */
export async function botUsername(): Promise<string | null> {
  return (await apiMeta()).botUsername ?? null;
}
