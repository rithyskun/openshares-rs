/**
 * RBAC permission catalog — every key a code path can check.
 * Grammar: `resource:action[:scope]`; scope defaults to `any`, `own` is explicit.
 * `*` is the only wildcard and belongs to the admin role only.
 *
 * Client-safe constants only — the enforcement half (`can`, `requirePerm`,
 * `apiGuard`, the admin invariants) moved into the Rust API. SSR pages check
 * permissions off the `GET /api/me` payload (see `canPerm`/`requireApiPerm`
 * in `api-server.ts`); client components use `usePermissions()`.
 */
export const PERMISSIONS = [
  "media:view",
  "media:upload",
  "media:edit:own",
  "media:edit:any",
  "media:delete:own",
  "media:delete:any",
  "album:create",
  "album:edit:own",
  "album:edit:any",
  "album:delete:own",
  "album:delete:any",
  "share:link",
  "share:album",
  "share:friend",
  "friend:invite",
  "friend:remove",
  "admin:access",
  "user:manage",
  "role:manage",
  "settings:manage",
] as const;

export type PermissionKey = (typeof PERMISSIONS)[number] | "*";

const VALID_KEYS = new Set<string>([...PERMISSIONS, "*"]);

export function isValidPermissionKey(key: string): key is PermissionKey {
  return VALID_KEYS.has(key);
}
