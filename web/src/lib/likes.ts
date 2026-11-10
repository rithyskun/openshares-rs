import { cookies } from "next/headers";

/**
 * Public-share likes — one per visitor, deduped by an HttpOnly cookie so the
 * count reflects distinct browsers rather than raw taps. The cookie is the
 * only record (no per-liker rows): clearing it allows another like, which is
 * acceptable at personal-vault scale.
 *
 * The Rust API sets the cookie on like (`oslk_*`, same names) — only the
 * read side (`hasLiked` for initial button state) still lives here.
 */

export type LikeKind = "share" | "album";

export function likeCookieName(kind: LikeKind, token: string): string {
  return `oslk_${kind === "share" ? "s" : "a"}_${token}`;
}

/** True when this browser already liked the token. */
export async function hasLiked(kind: LikeKind, token: string): Promise<boolean> {
  return !!(await cookies()).get(likeCookieName(kind, token))?.value;
}
