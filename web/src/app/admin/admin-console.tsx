"use client";

import { useRouter } from "next/navigation";
import { apiFetch } from "@/lib/apiFetch";
import { Eraser, Plus, X } from "lucide-react";
import { useMemo, useState } from "react";
import { ADMIN_TABS, type AdminTab } from "./tabs";

export type AdminUser = {
  id: number;
  email: string | null;
  name: string;
  telegram_id: string | null;
  disabled_at: number | null;
  created_at: number;
  roles: string[];
};
export type RoleWithPerms = {
  id: number;
  name: string;
  builtin: number;
  permissions: string[];
  memberCount: number;
};
export type PermGroup = { resource: string; keys: string[] };
export type CacheInfo = {
  ttlDays: number;
  last: { at: number; scanned: number; deleted: number; freedBytes: number; ttlDays: number } | null;
};
export type AuditRow = {
  id: number;
  actor_user_id: number | null;
  actor_name: string | null;
  action: string;
  target_type: string | null;
  target_id: string | null;
  detail: string | null;
  created_at: number;
};
export type VaultStats = {
  media: { count: number; bytes: number };
  byKind: Record<string, number>;
  byAvailability: Record<string, number>;
  albums: { count: number; publicCount: number };
  shares: { count: number; activeLinks: number };
  users: { count: number; active: number };
  topOwners: { name: string; n: number; bytes: number }[];
};

type Drawer =
  | { kind: "user"; id: number }
  | { kind: "role"; id: number }
  | { kind: "newRole" }
  | { kind: "newUser" }
  | null;

async function call(path: string, init?: RequestInit): Promise<string | null> {
  const res = await apiFetch(path, {
    headers: { "Content-Type": "application/json" },
    ...init,
  });
  if (!res.ok) {
    const body = (await res.json().catch(() => ({}))) as { error?: string };
    return body.error ?? `Request failed (${res.status})`;
  }
  return null;
}

