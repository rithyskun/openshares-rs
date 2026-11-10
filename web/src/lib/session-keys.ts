/**
 * Client-side store for the per-session request-signing key.
 *
 * The key lives in sessionStorage (`os_sigkey`): tab-scoped, gone when the tab
 * closes, and recoverable after a page refresh — the reference keeps it in a
 * Pinia store only, which would force re-login on every reload. Either way the
 * key is reachable from page JS, so it buys no extra protection against XSS —
 * it exists so a stolen *request* can't be replayed with a different body.
 *
 * When the key is missing (fresh tab, expired storage) it's lazily re-issued
 * via GET /api/me — a plain fetch, since GETs aren't signed.
 */

const STORE_KEY = "os_sigkey";
let cached: string | null | undefined; // undefined = not probed yet

export function setSigningKey(key: string | null): void {
  cached = key;
  if (typeof window === "undefined") return;
  try {
    if (key) sessionStorage.setItem(STORE_KEY, key);
    else sessionStorage.removeItem(STORE_KEY);
  } catch {
    /* storage disabled — in-memory copy still works */
  }
}

export function clearSigningKey(): void {
  setSigningKey(null);
}

export async function signingKey(): Promise<string | null> {
  if (typeof window === "undefined") return null;
  if (cached !== undefined) return cached;
  try {
    cached = sessionStorage.getItem(STORE_KEY);
  } catch {
    cached = null;
  }
  if (cached) return cached;
  try {
    const res = await fetch("/api/me");
    if (res.ok) {
      const me = (await res.json()) as { signingKey?: string | null };
      setSigningKey(me.signingKey ?? null);
    }
  } catch {
    /* offline / server down — stay unsigned */
  }
  return cached ?? null;
}
