"use client";

import Link from "next/link";
import { useState } from "react";
import { Check, Globe, Images, Link2, Send } from "lucide-react";
import { useRouter } from "next/navigation";
import { ConfirmDialog } from "./ConfirmDialog";
import { apiFetch } from "@/lib/apiFetch";
import { usePermissions } from "./PermissionsProvider";

export type AlbumModel = {
  id: number;
  slug: string;
  ownerId: number | null;
  title: string;
  token: string;
  isPublic: boolean;
  publicExpiresAt: number | null;
  itemCount: number;
  coverSlug: string | null;
  createdAt: number;
  views?: number;
  likes?: number;
  botUsername?: string | null;
};

/** Share/stop-sharing + delete controls (used on the gallery card and the album page). */
export function AlbumActions({ album, compact = false }: { album: AlbumModel; compact?: boolean }) {
  const router = useRouter();
  const [isPublic, setIsPublic] = useState(album.isPublic);
  const [busy, setBusy] = useState(false);
  const [msg, setMsg] = useState<{ text: string; ok: boolean } | null>(null);
  const [deleted, setDeleted] = useState(false);
  const [confirm, setConfirm] = useState<"delete" | "stop" | null>(null);
  const { can } = usePermissions();

  async function copyLink(url: string, label: string) {
    setMsg(null);
    try {
      await navigator.clipboard.writeText(url);
      setMsg({ text: `${label} copied`, ok: true });
    } catch {
      setMsg({ text: url, ok: true });
    }
  }

  async function share() {
    setBusy(true);
    setMsg(null);
    const res = await apiFetch(`/api/albums/${album.slug}`, {
      method: "PATCH",
      headers: { "content-type": "application/json" },
      body: JSON.stringify({ isPublic: true }),
    });
    setBusy(false);
    if (!res.ok) return setMsg({ text: "Failed", ok: false });
    const data = (await res.json().catch(() => ({}))) as { expiresAt?: number | null };
    setIsPublic(true);
    const exp = data.expiresAt
      ? ` — expires ${new Date(data.expiresAt * 1000).toLocaleDateString(undefined, { month: "short", day: "numeric" })}`
      : "";
    await copyLink(`${window.location.origin}/a/${album.token}`, "Public link");
    setMsg((m) => (m?.ok ? { ...m, text: `${m.text}${exp}` } : m));
    router.refresh();
  }

  async function doStopSharing() {
    setBusy(true);
    setMsg(null);
    const res = await apiFetch(`/api/albums/${album.slug}`, {
      method: "PATCH",
      headers: { "content-type": "application/json" },
      body: JSON.stringify({ isPublic: false }),
    });
    setBusy(false);
    if (!res.ok) {
      setConfirm(null);
      return setMsg({ text: "Failed", ok: false });
    }
    setConfirm(null);
    setIsPublic(false);
    setMsg({ text: "Sharing stopped", ok: true });
    router.refresh();
  }

  async function doDelete() {
    setBusy(true);
    const res = await apiFetch(`/api/albums/${album.slug}`, { method: "DELETE" });
    setBusy(false);
    if (res.ok) {
      setDeleted(true);
      router.refresh();
    } else {
      setConfirm(null);
      setMsg({ text: "Delete failed", ok: false });
    }
  }

  if (deleted) return null;

  const telegramUrl = album.botUsername
    ? `https://t.me/${album.botUsername}?start=album_${album.token}`
    : null;

  // UX gate — share/stop needs album:edit (+share:album for publishing),
  // delete needs album:delete, both scoped to the album's owner.
  const canEdit = can("album:edit", album.ownerId);
  const canShare = canEdit && can("share:album");
  const canDelete = can("album:delete", album.ownerId);
  if (!canEdit && !canDelete) return null;

  return (
    <div className="flex flex-wrap items-center gap-1.5">
      {canEdit && (
        isPublic ? (
          <>
            <button
              disabled={busy}
              onClick={() => void copyLink(`${window.location.origin}/a/${album.token}`, "Web link")}
              className="inline-flex items-center gap-1 rounded-lg px-2 py-1 text-xs font-medium transition"
            >
              <Link2 className="h-3.5 w-3.5" />
              Copy web link
            </button>
            {telegramUrl && (
              <button
                disabled={busy}
                onClick={() => void copyLink(telegramUrl, "Telegram link")}
                title="Friends who open this in Telegram get the album delivered by the bot"
                className="inline-flex items-center gap-1 rounded-lg px-2 py-1 text-xs font-medium text-sky-600 transition hover:bg-sky-500/10"
              >
                <Send className="h-3.5 w-3.5" />
                Telegram link
              </button>
            )}
            {canShare && (
              <button
                disabled={busy}
                onClick={() => setConfirm("stop")}
                className="rounded-lg px-2 py-1 text-xs transition"
              >
                Stop
              </button>
            )}
          </>
        ) : (
          canShare && (
            <button
              disabled={busy}
              onClick={() => void share()}
              className="inline-flex items-center gap-1 rounded-lg px-2 py-1 text-xs font-medium transition"
            >
              <Globe className="h-3.5 w-3.5" />
              Share to public
            </button>
          )
        )
      )}
      {!compact && canDelete && (
        <button
          disabled={busy}
          onClick={() => setConfirm("delete")}
          className="rounded-lg px-2 py-1 text-xs text-rose-600 transition hover:bg-rose-500/10"
        >
          Delete
        </button>
      )}
      {msg && (
        <span
          className={`inline-flex items-center gap-1 break-all text-[11px] ${
            msg.ok ? "text-teal-300" : "text-rose-400"
          }`}
        >
          {msg.ok && <Check className="h-3 w-3 shrink-0" />}
          {msg.text}
        </span>
      )}

      {confirm === "delete" && (
        <ConfirmDialog
          title={`Delete album “${album.title.slice(0, 40)}”?`}
          confirmLabel="Delete album"
          danger
          busy={busy}
          onConfirm={() => void doDelete()}
          onCancel={() => setConfirm(null)}
        >
          The album grouping is removed — its {album.itemCount} photo
          {album.itemCount === 1 ? "" : "s"}/video{album.itemCount === 1 ? "" : "s"} stay
          in your vault and in Telegram.
        </ConfirmDialog>
      )}
      {confirm === "stop" && (
        <ConfirmDialog
          title="Stop sharing this album?"
          confirmLabel="Stop sharing"
          danger
          busy={busy}
          onConfirm={() => void doStopSharing()}
          onCancel={() => setConfirm(null)}
        >
          The public web link and the Telegram deep link stop working
          immediately.
        </ConfirmDialog>
      )}
    </div>
  );
}

