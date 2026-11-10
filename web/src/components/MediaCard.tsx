"use client";

import { useState } from "react";
import { Check, ChevronDown, Eye, Film, Link2, Play, Send, Trash2 } from "lucide-react";
import { ConfirmDialog } from "./ConfirmDialog";
import { apiFetch } from "@/lib/apiFetch";
import { usePermissions } from "./PermissionsProvider";

export type MediaItem = {
  slug: string;
  ownerId: number | null;
  kind: "photo" | "video";
  caption: string | null;
  width: number | null;
  height: number | null;
  duration: number | null;
  fileSize: number | null;
  availability: "available" | "telegram-only" | "dead";
  sharesCount: number;
  createdAt: number;
};

export type Friend = { id: number; name: string; telegram_id: string; username: string | null };

type ShareLink = {
  token: string;
  url: string;
  createdAt: number;
  expiresAt: number | null;
  views: number;
  likes: number;
};

const fmtSize = (n: number) =>
  n < 1024 ? `${n} B` : n < 1048576 ? `${(n / 1024).toFixed(0)} KB` : `${(n / 1048576).toFixed(1)} MB`;

const fmtDate = (ts: number) =>
  new Date(ts * 1000).toLocaleString(undefined, { month: "short", day: "numeric", hour: "2-digit", minute: "2-digit" });

const fmtDur = (s: number) => `${Math.floor(s / 60)}:${String(s % 60).padStart(2, "0")}`;

