"use client";

/**
 * Client-side mirror of the server RBAC check — the React port of the
 * reference's `v-can` directive / usePermissions composable. Permissions are
 * computed server-side in the root layout (permissionsOf) and passed down as
 * plain props, so they're correct on first paint.
 *
 * This is a UX gate only: it hides controls the signed-in user can't use so
 * they never see a dead-end 403. Every mutation stays authoritatively checked
 * server-side — never treat `can()` here as enforcement.
 */
import { createContext, useContext, useMemo, type ReactNode } from "react";

interface PermissionsContextValue {
  userId: number | null;
  permissions: ReadonlySet<string>;
}

const PermissionsContext = createContext<PermissionsContextValue>({
  userId: null,
  permissions: new Set<string>(),
});

export function PermissionsProvider({
  userId,
  permissions,
  children,
}: {
  userId: number | null;
  permissions: string[];
  children: ReactNode;
}) {
  const value = useMemo(
    () => ({ userId, permissions: new Set(permissions) as ReadonlySet<string> }),
    [userId, permissions]
  );
  return <PermissionsContext.Provider value={value}>{children}</PermissionsContext.Provider>;
}

/**
 * `can(key, ownerId)` mirrors the server's `can(userId, key, ownerId)`:
 * `*` or the exact key or `:any` always passes; `:own` passes only when
 * `ownerId === userId`. `canAny(key)` also accepts `:own` with no owner —
 * for chrome-level "could this user ever do this" checks (batch action bars).
 */
export function usePermissions() {
  const { userId, permissions } = useContext(PermissionsContext);
  return useMemo(
    () => ({
      userId,
      can: (key: string, ownerId?: number | null): boolean =>
        permissions.has("*") ||
        permissions.has(key) ||
        permissions.has(`${key}:any`) ||
        (ownerId != null && ownerId === userId && permissions.has(`${key}:own`)),
      canAny: (key: string): boolean =>
        permissions.has("*") || permissions.has(key) || permissions.has(`${key}:any`) || permissions.has(`${key}:own`),
    }),
    [userId, permissions]
  );
}