/** Gallery tile for an album — the cover doubles as a drop target for media dragged from the grid. */
export function AlbumCard({
  album,
  droppable = false,
  selectable = false,
  selected = false,
  onToggleSelect,
}: {
  album: AlbumModel;
  droppable?: boolean;
  selectable?: boolean;
  selected?: boolean;
  onToggleSelect?: (slug: string) => void;
}) {
  const router = useRouter();
  const coverSrc = album.coverSlug ? `/api/media/${album.coverSlug}?thumb=1` : null;
  const [dropActive, setDropActive] = useState(false);
  const [dropMsg, setDropMsg] = useState<{ text: string; ok: boolean } | null>(null);

  async function onDrop(e: React.DragEvent) {
    e.preventDefault();
    setDropActive(false);
    let slugs: string[] = [];
    try {
      const parsed: unknown = JSON.parse(e.dataTransfer.getData("application/x-os-media"));
      if (Array.isArray(parsed)) slugs = parsed.filter((s): s is string => typeof s === "string");
    } catch {
      return; // not our drag payload
    }
    if (slugs.length === 0) return;
    setDropMsg({ text: `Moving ${slugs.length}…`, ok: true });
    try {
      const res = await apiFetch("/api/media/move", {
        method: "POST",
        headers: { "content-type": "application/json" },
        body: JSON.stringify({ slugs, albumId: album.id }),
      });
      const body = (await res.json().catch(() => ({}))) as { moved?: number; failed?: number; skipped?: number };
      if (!res.ok) {
        setDropMsg({ text: "Move failed", ok: false });
        return;
      }
      const moved = body.moved ?? 0;
      const skipped = body.skipped ?? 0;
      setDropMsg(
        moved === 0 && skipped > 0
          ? { text: "Already in this album", ok: true }
          : {
              text: `Moved ${moved} item${moved === 1 ? "" : "s"}${body.failed ? ` — ${body.failed} failed` : ""}`,
              ok: !body.failed,
            }
      );
      router.refresh();
    } catch {
      setDropMsg({ text: "Move failed", ok: false });
    }
  }

  return (
    <div
      className={`overflow-hidden rounded-2xl border bg-white/3 transition ${
        dropActive ? "border-teal-400/60 ring-2 ring-teal-400/60" : "border-white/5 hover:border-white/10"
      }`}
    >
      <Link
        href={`/album/${album.slug}`}
        className="relative block"
        onClick={
          selectable
            ? (e) => {
                e.preventDefault();
                onToggleSelect?.(album.slug);
              }
            : undefined
        }
        onDragOver={
          droppable
            ? (e) => {
                e.preventDefault();
                e.dataTransfer.dropEffect = "move";
                setDropActive(true);
              }
            : undefined
        }
        onDragLeave={
          droppable
            ? (e) => {
                if (!e.currentTarget.contains(e.relatedTarget as Node)) setDropActive(false);
              }
            : undefined
        }
        onDrop={droppable ? (e) => void onDrop(e) : undefined}
      >
        <div className="relative aspect-4/3 w-full bg-black/40">
          {coverSrc ? (
            // eslint-disable-next-line @next/next/no-img-element
            <img src={coverSrc} alt={album.title} className="h-full w-full object-cover" loading="lazy" />
          ) : (
            <div className="grid h-full place-items-center text-slate-500">
              <Images className="h-8 w-8" />
            </div>
          )}
          <span className="absolute right-2 top-2 rounded-md bg-black/70 px-1.5 py-0.5 text-[10px] font-medium text-white">
            {album.itemCount} item{album.itemCount === 1 ? "" : "s"}
            {(album.views ?? 0) > 0 && ` · ${album.views} view${album.views === 1 ? "" : "s"}`}
            {(album.likes ?? 0) > 0 && ` · ${album.likes} like${album.likes === 1 ? "" : "s"}`}
          </span>
          {selectable && (
            <>
              {selected && (
                <span className="pointer-events-none absolute inset-0 bg-teal-400/15 ring-2 ring-inset ring-teal-400" />
              )}
              <span
                className={`pointer-events-none absolute left-2 top-2 z-30 grid h-6 w-6 place-items-center rounded-full border transition ${
                  selected
                    ? "border-teal-400 bg-teal-500 text-[#0b0e14]"
                    : "border-white/50 bg-black/60 text-transparent"
                }`}
              >
                <Check className="h-3.5 w-3.5" />
              </span>
            </>
          )}
        </div>
        <div className="px-3 pt-2.5">
          <div className="truncate text-sm font-medium text-slate-100">{album.title}</div>
          {dropMsg && (
            <div className={`mt-0.5 text-[11px] ${dropMsg.ok ? "text-teal-300" : "text-rose-400"}`}>
              {dropMsg.text}
            </div>
          )}
        </div>
      </Link>
      <div className="px-3 pb-3 pt-1.5">
        <AlbumActions album={album} compact />
      </div>
    </div>
  );
}
