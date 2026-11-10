"use client";

import { useCallback, useEffect, useState } from "react";
import { ArrowRight, UserPlus, Users } from "lucide-react";
import { useRouter } from "next/navigation";
import { ConfirmDialog } from "./ConfirmDialog";
import { apiFetch } from "@/lib/apiFetch";
import { usePermissions } from "./PermissionsProvider";

type Friend = {
  id: number;
  name: string;
  telegram_id: string;
  username: string | null;
  shared: number;
};

type Invite = { code: string; bot_link: string };

export function FriendsPanel() {
  const router = useRouter();
  const [friends, setFriends] = useState<Friend[] | null>(null);
  const [invite, setInvite] = useState<Invite | null>(null);
  const [busy, setBusy] = useState(false);
  const [err, setErr] = useState<string | null>(null);
  const [confirmRemove, setConfirmRemove] = useState<Friend | null>(null);
  const { can } = usePermissions();

  const load = useCallback(async () => {
    const res = await apiFetch("/api/friends", { cache: "no-store" });
    if (!res.ok) {
      setErr("Could not load friends.");
      return;
    }
    const { friends: list } = (await res.json()) as { friends: Friend[] };
    setFriends(list);
  }, []);

  useEffect(() => {
    void load();
  }, [load]);

  async function createInvite() {
    setBusy(true);
    setErr(null);
    const res = await apiFetch("/api/friends", {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify({ action: "invite" }),
    });
    setBusy(false);
    const body = (await res.json().catch(() => ({}))) as Partial<Invite> & { error?: string };
    if (!res.ok || !body.bot_link || !body.code) {
      setErr(body.error ?? "Invite failed.");
      return;
    }
    setInvite({ code: body.code, bot_link: body.bot_link });
  }

  async function doRemove(f: Friend) {
    setBusy(true);
    const res = await apiFetch(`/api/friends/${f.id}`, { method: "DELETE" });
    setBusy(false);
    setConfirmRemove(null);
    if (res.ok) setFriends((prev) => prev?.filter((x) => x.id !== f.id) ?? null);
    else setErr(`Couldn't remove ${f.name}.`);
    router.refresh();
  }

  return (
    <div className="space-y-5">
      <div className="flex flex-wrap items-center justify-between gap-3">
        <p className="text-sm text-slate-400">
          Friends are Telegram users. Once they accept your invite you can push any stored item
          straight into their chat.
        </p>
        {can("friend:invite") && (
          <button
            disabled={busy}
            onClick={createInvite}
            className="inline-flex shrink-0 items-center gap-1.5 rounded-xl px-4 py-2 text-sm font-semibold transition disabled:opacity-50"
          >
            <UserPlus className="h-4 w-4" />
            Add friend
          </button>
        )}
      </div>

      {err && <p className="text-sm text-rose-400">{err}</p>}

      {invite && (
        <div className="rounded-2xl border border-teal-500/25 bg-teal-500/[0.06] p-4">
          <h3 className="text-sm font-semibold text-white">Invite your friend</h3>
          <p className="mt-1 text-sm text-slate-400">
            Send them this link (or have them send the code to the bot) — one-time use, 7 days:
          </p>
          <div className="mt-2 flex flex-wrap items-center gap-2">
            <code className="select-all rounded-lg bg-black/40 px-3 py-2 font-mono text-xs text-teal-300">
              {invite.bot_link}
            </code>
            <button
              onClick={() => void navigator.clipboard.writeText(invite.bot_link)}
              className="rounded-lg px-2.5 py-1.5 text-xs transition"
            >
              Copy
            </button>
            <a
              href={invite.bot_link}
              target="_blank"
              rel="noreferrer"
              className="inline-flex items-center gap-1 rounded-lg bg-[#2AABEE] px-3 py-1.5 text-xs font-medium text-[#fff] hover:brightness-110"
            >
              Open in Telegram
              <ArrowRight className="h-3.5 w-3.5" />
            </a>
            <code className="rounded-lg bg-black/40 px-2 py-1.5 font-mono text-xs text-slate-400">
              /start {invite.code}
            </code>
          </div>
        </div>
      )}

      {friends === null ? (
        <p className="text-sm text-slate-500">Loading…</p>
      ) : friends.length === 0 ? (
        <div className="rounded-2xl border border-white/5 bg-white/2 px-6 py-14 text-center">
          <Users className="mx-auto h-10 w-10 text-slate-500" />
          <h2 className="mt-3 font-semibold text-white">No friends yet</h2>
          <p className="mx-auto mt-1 max-w-sm text-sm text-slate-400">
            Click “Add friend” and share the invite with someone on Telegram.
          </p>
        </div>
      ) : (
        <ul className="divide-y divide-white/5 overflow-hidden rounded-2xl border border-white/5 bg-white/2">
          {friends.map((f) => (
            <li key={f.id} className="flex items-center gap-3 px-4 py-3">
              <span className="grid h-9 w-9 shrink-0 place-items-center rounded-full bg-gradient-to-br from-sky-500/80 to-teal-500/80 text-xs font-bold text-black">
                {(f.name ?? "?").slice(0, 2).toUpperCase()}
              </span>
              <div className="min-w-0 flex-1">
                <div className="truncate text-sm font-medium text-slate-200">{f.name}</div>
                <div className="truncate text-xs text-slate-500">
                  {f.username ? `@${f.username}` : f.telegram_id}
                </div>
              </div>
              <span className="rounded-lg bg-white/5 px-2 py-0.5 text-xs text-slate-400">
                {f.shared} shared
              </span>
              {can("friend:remove") && (
                <button
                  onClick={() => setConfirmRemove(f)}
                  className="rounded-lg px-2 py-1 text-xs text-rose-600 transition hover:bg-rose-500/10"
                >
                  Remove
                </button>
              )}
            </li>
          ))}
        </ul>
      )}

      {confirmRemove && (
        <ConfirmDialog
          title={`Remove ${confirmRemove.name}?`}
          confirmLabel="Remove friend"
          danger
          busy={busy}
          onConfirm={() => void doRemove(confirmRemove)}
          onCancel={() => setConfirmRemove(null)}
        >
          They&apos;ll no longer get media pushed to their chat. You can always
          invite them again.
        </ConfirmDialog>
      )}
    </div>
  );
}
