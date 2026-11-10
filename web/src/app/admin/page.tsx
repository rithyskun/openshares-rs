import { apiServerFetch, apiServerJson, requireApiPerm } from "@/lib/api-server";
import { siteName } from "@/lib/meta";
import { AdminConsole, type AdminUser, type RoleWithPerms, type PermGroup, type AuditRow, type CacheInfo, type VaultStats } from "./admin-console";

export const dynamic = "force-dynamic";

/**
 * Optional-permission fetch — `/api/admin/access` and `/api/admin/cache`
 * gate on `settings:manage`, a level above the page's `admin:access`.
 * Admins without it get `fallback` (the old page read the settings
 * directly; the split can't bypass the route's own RBAC).
 */
async function adminJson<T>(path: string, fallback: T): Promise<T> {
  const res = await apiServerFetch(path);
  if (!res.ok) return fallback;
  return res.json() as Promise<T>;
}

/** Zeroed stats — shown until the `GET /api/admin/vault` endpoint lands. */
const EMPTY_VAULT: VaultStats = {
  media: { count: 0, bytes: 0 },
  byKind: {},
  byAvailability: {},
  albums: { count: 0, publicCount: 0 },
  shares: { count: 0, activeLinks: 0 },
  users: { count: 0, active: 0 },
  topOwners: [],
};

export default async function AdminPage() {
  await requireApiPerm("admin:access");

  const [usersBody, rolesBody, permsBody, activityBody, allowlistBody, cache, vault, name] =
    await Promise.all([
      apiServerJson<{ users: AdminUser[] }>("/api/admin/users"),
      apiServerJson<{ roles: RoleWithPerms[] }>("/api/admin/roles"),
      apiServerJson<{ permissions: PermGroup[] }>("/api/admin/permissions"),
      apiServerJson<{ activity: AuditRow[] }>("/api/admin/activity"),
      adminJson<{ telegram_ids: string[] }>("/api/admin/access", { telegram_ids: [] }),
      adminJson<CacheInfo>("/api/admin/cache", { ttlDays: 7, last: null }),
      adminJson<VaultStats>("/api/admin/vault", EMPTY_VAULT),
      siteName(),
    ]);

  return (
    <div className="space-y-6">
      <header>
        <h1 className="text-2xl font-bold text-white">Admin</h1>
        <p className="mt-1 text-sm text-slate-400">
          Users, roles, and permissions for this vault.
        </p>
      </header>
      <AdminConsole
        users={usersBody.users}
        roles={rolesBody.roles}
        permGroups={permsBody.permissions}
        allowlist={allowlistBody.telegram_ids}
        activity={activityBody.activity}
        cache={cache}
        vault={vault}
        siteName={name}
      />
    </div>
  );
}
