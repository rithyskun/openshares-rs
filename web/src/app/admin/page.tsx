import { apiServerFetch, apiServerJson, requireApiPerm } from "@/lib/api-server";
import { siteName } from "@/lib/meta";
import { AdminConsole, type AdminUser, type RoleWithPerms, type PermGroup, type AuditRow, type CacheInfo, type VaultStats } from "./admin-console";
import { ADMIN_TABS, type AdminTab } from "./tabs";

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

/**
 * Tab-scoped fetch — `/admin?tab=<t>` loads only the calls the active
 * tab renders. `users` also needs `roles` (its drawer's role toggles)
 * and `roles` needs `permGroups` (its drawer's permission checklist);
 * every other tab is a single call.
 */
export default async function AdminPage({
  searchParams,
}: {
  searchParams: Promise<{ tab?: string }>;
}) {
  await requireApiPerm("admin:access");
  const { tab: raw } = await searchParams;
  const tab = (ADMIN_TABS as readonly string[]).includes(raw ?? "")
    ? (raw as AdminTab)
    : "users";

  const data: {
    users?: AdminUser[];
    roles?: RoleWithPerms[];
    permGroups?: PermGroup[];
    allowlist?: string[];
    activity?: AuditRow[];
    cache?: CacheInfo;
    vault?: VaultStats;
    siteName?: string;
  } = {};

  switch (tab) {
    case "users": {
      const [usersBody, rolesBody] = await Promise.all([
        apiServerJson<{ users: AdminUser[] }>("/api/admin/users"),
        apiServerJson<{ roles: RoleWithPerms[] }>("/api/admin/roles"),
      ]);
      data.users = usersBody.users;
      data.roles = rolesBody.roles;
      break;
    }
    case "roles": {
      const [rolesBody, permsBody] = await Promise.all([
        apiServerJson<{ roles: RoleWithPerms[] }>("/api/admin/roles"),
        apiServerJson<{ permissions: PermGroup[] }>("/api/admin/permissions"),
      ]);
      data.roles = rolesBody.roles;
      data.permGroups = permsBody.permissions;
      break;
    }
    case "permissions":
      data.permGroups = (
        await apiServerJson<{ permissions: PermGroup[] }>("/api/admin/permissions")
      ).permissions;
      break;
    case "access":
      [data.allowlist, data.siteName] = await Promise.all([
        adminJson<{ telegram_ids: string[] }>("/api/admin/access", { telegram_ids: [] }).then(
          (b) => b.telegram_ids
        ),
        siteName(),
      ]);
      break;
    case "vault":
      data.vault = await adminJson<VaultStats>("/api/admin/vault", EMPTY_VAULT);
      break;
    case "storage":
      data.cache = await adminJson<CacheInfo>("/api/admin/cache", { ttlDays: 7, last: null });
      break;
    case "activity":
      data.activity = (
        await apiServerJson<{ activity: AuditRow[] }>("/api/admin/activity")
      ).activity;
      break;
  }

  return (
    <div className="space-y-6">
      <header>
        <h1 className="text-2xl font-bold text-white">Admin</h1>
        <p className="mt-1 text-sm text-slate-400">
          Users, roles, and permissions for this vault.
        </p>
      </header>
      <AdminConsole
        tab={tab}
        users={data.users}
        roles={data.roles}
        permGroups={data.permGroups}
        allowlist={data.allowlist}
        activity={data.activity}
        cache={data.cache}
        vault={data.vault}
        siteName={data.siteName}
      />
    </div>
  );
}