export function MediaCard({
  item,
  friends,
  selectable = false,
  selected = false,
  onToggleSelect,
  compact = false,
  onDragStartItems,
  onView,
}: {
  item: MediaItem;
  friends: Friend[];
  selectable?: boolean;
  selected?: boolean;
  onToggleSelect?: (slug: string) => void;
  compact?: boolean;
  /** Set by MediaGrid — makes the tile a drag source for move-to-album (photos + select-mode). */
  onDragStartItems?: (slug: string, e: React.DragEvent) => void;
  /** Opens the fullscreen viewer (streamable items only). */
  onView?: () => void;
}) {
  const [caption, setCaption] = useState(item.caption ?? "");
  const [editing, setEditing] = useState(false);
  const [busy, setBusy] = useState(false);
  const [actionMsg, setActionMsg] = useState<{ text: string; ok: boolean } | null>(null);
  const [deleted, setDeleted] = useState(false);
  const [imgReady, setImgReady] = useState(false);
  const [confirmDelete, setConfirmDelete] = useState(false);
  const [linksOpen, setLinksOpen] = useState(false);
  const [links, setLinks] = useState<ShareLink[] | null>(null);
  const [revoking, setRevoking] = useState<string | null>(null);

  const srcUrl = `/api/media/${item.slug}`;
  const kindLabel = item.kind === "video" ? "Video" : "Photo";
  // UX gate — hides controls the signed-in user can't actually run
  // (server-side RBAC stays authoritative).
  const { can } = usePermissions();
  const canEdit = can("media:edit", item.ownerId);
  const canDeleteItem = can("media:delete", item.ownerId);
  const canShareLink = can("share:link");
  const canShareFriend = can("share:friend");

  async function patchCaption(next: string) {
    setBusy(true);
    const res = await apiFetch(`/api/media/${item.slug}`, {
      method: "PATCH",
      headers: { "content-type": "application/json" },
      body: JSON.stringify({ caption: next }),
    });
    setBusy(false);
    if (res.ok) {
      setCaption(next);
      setEditing(false);
    } else {
      setActionMsg({ text: "Rename failed", ok: false });
    }
  }

  async function shareLink() {
    setBusy(true);
    setActionMsg(null);
    const res = await apiFetch(`/api/media/${item.slug}/share`, {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify({ mode: "link" }),
    });
    setBusy(false);
    if (!res.ok) return setActionMsg({ text: "Share failed", ok: false });
    const { url, token, expiresAt } = (await res.json()) as {
      url: string;
      token?: string;
      expiresAt?: number | null;
    };
    if (linksOpen && links !== null && token) {
      setLinks((prev) => [
        { token, url, createdAt: Math.floor(Date.now() / 1000), expiresAt: expiresAt ?? null, views: 0, likes: 0 },
        ...(prev ?? []),
      ]);
    }
    const exp = expiresAt
      ? ` — expires ${new Date(expiresAt * 1000).toLocaleDateString(undefined, { month: "short", day: "numeric" })}`
      : "";
    try {
      await navigator.clipboard.writeText(url);
      setActionMsg({ text: `Public link copied${exp}`, ok: true });
    } catch {
      setActionMsg({ text: url, ok: true });
    }
  }

  async function shareTo(friendId: number) {
    setBusy(true);
    setActionMsg(null);
    const res = await apiFetch(`/api/media/${item.slug}/share`, {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify({ mode: "telegram", friend_id: friendId }),
    });
    setBusy(false);
    const body = (await res.json().catch(() => ({}))) as { ok?: boolean; error?: string };
    setActionMsg(res.ok ? { text: "Sent", ok: true } : { text: body.error ?? "Send failed", ok: false });
  }

  async function toggleLinks() {
    const next = !linksOpen;
    setLinksOpen(next);
    if (next && links === null) {
      const res = await apiFetch(`/api/media/${item.slug}/share`);
      if (res.ok) setLinks(((await res.json()) as { links: ShareLink[] }).links);
      else setLinks([]);
    }
  }

  async function revokeLink(token: string) {
    setRevoking(token);
    const res = await apiFetch(`/api/media/${item.slug}/share`, {
      method: "DELETE",
      headers: { "content-type": "application/json" },
      body: JSON.stringify({ token }),
    });
    setRevoking(null);
    if (res.ok) setLinks((prev) => prev?.filter((l) => l.token !== token) ?? prev);
    else setActionMsg({ text: "Revoke failed", ok: false });
  }

  async function doDelete() {
    setBusy(true);
    const res = await apiFetch(`/api/media/${item.slug}`, { method: "DELETE" });
    setBusy(false);
    if (res.ok) setDeleted(true);
    else {
      setConfirmDelete(false);
      setActionMsg({ text: "Delete failed", ok: false });
    }
  }

  if (deleted) return null;

  return (
    <div className="group overflow-hidden rounded-2xl border border-white/5 bg-white/3 transition hover:border-white/10">
      <div
        className="relative aspect-square w-full bg-black/40"
        draggable={!!onDragStartItems && item.kind === "photo"}
        onDragStart={(e) => onDragStartItems?.(item.slug, e)}
      >
        {item.kind === "photo" ? (
          <>
            {!imgReady && <span className="absolute inset-0 animate-pulse bg-white/5" />}
            {/* eslint-disable-next-line @next/next/no-img-element */}
            <img
              src={`${srcUrl}?thumb=1`}
              alt={caption || "Photo"}
              loading="lazy"
              draggable={false}
              onLoad={() => setImgReady(true)}
              className={`h-full w-full cursor-pointer object-cover transition duration-300 group-hover:opacity-90 ${
                imgReady ? "opacity-100" : "opacity-0"
              }`}
              onClick={onView}
            />
          </>
        ) : item.availability === "available" ? (
          <button type="button" onClick={onView} className="relative block h-full w-full">
            {/* eslint-disable-next-line @next/next/no-img-element */}
            <img
              src={`${srcUrl}?thumb=1`}
              alt={caption || "Video"}
              loading="lazy"
              draggable={false}
              className="h-full w-full object-cover"
            />
            <span className="absolute inset-0 grid place-items-center">
              <span className="grid h-11 w-11 place-items-center rounded-full bg-black/60 text-white">
                <Play className="h-5 w-5" fill="currentColor" />
              </span>
            </span>
          </button>
        ) : (
          <div className="flex h-full w-full flex-col items-center justify-center gap-2 text-center text-xs text-slate-500">
            <Film className="h-8 w-8" />
            <span>Video stored in Telegram</span>
            <span className="text-[10px]">(over 20 MB — web playback unavailable)</span>
          </div>
        )}
        {item.kind === "video" && item.duration !== null && (
          <span className="pointer-events-none absolute bottom-2 right-2 rounded-md bg-black/70 px-1.5 py-0.5 text-[10px] font-medium text-white">
            {fmtDur(item.duration)}
          </span>
        )}
        {item.availability === "telegram-only" && (
          <span className="absolute left-2 top-2 rounded-md bg-amber-500/90 px-1.5 py-0.5 text-[10px] font-semibold text-black">
            20 MB+
          </span>
        )}
        {item.availability === "dead" && (
          <span className="absolute left-2 top-2 rounded-md bg-rose-500/90 px-1.5 py-0.5 text-[10px] font-semibold text-white">
            gone
          </span>
        )}
        {selectable && (
          <>
            {selected && (
              <span className="pointer-events-none absolute inset-0 bg-teal-400/15 ring-2 ring-inset ring-teal-400" />
            )}
            <span
              className={`pointer-events-none absolute right-2 top-2 z-30 grid h-6 w-6 place-items-center rounded-full border text-xs font-bold transition ${
                selected
                  ? "border-teal-400 bg-teal-500 text-[#0b0e14]"
                  : "border-white/50 bg-black/60 text-transparent"
              }`}
            >
              <Check className="h-3.5 w-3.5" />
            </span>
            <button
              type="button"
              aria-label={selected ? "Deselect item" : "Select item"}
              onClick={() => onToggleSelect?.(item.slug)}
              draggable={!!onDragStartItems}
              onDragStart={(e) => onDragStartItems?.(item.slug, e)}
              className="absolute inset-0 z-20"
            />
          </>
        )}
      </div>

      <div className="p-3">
        <div className="flex items-center justify-between text-[11px] text-slate-500">
          <span>{kindLabel} · {fmtDate(item.createdAt)}</span>
          <span>{item.fileSize ? fmtSize(item.fileSize) : ""}</span>
        </div>

        {editing && canEdit ? (
          <div className="mt-2 flex gap-1.5">
            <input
              value={caption}
              onChange={(e) => setCaption(e.target.value)}
              placeholder="Caption…"
              className="min-w-0 flex-1 rounded-lg px-2 py-1 text-xs outline-none focus:border-control-ink"
              onKeyDown={(e) => {
                if (e.key === "Enter") void patchCaption(caption);
                if (e.key === "Escape") setEditing(false);
              }}
            />
            <button
              disabled={busy}
              onClick={() => void patchCaption(caption)}
              className="rounded-lg px-2 py-1 text-xs font-semibold"
            >
              <Check className="h-3.5 w-3.5" />
            </button>
          </div>
        ) : canEdit ? (
          <button
            onClick={() => setEditing(true)}
            className="mt-1 block w-full truncate text-left text-sm text-slate-200 hover:text-white"
            title="Click to edit caption"
          >
            {caption || <span className="text-slate-500">Add a caption…</span>}
          </button>
        ) : (
          caption && <p className="mt-1 truncate text-left text-sm text-slate-200">{caption}</p>
        )}

        {(canShareLink || canShareFriend || canDeleteItem) && (
          <div className="mt-2.5 flex flex-wrap items-center gap-1.5">
            {canShareLink && (
              <>
                <button
                  disabled={busy}
                  onClick={shareLink}
                  title="Copy public link"
                  aria-label="Copy public link"
                  className="inline-flex items-center gap-1 rounded-lg px-2.5 py-1.5 text-xs transition"
                >
                  <Link2 className="h-3.5 w-3.5" />
                  {!compact && "Copy link"}
                </button>
                <button
                  disabled={busy}
                  onClick={() => void toggleLinks()}
                  title="Manage share links"
                  aria-label="Manage share links"
                  aria-expanded={linksOpen}
                  className={`inline-flex items-center gap-1 rounded-lg px-2.5 py-1.5 text-xs transition ${
                    linksOpen ? "bg-control-ink text-control" : ""
                  }`}
                >
                  <Eye className="h-3.5 w-3.5" />
                  {!compact && "Links"}
                </button>
              </>
            )}
            {canShareFriend && (
              <div className="relative">
                <Send className="pointer-events-none absolute left-2 top-1/2 h-3.5 w-3.5 -translate-y-1/2 text-slate-400" />
                <ChevronDown className="pointer-events-none absolute right-1.5 top-1/2 h-3.5 w-3.5 -translate-y-1/2 text-slate-500" />
                <select
                  disabled={busy || friends.length === 0}
                  value=""
                  title="Send to a friend"
                  aria-label="Send to a friend"
                  onChange={(e) => {
                    const id = Number(e.target.value);
                    if (id) void shareTo(id);
                  }}
                  className="cursor-pointer appearance-none rounded-lg py-1.5 pl-7 pr-6 text-xs transition disabled:opacity-40"
                >
                  <option value="">
                    {friends.length === 0 ? "No friends" : compact ? "" : "Send to…"}
                  </option>
                  {friends.map((f) => (
                    <option key={f.id} value={f.id} className="bg-menu">
                      {f.name}
                    </option>
                  ))}
                </select>
              </div>
            )}
            {canDeleteItem && (
              <button
                disabled={busy}
                onClick={() => setConfirmDelete(true)}
                title="Delete"
                aria-label="Delete"
                className="ml-auto inline-flex items-center justify-center rounded-lg px-2.5 py-1.5 text-xs text-rose-600 transition hover:bg-rose-500/10"
              >
                {compact ? <Trash2 className="h-3.5 w-3.5" /> : "Delete"}
              </button>
            )}
          </div>
        )}

        {linksOpen && links !== null && (
          <div className="mt-2 space-y-1.5 rounded-xl border border-white/10 bg-black/30 p-2">
            {links.length === 0 ? (
              <p className="px-1 py-1 text-[11px] text-slate-500">No active links.</p>
            ) : (
              links.map((l) => (
                <div key={l.token} className="flex items-center gap-2 text-[11px]">
                  <div className="min-w-0 flex-1">
                    <div className="flex items-center gap-1.5 text-slate-400">
                      <span>{l.views} view{l.views === 1 ? "" : "s"}</span>
                      <span>·</span>
                      <span>{l.likes} like{l.likes === 1 ? "" : "s"}</span>
                      <span>·</span>
                      <span>
                        {l.expiresAt
                          ? `exp ${new Date(l.expiresAt * 1000).toLocaleDateString(undefined, { month: "short", day: "numeric" })}`
                          : "no expiry"}
                      </span>
                    </div>
                    <div className="truncate font-mono text-slate-500">/s/{l.token.slice(0, 10)}…</div>
                  </div>
                  <button
                    disabled={revoking === l.token}
                    onClick={async () => {
                      try {
                        await navigator.clipboard.writeText(l.url);
                        setActionMsg({ text: "Link copied", ok: true });
                      } catch {
                        setActionMsg({ text: l.url, ok: true });
                      }
                    }}
                    className="rounded-md px-2 py-1 transition"
                  >
                    Copy
                  </button>
                  <button
                    disabled={revoking === l.token}
                    onClick={() => void revokeLink(l.token)}
                    title="Revoke link"
                    className="rounded-md px-2 py-1 text-rose-600 transition hover:bg-rose-500/10 disabled:opacity-40"
                  >
                    {revoking === l.token ? "…" : "Revoke"}
                  </button>
                </div>
              ))
            )}
          </div>
        )}

        {actionMsg && (
          <p
            className={`mt-2 flex items-center gap-1 break-all text-[11px] ${
              actionMsg.ok ? "text-teal-300" : "text-rose-400"
            }`}
          >
            {actionMsg.ok && <Check className="h-3 w-3 shrink-0" />}
            {actionMsg.text}
          </p>
        )}
      </div>

      {confirmDelete && (
        <ConfirmDialog
          title="Delete this item?"
          confirmLabel="Delete"
          danger
          busy={busy}
          onConfirm={() => void doDelete()}
          onCancel={() => setConfirmDelete(false)}
        >
          {caption ? `“${caption.slice(0, 60)}”` : "It"} leaves your vault index — the
          original file stays inside Telegram. This can&apos;t be undone.
        </ConfirmDialog>
      )}
    </div>
  );
}