export function AdminConsole({
  tab,
  users,
  roles,
  permGroups,
  allowlist,
  activity,
  cache,
  vault,
  siteName,
}: {
  /** Active tab — route-driven (`/admin?tab=`); the page fetches only
   * the props that tab renders, the rest arrive undefined. */
  tab: AdminTab;
  users?: AdminUser[];
  roles?: RoleWithPerms[];
  permGroups?: PermGroup[];
  allowlist?: string[];
  activity?: AuditRow[];
  cache?: CacheInfo;
  vault?: VaultStats;
  siteName?: string;
}) {
  const router = useRouter();
  const [drawer, setDrawer] = useState<Drawer>(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const openUser = drawer?.kind === "user" ? users?.find((u) => u.id === drawer.id) : undefined;
  const openRole = drawer?.kind === "role" ? roles?.find((r) => r.id === drawer.id) : undefined;

  async function run(fn: () => Promise<string | null>, after?: () => void) {
    setBusy(true);
    setError(null);
    const err = await fn();
    if (err) setError(err);
    else after?.();
    setBusy(false);
    router.refresh();
  }

  return (
    <div>
      {/* Tabs — route-driven so each tab fetches only its own data */}
      <div className="flex gap-1 border-b border-white/5">
        {ADMIN_TABS.map((t) => (
          <button
            key={t}
            onClick={() => router.push(`/admin?tab=${t}`)}
            className={`border-0 bg-transparent px-4 py-2 text-sm capitalize transition ${
              tab === t
                ? "border-b-2 border-teal-400 text-white"
                : "text-slate-400 hover:text-slate-200"
            }`}
          >
            {t}
          </button>
        ))}
      </div>

      {error && (
        <p className="mt-4 rounded-lg border border-red-500/30 bg-red-500/10 px-3 py-2 text-sm text-red-300">
          {error}
        </p>
      )}

      {tab === "users" && users && (
        <UsersTab
          users={users}
          onOpen={(id) => setDrawer({ kind: "user", id })}
          onNew={() => setDrawer({ kind: "newUser" })}
        />
      )}
      {tab === "roles" && roles && (
        <RolesTab
          roles={roles}
          onOpen={(id) => setDrawer({ kind: "role", id })}
          onNew={() => setDrawer({ kind: "newRole" })}
        />
      )}
      {tab === "permissions" && permGroups && <PermissionsTab permGroups={permGroups} />}
      {tab === "access" && allowlist && (
        <AccessTab
          allowlist={allowlist}
          siteName={siteName ?? ""}
          busy={busy}
          onSaveSiteName={(name) =>
            run(() => call("/api/admin/settings", { method: "PATCH", body: JSON.stringify({ siteName: name }) }))
          }
          onAdd={(id) =>
            run(() => call("/api/admin/access", { method: "POST", body: JSON.stringify({ telegram_id: id }) }))
          }
          onRemove={(id) =>
            run(() => call(`/api/admin/access?id=${encodeURIComponent(id)}`, { method: "DELETE" }))
          }
        />
      )}
      {tab === "vault" && vault && <VaultTab vault={vault} />}
      {tab === "storage" && cache && (
        <StorageTab
          cache={cache}
          busy={busy}
          onSaveTtl={(days) =>
            run(() => call("/api/admin/cache", { method: "PATCH", body: JSON.stringify({ ttlDays: days }) }))
          }
          onRunNow={() =>
            run(() => call("/api/admin/cache", { method: "POST" }))
          }
        />
      )}
      {tab === "activity" && activity && <ActivityTab activity={activity} />}

      {/* Drawer */}
      {drawer && (
        <div className="fixed inset-0 z-50 flex justify-end" role="dialog" aria-modal="true">
          <button
            aria-label="Close"
            className="absolute inset-0 border-0 bg-black/60 backdrop-blur-sm"
            onClick={() => setDrawer(null)}
          />
          <aside className="relative h-full w-full max-w-md overflow-y-auto border-l border-white/10 bg-menu p-6 shadow-2xl">
            <button
              onClick={() => setDrawer(null)}
              className="absolute right-4 top-4 rounded-lg border-0 bg-transparent px-2 py-1 text-slate-400 transition hover:bg-white/5 hover:text-white"
            >
              <X className="h-4 w-4" />
            </button>
            {openUser && (
              <UserDrawer
                key={openUser.id}
                user={openUser}
                roles={roles ?? []}
                busy={busy}
                onSaveProfile={(patch) =>
                  run(() =>
                    call(`/api/admin/users/${openUser.id}`, {
                      method: "PATCH",
                      body: JSON.stringify(patch),
                    })
                  )
                }
                onToggleRole={(role) =>
                  run(() =>
                    openUser.roles.includes(role)
                      ? call(`/api/admin/users/${openUser.id}/roles?role=${encodeURIComponent(role)}`, { method: "DELETE" })
                      : call(`/api/admin/users/${openUser.id}/roles`, { method: "POST", body: JSON.stringify({ role }) })
                  )
                }
                onToggleSuspend={() =>
                  run(() =>
                    call(`/api/admin/users/${openUser.id}`, {
                      method: "PATCH",
                      body: JSON.stringify({ disabled: !openUser.disabled_at }),
                    })
                  )
                }
              />
            )}
            {drawer.kind === "newUser" && (
              <NewUserDrawer
                roles={roles ?? []}
                busy={busy}
                onCreate={(data) =>
                  run(
                    () => call("/api/admin/users", { method: "POST", body: JSON.stringify(data) }),
                    () => setDrawer(null)
                  )
                }
              />
            )}
            {(openRole || drawer.kind === "newRole") && (
              <RoleDrawer
                role={openRole}
                permGroups={permGroups ?? []}
                busy={busy}
                onSave={(name, permissions) =>
                  run(
                    () =>
                      openRole
                        ? call(`/api/admin/roles/${openRole.id}`, {
                            method: "PATCH",
                            body: JSON.stringify({
                              ...(openRole.builtin ? {} : { name }),
                              permissions,
                            }),
                          })
                        : (async () => {
                            const created = await call("/api/admin/roles", {
                              method: "POST",
                              body: JSON.stringify({ name }),
                            });
                            if (created) return created;
                            return null;
                          })(),
                    () => setDrawer(null)
                  )
                }
                onDelete={
                  openRole && !openRole.builtin
                    ? () =>
                        run(
                          () => call(`/api/admin/roles/${openRole.id}`, { method: "DELETE" }),
                          () => setDrawer(null)
                        )
                    : undefined
                }
              />
            )}
          </aside>
        </div>
      )}
    </div>
  );
}

/* ------------------------------- Users tab ------------------------------- */

function UsersTab({
  users,
  onOpen,
  onNew,
}: {
  users: AdminUser[];
  onOpen: (id: number) => void;
  onNew: () => void;
}) {
  return (
    <div className="mt-4 overflow-x-auto">
      <table className="w-full text-left text-sm">
        <thead>
          <tr className="border-b border-white/5 text-xs uppercase tracking-wide text-slate-500">
            <th className="pb-2 pr-4">User</th>
            <th className="pb-2 pr-4">Telegram</th>
            <th className="pb-2 pr-4">Roles</th>
            <th className="pb-2 pr-4">Status</th>
          </tr>
        </thead>
        <tbody>
          {users.map((u) => (
            <tr
              key={u.id}
              onClick={() => onOpen(u.id)}
              className="cursor-pointer border-b border-white/5 transition last:border-0 hover:bg-white/3"
            >
              <td className="py-3 pr-4">
                <div className="font-medium text-white">{u.name}</div>
                <div className="text-xs text-slate-500">{u.email ?? "—"}</div>
              </td>
              <td className="py-3 pr-4 text-slate-400">{u.telegram_id ?? "—"}</td>
              <td className="py-3 pr-4">
                <div className="flex flex-wrap gap-1.5">
                  {u.roles.length === 0 && <span className="text-xs text-slate-500">none</span>}
                  {u.roles.map((r) => (
                    <span key={r} className="rounded-full bg-teal-500/20 px-2.5 py-1 text-xs text-teal-300 ring-1 ring-teal-500/40">
                      {r}
                    </span>
                  ))}
                </div>
              </td>
              <td className="py-3 pr-4">
                {u.disabled_at ? (
                  <span className="text-xs text-red-400">suspended</span>
                ) : (
                  <span className="text-xs text-emerald-400">active</span>
                )}
              </td>
            </tr>
          ))}
        </tbody>
      </table>
      <button
        onClick={onNew}
        className="mt-4 flex w-full items-center justify-center gap-1.5 rounded-2xl border border-dashed border-control-line p-4 text-sm transition hover:border-control-ink"
      >
        <Plus className="h-4 w-4" />
        New user
      </button>
      <p className="mt-3 text-xs text-slate-500">Click a user to edit profile, roles, or suspend.</p>
    </div>
  );
}

/* ------------------------------- Roles tab ------------------------------- */

function RolesTab({
  roles,
  onOpen,
  onNew,
}: {
  roles: RoleWithPerms[];
  onOpen: (id: number) => void;
  onNew: () => void;
}) {
  return (
    <div className="mt-4 space-y-3">
      {roles.map((r) => (
        <button
          key={r.id}
          onClick={() => onOpen(r.id)}
          className="w-full rounded-2xl border border-white/5 bg-white/3 p-4 text-left transition hover:border-white/10"
        >
          <div className="flex items-center justify-between gap-3">
            <div className="flex items-center gap-2">
              <span className="font-semibold text-white">{r.name}</span>
              {!!r.builtin && (
                <span className="rounded-full bg-white/5 px-2 py-0.5 text-[10px] uppercase tracking-wide text-slate-400">
                  built-in
                </span>
              )}
            </div>
            <span className="text-xs text-slate-500">
              {r.memberCount} member{r.memberCount === 1 ? "" : "s"} · {r.permissions.includes("*") ? "all" : r.permissions.length}{" "}
              permission{r.permissions.includes("*") || r.permissions.length === 1 ? "" : "s"}
            </span>
          </div>
          <div className="mt-2 flex flex-wrap gap-1.5">
            {r.permissions.includes("*") ? (
              <span className="rounded-full bg-amber-500/15 px-2 py-0.5 text-xs text-amber-300">* (all permissions)</span>
            ) : (
              r.permissions.slice(0, 8).map((p) => (
                <span key={p} className="rounded-full bg-white/5 px-2 py-0.5 text-xs text-slate-400">
                  {p}
                </span>
              ))
            )}
            {!r.permissions.includes("*") && r.permissions.length > 8 && (
              <span className="text-xs text-slate-500">+{r.permissions.length - 8} more</span>
            )}
          </div>
        </button>
      ))}
      <button
        onClick={onNew}
        className="flex w-full items-center justify-center gap-1.5 rounded-2xl border border-dashed border-control-line p-4 text-sm transition hover:border-control-ink"
      >
        <Plus className="h-4 w-4" />
        New custom role
      </button>
    </div>
  );
}

/* ---------------------------- Permissions tab ---------------------------- */

const PERM_DESC: Record<string, string> = {
  "media:view": "Browse & stream the whole vault",
  "media:upload": "Upload files (web & bot)",
  "media:edit:own": "Edit captions on own items",
  "media:edit:any": "Edit captions on any item",
  "media:delete:own": "Delete own items",
  "media:delete:any": "Delete any item",
  "album:create": "Create albums",
  "album:edit:own": "Edit own albums",
  "album:edit:any": "Edit any album",
  "album:delete:own": "Delete own albums",
  "album:delete:any": "Delete any album",
  "share:link": "Mint public /s/ links",
  "share:album": "Toggle album public links",
  "share:friend": "Push media to friends' Telegram",
  "friend:invite": "Mint friend invite links",
  "friend:remove": "Remove friends",
  "admin:access": "Open the /admin area",
  "user:manage": "Assign roles, suspend users",
  "role:manage": "Create & edit roles",
  "settings:manage": "Edit instance settings (P2)",
};

function PermissionsTab({ permGroups }: { permGroups: PermGroup[] }) {
  return (
    <div className="mt-4 space-y-4">
      {permGroups.map((g) => (
        <section key={g.resource} className="rounded-2xl border border-white/5 bg-white/3 p-4">
          <h3 className="text-sm font-semibold capitalize text-white">{g.resource}</h3>
          <ul className="mt-2 space-y-1.5">
            {g.keys.map((k) => (
              <li key={k} className="flex items-baseline justify-between gap-4 text-sm">
                <code className="text-teal-300">{k}</code>
                <span className="text-right text-xs text-slate-500">{PERM_DESC[k] ?? ""}</span>
              </li>
            ))}
          </ul>
        </section>
      ))}
      <p className="text-xs text-slate-500">
        The catalog is code-defined — a permission only works where a route checks it.{" "}
        <code className="text-slate-400">*</code> (admin role) grants everything.
      </p>
    </div>
  );
}

/* ------------------------------- Access tab ------------------------------ */

function AccessTab({
  allowlist,
  siteName,
  busy,
  onSaveSiteName,
  onAdd,
  onRemove,
}: {
  allowlist: string[];
  siteName: string;
  busy: boolean;
  onSaveSiteName: (name: string) => void;
  onAdd: (id: string) => void;
  onRemove: (id: string) => void;
}) {
  const [id, setId] = useState("");
  const [name, setName] = useState(siteName);
  const nameDirty = name.trim() !== "" && name.trim() !== siteName;
  return (
    <div className="mt-4 space-y-4">
      <section className="rounded-2xl border border-white/5 bg-white/3 p-4">
        <h3 className="text-sm font-semibold text-white">Site name</h3>
        <p className="mt-1 text-xs text-slate-500">
          Shown in the navbar, page titles, share pages and bot messages. Stored in the
          database — no redeploy needed.
        </p>
        <div className="mt-3 flex gap-2">
          <input
            value={name}
            onChange={(e) => setName(e.target.value)}
            maxLength={80}
            placeholder="OpenShares"
            className="flex-1 rounded-xl px-3 py-2 text-sm outline-none focus:border-control-ink"
          />
          <button
            disabled={busy || !nameDirty}
            onClick={() => onSaveSiteName(name.trim())}
            className="rounded-xl px-4 py-2 text-sm font-semibold transition disabled:opacity-50"
          >
            Save
          </button>
        </div>
      </section>
      <section className="rounded-2xl border border-white/5 bg-white/3 p-4">
        <h3 className="text-sm font-semibold text-white">Telegram allowlist</h3>
        <p className="mt-1 text-xs text-slate-500">
          Only these Telegram ids can create vault accounts via the bot sign-in code.
          Stored in the database — no redeploy needed.
        </p>
        <div className="mt-3 flex gap-2">
          <input
            value={id}
            onChange={(e) => setId(e.target.value.replace(/\D/g, ""))}
            placeholder="Telegram user id (digits)"
            className="flex-1 rounded-xl px-3 py-2 text-sm outline-none focus:border-control-ink"
          />
          <button
            disabled={busy || !id.trim()}
            onClick={() => { onAdd(id.trim()); setId(""); }}
            className="rounded-xl px-4 py-2 text-sm font-semibold transition disabled:opacity-50"
          >
            Add
          </button>
        </div>
      </section>
      <ul className="divide-y divide-white/5 overflow-hidden rounded-2xl border border-white/5 bg-white/2">
        {allowlist.length === 0 && (
          <li className="px-4 py-3 text-sm text-slate-500">Allowlist is empty — nobody new can sign up.</li>
        )}
        {allowlist.map((tid) => (
          <li key={tid} className="flex items-center justify-between px-4 py-2.5 text-sm">
            <code className="text-teal-300">{tid}</code>
            <button
              disabled={busy}
              onClick={() => onRemove(tid)}
              className="rounded-lg px-2.5 py-1 text-xs text-red-600 transition hover:bg-red-500/10"
            >
              Remove
            </button>
          </li>
        ))}
      </ul>
    </div>
  );
}

/* ------------------------------- Vault tab ------------------------------- */

const fmtMb = (n: number) =>
  n < 1048576 ? `${(n / 1024).toFixed(0)} KB` : `${(n / 1048576).toFixed(1)} MB`;

function Stat({ label, value }: { label: string; value: string }) {
  return (
    <div className="rounded-xl border border-white/5 bg-black/20 px-3 py-2.5">
      <div className="text-[11px] uppercase tracking-wide text-slate-500">{label}</div>
      <div className="mt-0.5 text-sm font-semibold text-white">{value}</div>
    </div>
  );
}

function VaultTab({ vault }: { vault: VaultStats }) {
  const avail = vault.byAvailability;
  return (
    <div className="mt-4 max-w-2xl space-y-4">
      <section className="rounded-2xl border border-white/5 bg-white/3 p-4">
        <h3 className="text-sm font-semibold text-white">Media index</h3>
        <div className="mt-3 grid grid-cols-2 gap-2 sm:grid-cols-4">
          <Stat label="Items" value={String(vault.media.count)} />
          <Stat label="Stored" value={fmtMb(vault.media.bytes)} />
          <Stat label="Photos" value={String(vault.byKind.photo ?? 0)} />
          <Stat label="Videos" value={String(vault.byKind.video ?? 0)} />
        </div>
        <div className="mt-2 grid grid-cols-3 gap-2">
          <Stat label="Streamable" value={String(avail.available ?? 0)} />
          <Stat label="Telegram-only" value={String(avail["telegram-only"] ?? 0)} />
          <Stat
            label="Gone"
            value={String(avail.dead ?? 0)}
          />
        </div>
      </section>

      <section className="rounded-2xl border border-white/5 bg-white/3 p-4">
        <h3 className="text-sm font-semibold text-white">Sharing</h3>
        <div className="mt-3 grid grid-cols-2 gap-2 sm:grid-cols-4">
          <Stat label="Albums" value={String(vault.albums.count)} />
          <Stat label="Public albums" value={String(vault.albums.publicCount)} />
          <Stat label="Share events" value={String(vault.shares.count)} />
          <Stat label="Live links" value={String(vault.shares.activeLinks)} />
        </div>
      </section>

      <section className="rounded-2xl border border-white/5 bg-white/3 p-4">
        <h3 className="text-sm font-semibold text-white">Users</h3>
        <div className="mt-3 grid grid-cols-2 gap-2 sm:grid-cols-4">
          <Stat label="Accounts" value={String(vault.users.count)} />
          <Stat label="Active" value={String(vault.users.active)} />
        </div>
      </section>

      {vault.topOwners.length > 0 && (
        <section className="rounded-2xl border border-white/5 bg-white/3 p-4">
          <h3 className="text-sm font-semibold text-white">Top storage owners</h3>
          <table className="mt-3 w-full text-left text-sm">
            <tbody>
              {vault.topOwners.map((o) => (
                <tr key={o.name} className="border-b border-white/5 last:border-0">
                  <td className="py-2 pr-4 text-slate-300">{o.name}</td>
                  <td className="py-2 pr-4 text-right text-slate-400">{o.n} items</td>
                  <td className="py-2 text-right text-slate-400">{fmtMb(o.bytes)}</td>
                </tr>
              ))}
            </tbody>
          </table>
        </section>
      )}
    </div>
  );
}

/* ------------------------------ Storage tab ------------------------------ */

function StorageTab({
  cache,
  busy,
  onSaveTtl,
  onRunNow,
}: {
  cache: CacheInfo;
  busy: boolean;
  onSaveTtl: (days: number) => void;
  onRunNow: () => void;
}) {
  const [days, setDays] = useState(cache.ttlDays);
  const last = cache.last;
  return (
    <div className="mt-4 max-w-lg space-y-4">
      <section className="rounded-2xl border border-white/5 bg-white/3 p-4">
        <h3 className="text-sm font-semibold text-white">Media cache rotation</h3>
        <p className="mt-1 text-xs text-slate-500">
          Downloaded Telegram files are cached in R2 (<code>MEDIA_CACHE</code>) for fast streaming.
          A daily job evicts objects older than the TTL — originals always stay in Telegram and are
          re-cached on the next view. Allowed range: 1-90 days.
        </p>
        <div className="mt-3 flex items-end gap-2">
          <label className="flex-1">
            <span className="mb-1 block text-xs text-slate-400">Keep cached files for (days)</span>
            <input
              type="number"
              min={1}
              max={90}
              value={days}
              onChange={(e) => setDays(Number(e.target.value))}
              className="w-full rounded-xl px-3 py-2 text-sm outline-none focus:border-control-ink"
            />
          </label>
          <button
            disabled={busy || !Number.isFinite(days) || days < 1}
            onClick={() => onSaveTtl(days)}
            className="rounded-xl px-4 py-2 text-sm font-semibold transition disabled:opacity-50"
          >
            Save
          </button>
        </div>
      </section>

      <section className="rounded-2xl border border-white/5 bg-white/3 p-4">
        <h3 className="text-sm font-semibold text-white">Cleanup runs</h3>
        {last ? (
          <dl className="mt-2 space-y-1 text-sm">
            <div className="flex justify-between">
              <dt className="text-slate-500">Last run</dt>
              <dd className="text-slate-300">{new Date(last.at * 1000).toLocaleString()}</dd>
            </div>
            <div className="flex justify-between">
              <dt className="text-slate-500">Evicted</dt>
              <dd className="text-slate-300">
                {last.deleted}/{last.scanned} objects · {(last.freedBytes / 1048576).toFixed(1)} MB freed
              </dd>
            </div>
            <div className="flex justify-between">
              <dt className="text-slate-500">TTL then</dt>
              <dd className="text-slate-300">{last.ttlDays} day{last.ttlDays === 1 ? "" : "s"}</dd>
            </div>
          </dl>
        ) : (
          <p className="mt-2 text-xs text-slate-500">No cleanup has run yet.</p>
        )}
        <button
          disabled={busy}
          onClick={onRunNow}
          className="mt-3 flex w-full items-center justify-center gap-2 rounded-xl px-4 py-2.5 text-sm transition disabled:opacity-50"
        >
          <Eraser className="h-4 w-4" />
          Run cleanup now
        </button>
        <p className="mt-2 text-[11px] text-slate-600">
          Also runs automatically every day at 04:15 UTC.
        </p>
      </section>
    </div>
  );
}

/* ------------------------------ Activity tab ----------------------------- */

function ActivityTab({ activity }: { activity: AuditRow[] }) {
  if (activity.length === 0) {
    return <p className="mt-4 text-sm text-slate-500">No admin actions logged yet.</p>;
  }
  return (
    <div className="mt-4 overflow-x-auto">
      <table className="w-full text-left text-sm">
        <thead>
          <tr className="border-b border-white/5 text-xs uppercase tracking-wide text-slate-500">
            <th className="pb-2 pr-4">When</th>
            <th className="pb-2 pr-4">Actor</th>
            <th className="pb-2 pr-4">Action</th>
            <th className="pb-2 pr-4">Target</th>
            <th className="pb-2">Detail</th>
          </tr>
        </thead>
        <tbody>
          {activity.map((a) => (
            <tr key={a.id} className="border-b border-white/5 last:border-0">
              <td className="py-2.5 pr-4 whitespace-nowrap text-xs text-slate-500">
                {new Date(a.created_at * 1000).toLocaleString()}
              </td>
              <td className="py-2.5 pr-4 text-slate-300">{a.actor_name ?? `#${a.actor_user_id ?? "—"}`}</td>
              <td className="py-2.5 pr-4"><code className="text-xs text-teal-300">{a.action}</code></td>
              <td className="py-2.5 pr-4 text-xs text-slate-400">
                {a.target_type ? `${a.target_type}${a.target_id ? ` ${a.target_id}` : ""}` : "—"}
              </td>
              <td className="py-2.5 text-xs text-slate-500">{a.detail ?? "—"}</td>
            </tr>
          ))}
        </tbody>
      </table>
    </div>
  );
}

/* ------------------------------- Drawers --------------------------------- */

function DrawerTitle({ children }: { children: React.ReactNode }) {
  return <h2 className="mb-4 pr-8 text-lg font-semibold text-white">{children}</h2>;
}

function UserDrawer({
  user,
  roles,
  busy,
  onSaveProfile,
  onToggleRole,
  onToggleSuspend,
}: {
  user: AdminUser;
  roles: RoleWithPerms[];
  busy: boolean;
  onSaveProfile: (patch: { name?: string; email?: string | null; password?: string }) => void;
  onToggleRole: (role: string) => void;
  onToggleSuspend: () => void;
}) {
  const [name, setName] = useState(user.name);
  const [email, setEmail] = useState(user.email ?? "");
  const [password, setPassword] = useState("");

  const patch: { name?: string; email?: string | null; password?: string } = {};
  if (name.trim() !== user.name) patch.name = name.trim();
  const nextEmail = email.trim() || null;
  if (nextEmail !== user.email) patch.email = nextEmail;
  if (password) patch.password = password;
  const dirty = Object.keys(patch).length > 0;

  return (
    <div>
      <DrawerTitle>{user.name}</DrawerTitle>
      <dl className="space-y-1 text-sm">
        <div className="flex justify-between"><dt className="text-slate-500">Telegram</dt><dd className="text-slate-300">{user.telegram_id ?? "—"}</dd></div>
        <div className="flex justify-between"><dt className="text-slate-500">Joined</dt><dd className="text-slate-300">{new Date(user.created_at * 1000).toLocaleDateString()}</dd></div>
      </dl>

      <h3 className="mb-2 mt-6 text-xs font-semibold uppercase tracking-wide text-slate-500">Account</h3>
      <div className="space-y-2.5">
        <input
          value={name}
          onChange={(e) => setName(e.target.value)}
          placeholder="Display name"
          className="w-full rounded-xl px-3 py-2 text-sm outline-none focus:border-control-ink"
        />
        <input
          value={email}
          onChange={(e) => setEmail(e.target.value)}
          placeholder="Email (sign-in)"
          inputMode="email"
          autoComplete="off"
          className="w-full rounded-xl px-3 py-2 text-sm outline-none focus:border-control-ink"
        />
        <input
          value={password}
          onChange={(e) => setPassword(e.target.value)}
          placeholder="New password — leave blank to keep"
          type="password"
          autoComplete="new-password"
          className="w-full rounded-xl px-3 py-2 text-sm outline-none focus:border-control-ink"
        />
        <button
          disabled={busy || !dirty}
          onClick={() => {
            onSaveProfile(patch);
            setPassword("");
          }}
          className="rounded-xl px-4 py-2 text-sm font-semibold transition disabled:opacity-50"
        >
          Save changes
        </button>
        <p className="text-xs text-slate-600">
          A new password signs the user out everywhere — they log in with the new one.
        </p>
      </div>

      <h3 className="mb-2 mt-6 text-xs font-semibold uppercase tracking-wide text-slate-500">Roles</h3>
      <div className="flex flex-wrap gap-1.5">
        {roles.map((r) => {
          const on = user.roles.includes(r.name);
          return (
            <button
              key={r.id}
              disabled={busy}
              onClick={() => onToggleRole(r.name)}
              className={`rounded-full px-3 py-1.5 text-xs transition ${
                on
                  ? "bg-teal-500/20 text-teal-300 ring-1 ring-teal-500/40"
                  : "bg-white/5 text-slate-500 hover:text-slate-300"
              }`}
            >
              {r.name}
            </button>
          );
        })}
      </div>

      <h3 className="mb-2 mt-6 text-xs font-semibold uppercase tracking-wide text-slate-500">Danger zone</h3>
      <button
        disabled={busy}
        onClick={onToggleSuspend}
        className={`rounded-lg px-3 py-2 text-sm transition ${
          user.disabled_at
            ? "bg-emerald-500/10 text-emerald-500 hover:bg-emerald-500/20"
            : "bg-red-500/10 text-red-500 hover:bg-red-500/20"
        }`}
      >
        {user.disabled_at ? "Unsuspend user" : "Suspend user (kills sessions)"}
      </button>
      <p className="mt-3 text-xs text-slate-500">
        The last admin can't be demoted or suspended — promote someone else first.
      </p>
    </div>
  );
}

function NewUserDrawer({
  roles,
  busy,
  onCreate,
}: {
  roles: RoleWithPerms[];
  busy: boolean;
  onCreate: (data: { name: string; email: string | null; password: string; roles: string[] }) => void;
}) {
  const [name, setName] = useState("");
  const [email, setEmail] = useState("");
  const [password, setPassword] = useState("");
  const [picked, setPicked] = useState<Set<string>>(new Set());

  const canCreate = name.trim().length > 0 && (!password || email.trim().length > 0);

  function toggle(role: string) {
    setPicked((prev) => {
      const next = new Set(prev);
      if (next.has(role)) next.delete(role);
      else next.add(role);
      return next;
    });
  }

  return (
    <div>
      <DrawerTitle>New user</DrawerTitle>
      <div className="space-y-2.5">
        <input
          value={name}
          onChange={(e) => setName(e.target.value)}
          placeholder="Display name"
          className="w-full rounded-xl px-3 py-2 text-sm outline-none focus:border-control-ink"
        />
        <input
          value={email}
          onChange={(e) => setEmail(e.target.value)}
          placeholder="Email (optional)"
          inputMode="email"
          autoComplete="off"
          className="w-full rounded-xl px-3 py-2 text-sm outline-none focus:border-control-ink"
        />
        <input
          value={password}
          onChange={(e) => setPassword(e.target.value)}
          placeholder="Password (optional, 8+ chars)"
          type="password"
          autoComplete="new-password"
          className="w-full rounded-xl px-3 py-2 text-sm outline-none focus:border-control-ink"
        />
        <p className="text-xs text-slate-600">
          Email + password enable web sign-in. Leave both blank for a Telegram-only account
          (they link via the bot later).
        </p>
      </div>

      <h3 className="mb-2 mt-6 text-xs font-semibold uppercase tracking-wide text-slate-500">Roles</h3>
      <div className="flex flex-wrap gap-1.5">
        {roles.map((r) => {
          const on = picked.has(r.name);
          return (
            <button
              key={r.id}
              disabled={busy}
              onClick={() => toggle(r.name)}
              className={`rounded-full px-3 py-1.5 text-xs transition ${
                on
                  ? "bg-teal-500/20 text-teal-300 ring-1 ring-teal-500/40"
                  : "bg-white/5 text-slate-500 hover:text-slate-300"
              }`}
            >
              {r.name}
            </button>
          );
        })}
      </div>

      <button
        disabled={busy || !canCreate}
        onClick={() =>
          onCreate({
            name: name.trim(),
            email: email.trim() || null,
            password,
            roles: [...picked],
          })
        }
        className="mt-6 rounded-xl px-4 py-2 text-sm font-semibold transition disabled:opacity-50"
      >
        Create user
      </button>
    </div>
  );
}

function RoleDrawer({
  role,
  permGroups,
  busy,
  onSave,
  onDelete,
}: {
  role?: RoleWithPerms;
  permGroups: PermGroup[];
  busy: boolean;
  onSave: (name: string, permissions: string[]) => void;
  onDelete?: () => void;
}) {
  const isAdminRole = role?.builtin === 1 && role.name === "admin";
  const [name, setName] = useState(role?.name ?? "");
  const [selected, setSelected] = useState<Set<string>>(
    () => new Set(role?.permissions.filter((p) => p !== "*") ?? [])
  );
  const allKeys = useMemo(() => permGroups.flatMap((g) => g.keys), [permGroups]);

  function toggle(key: string) {
    setSelected((prev) => {
      const next = new Set(prev);
      if (next.has(key)) next.delete(key);
      else next.add(key);
      return next;
    });
  }

  const payload = isAdminRole ? ["*", ...[...selected].filter((k) => k !== "*")] : [...selected];

  return (
    <div>
      <DrawerTitle>{role ? `Edit role — ${role.name}` : "New role"}</DrawerTitle>

      <label className="block text-xs font-semibold uppercase tracking-wide text-slate-500">Name</label>
      <input
        value={name}
        onChange={(e) => setName(e.target.value)}
        disabled={busy || !!role?.builtin}
        placeholder="e.g. curator"
        className="mt-1 w-full rounded-xl px-3 py-2 text-sm outline-none focus:border-control-ink disabled:opacity-50"
      />
      {!!role?.builtin && <p className="mt-1 text-xs text-slate-500">Built-in roles can't be renamed.</p>}

      <div className="mb-2 mt-6 flex items-center justify-between">
        <h3 className="text-xs font-semibold uppercase tracking-wide text-slate-500">Permissions</h3>
        <div className="flex gap-2 text-xs">
          <button className="border-0 bg-transparent p-0 text-teal-300 hover:underline" onClick={() => setSelected(new Set(allKeys))}>all</button>
          <button className="border-0 bg-transparent p-0 text-slate-500 hover:underline" onClick={() => setSelected(new Set())}>none</button>
        </div>
      </div>

      {isAdminRole && (
        <p className="mb-3 rounded-lg border border-amber-500/30 bg-amber-500/10 px-3 py-2 text-xs text-amber-300">
          Admin holds <code>*</code> (all permissions). The checkboxes below only document intent — * already covers them.
        </p>
      )}

      <div className="space-y-4">
        {permGroups.map((g) => (
          <section key={g.resource}>
            <h4 className="mb-1.5 text-xs font-semibold capitalize text-slate-400">{g.resource}</h4>
            <div className="grid grid-cols-1 gap-1">
              {g.keys.map((k) => (
                <label key={k} className="flex cursor-pointer items-center gap-2 rounded-lg px-2 py-1.5 text-sm hover:bg-white/5">
                  <input
                    type="checkbox"
                    checked={selected.has(k)}
                    onChange={() => toggle(k)}
                    disabled={busy}
                    className="accent-teal-500"
                  />
                  <code className="text-teal-300">{k}</code>
                  <span className="ml-auto text-xs text-slate-500">{PERM_DESC[k] ?? ""}</span>
                </label>
              ))}
            </div>
          </section>
        ))}
      </div>

      <div className="mt-6 flex items-center gap-3">
        <button
          disabled={busy || (!role && !name.trim())}
          onClick={() => onSave(name.trim(), payload)}
          className="rounded-xl px-4 py-2 text-sm font-semibold transition disabled:opacity-50"
        >
          {role ? "Save changes" : "Create role"}
        </button>
        {onDelete && (
          <button
            disabled={busy}
            onClick={onDelete}
            className="rounded-xl bg-red-500/10 px-4 py-2 text-sm text-red-500 transition hover:bg-red-500/20"
          >
            Delete role
          </button>
        )}
      </div>
      {role && (
        <p className="mt-3 text-xs text-slate-500">
          {role.memberCount} member{role.memberCount === 1 ? "" : "s"} — changes apply on their next request.
        </p>
      )}
    </div>
  );
}
