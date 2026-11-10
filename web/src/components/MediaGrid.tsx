"use client";

import { useEffect, useRef, useState } from "react";
import { useRouter } from "next/navigation";
import { ChevronDown, Download, FolderInput, FolderOutput, Loader2, SquareCheck, Trash2 } from "lucide-react";
import { MediaCard, type Friend, type MediaItem } from "./MediaCard";
import { MediaViewer } from "./MediaViewer";
import { ConfirmDialog } from "./ConfirmDialog";
import { GRID_CLASSES, SizeToggle, useGridSize } from "./GridControls";
import { apiFetch } from "@/lib/apiFetch";
import { pool } from "@/lib/pool";
import { usePermissions } from "./PermissionsProvider";

/**
 * Media grid with server-driven paging: the page SSRs the first 60 rows, this
 * component owns the accumulated list — debounced search/kind/gone filters
 * refetch page 1, an IntersectionObserver sentinel appends the rest. Selection,
 * batch download/delete/move and the viewer all operate on loaded items.
 */

type Filters = { q: string; kind: "" | "photo" | "video"; missing: boolean };

const PAGE_SIZE = 60;
const DEFAULT_FILTERS: Filters = { q: "", kind: "", missing: false };

export function MediaGrid({
  scope,
  initialItems,
  initialCursor,
  friends,
  canDelete,
  albums,
  inAlbum = false,
}: {
  /** "vault" or "album:<slug>" — selects the /api/media scope. */
  scope: string;
  initialItems: MediaItem[];
  initialCursor: string | null;
  friends: Friend[];
  canDelete: boolean;
  /** Gallery: albums that selected items can be moved into (also enables drag & drop targets). */
  albums?: { id: number; slug: string; title: string }[];
  /** Album page: enables "Remove from album" (moves items back to the vault). */
  inAlbum?: boolean;
}) {
  const router = useRouter();
  const [selecting, setSelecting] = useState(false);
  const [selected, setSelected] = useState<Set<string>>(new Set());
  const [removed, setRemoved] = useState<Set<string>>(new Set());
  const [confirm, setConfirm] = useState<"delete" | "download" | null>(null);
  const [busy, setBusy] = useState(false);
  const [progress, setProgress] = useState<string | null>(null);
  const [notice, setNotice] = useState<string | null>(null);
  const [open, setOpen] = useState<number | null>(null);
  const { size, pickSize } = useGridSize();
  const { canAny } = usePermissions();
  const canMove = canAny("media:edit");

  // ── Server-driven list state ────────────────────────────────────────────
  const [items, setItems] = useState<MediaItem[]>(initialItems);
  const [cursor, setCursor] = useState<string | null>(initialCursor);
  const [resultFilters, setResultFilters] = useState<Filters>(DEFAULT_FILTERS);
  const [query, setQuery] = useState("");
  const [debouncedQ, setDebouncedQ] = useState("");
  const [kind, setKind] = useState<"" | "photo" | "video">("");
  const [missingOnly, setMissingOnly] = useState(false);
  const [loading, setLoading] = useState(false);
  const sentinelRef = useRef<HTMLDivElement>(null);

  useEffect(() => {
    const t = setTimeout(() => setDebouncedQ(query.trim()), 300);
    return () => clearTimeout(t);
  }, [query]);

  const want: Filters = { q: debouncedQ, kind, missing: missingOnly };
  const dirty =
    want.q !== resultFilters.q || want.kind !== resultFilters.kind || want.missing !== resultFilters.missing;

  async function fetchPage(cur: string | null, f: Filters, signal?: AbortSignal) {
    const params = new URLSearchParams({ scope, limit: String(PAGE_SIZE) });
    if (f.q) params.set("q", f.q);
    if (f.kind) params.set("kind", f.kind);
    if (f.missing) params.set("missing", "1");
    if (cur) params.set("cursor", cur);
    const res = await apiFetch(`/api/media?${params.toString()}`, { signal });
    if (!res.ok) throw new Error(`media list failed (${res.status})`);
    return (await res.json()) as { items: MediaItem[]; nextCursor: string | null };
  }

  // Filter change → restart from page 1 (aborts superseded requests).
  useEffect(() => {
    if (!dirty) return;
    const ac = new AbortController();
    setLoading(true);
    fetchPage(null, want, ac.signal)
      .then((data) => {
        setItems(data.items);
        setCursor(data.nextCursor);
        setResultFilters(want);
      })
      .catch(() => {
        /* aborted or transient — keep current list */
      })
      .finally(() => setLoading(false));
    return () => ac.abort();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [dirty, debouncedQ, kind, missingOnly, scope]);

  function loadMore() {
    if (!cursor || loading || dirty) return;
    setLoading(true);
    fetchPage(cursor, resultFilters)
      .then((data) => {
        setItems((prev) => [...prev, ...data.items]);
        setCursor(data.nextCursor);
      })
      .catch(() => {})
      .finally(() => setLoading(false));
  }

  // Infinite scroll — append the next page when the sentinel nears view.
  // Browsers without IntersectionObserver get an explicit Load more button.
  const ioSupported = typeof IntersectionObserver !== "undefined";
  useEffect(() => {
    const el = sentinelRef.current;
    if (!el || !cursor || loading || dirty || !ioSupported) return;
    const obs = new IntersectionObserver(
      (entries) => {
        if (!entries[0].isIntersecting) return;
        obs.disconnect();
        loadMore();
      },
      { rootMargin: "800px" }
    );
    obs.observe(el);
    return () => obs.disconnect();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [cursor, loading, dirty, resultFilters, scope, ioSupported]);

  const visible = items.filter((i) => !removed.has(i.slug));

  // Feed-style viewer: as the open index nears the loaded tail, pull the next
  // page so swiping/arrows keep advancing (loadMore self-guards).
  useEffect(() => {
    if (open === null || !cursor || loading || dirty) return;
    if (open >= visible.length - 8) loadMore();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [open, visible.length, cursor, loading, dirty]);

  // router.refresh() (post-move) delivers a fresh SSR page — resync to it.
  // Resetting resultFilters makes a non-default filter set refetch via `dirty`.
  useEffect(() => {
    setItems(initialItems);
    setCursor(initialCursor);
    setResultFilters(DEFAULT_FILTERS);
    setRemoved(new Set());
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [initialItems, initialCursor]);

  const chosen = visible.filter((i) => selected.has(i.slug));
  const selBytes = chosen.reduce((s, i) => s + (i.fileSize ?? 0), 0);
  const allSelected = visible.length > 0 && selected.size === visible.length;
  const filtering = resultFilters.q !== "" || resultFilters.kind !== "" || resultFilters.missing;

  function toggle(slug: string) {
    setSelected((prev) => {
      const next = new Set(prev);
      if (next.has(slug)) next.delete(slug);
      else next.add(slug);
      return next;
    });
  }

  function exitSelect() {
    setSelecting(false);
    setSelected(new Set());
  }

  /** Batch move via POST /api/media/move — one call carries the whole selection. */
  async function doMove(albumId: number | null, albumTitle?: string) {
    setBusy(true);
    const slugs = [...selected];
    setProgress(`Moving ${slugs.length}…`);
    let moved = 0;
    let failed = 0;
    let skipped = 0;
    try {
      const res = await apiFetch("/api/media/move", {
        method: "POST",
        headers: { "content-type": "application/json" },
        body: JSON.stringify({ slugs, albumId }),
      });
      if (res.ok) ({ moved, failed, skipped } = (await res.json()) as { moved: number; failed: number; skipped: number });
      else failed = slugs.length;
    } catch {
      failed = slugs.length;
    }
    setBusy(false);
    setProgress(null);
    exitSelect();
    setNotice(
      moved === 0 && skipped > 0
        ? "Those items are already in that album."
        : failed
          ? `Moved ${moved} — ${failed} couldn't be moved (permission or error).`
          : albumId
            ? `Moved ${moved} item${moved === 1 ? "" : "s"} to “${albumTitle}”.`
            : `Removed ${moved} item${moved === 1 ? "" : "s"} from the album.`
    );
    // Moved items leave this scope — refresh pulls the corrected first page.
    if (moved > 0) router.refresh();
  }

  /** Drag payload: a selected card drags its whole selection, others drag themselves. */
  function handleDragStart(slug: string, e: React.DragEvent) {
    const slugs = selected.has(slug) ? [...selected] : [slug];
    e.dataTransfer.setData("application/x-os-media", JSON.stringify(slugs));
    e.dataTransfer.effectAllowed = "move";
  }

  async function doDelete() {
    setBusy(true);
    const slugs = [...selected];
    const st = { ok: 0, fail: 0, done: 0 };
    // Pool of 4 parallel deletes — each is an independent RBAC-checked DELETE.
    await pool(slugs, 4, async (slug) => {
      try {
        const res = await apiFetch(`/api/media/${slug}`, { method: "DELETE" });
        if (res.ok) {
          st.ok++;
          setRemoved((prev) => new Set(prev).add(slug));
        } else {
          st.fail++;
        }
      } catch {
        st.fail++;
      }
      st.done++;
      setProgress(`Deleting ${st.done}/${slugs.length}…`);
    });
    const { ok, fail } = st;
    setBusy(false);
    setProgress(null);
    setConfirm(null);
    exitSelect();
    setNotice(
      fail
        ? `Deleted ${ok} — ${fail} couldn't be removed (permission or error).`
        : `Deleted ${ok} item${ok === 1 ? "" : "s"}.`
    );
  }

  async function doDownload() {
    setBusy(true);
    let n = 0;
    for (const slug of selected) {
      n++;
      setProgress(`Downloading ${n}/${selected.size}…`);
      const a = document.createElement("a");
      a.href = `/api/media/${slug}?download=1`;
      a.download = "";
      document.body.appendChild(a);
      a.click();
      a.remove();
      // Browsers throttle rapid downloads — a short gap keeps them all flowing.
      await new Promise((r) => setTimeout(r, 400));
    }
    const count = selected.size;
    setBusy(false);
    setProgress(null);
    setConfirm(null);
    exitSelect();
    setNotice(`Started ${count} download${count === 1 ? "" : "s"} — check your downloads folder.`);
  }

  return (
    <div>
      <div className="mb-3 flex flex-wrap items-center gap-2">
        <button
          onClick={selecting ? exitSelect : () => setSelecting(true)}
          className={`inline-flex items-center gap-1.5 rounded-lg px-3 py-1.5 text-xs font-medium transition ${
            selecting ? "bg-control-ink text-control" : ""
          }`}
        >
          {selecting ? "Done" : (
            <>
              <SquareCheck className="h-3.5 w-3.5" />
              Select
            </>
          )}
        </button>
        <input
          value={query}
          onChange={(e) => setQuery(e.target.value)}
          placeholder="Search captions…"
          aria-label="Search media by caption"
          className="min-w-0 flex-1 rounded-lg px-3 py-1.5 text-xs transition focus:border-control-ink focus:outline-none"
        />
        <div className="relative">
          <select
            value={kind}
            onChange={(e) => setKind(e.target.value as "" | "photo" | "video")}
            aria-label="Filter by media type"
            className="cursor-pointer appearance-none rounded-lg py-1.5 pl-2.5 pr-7 text-xs transition"
          >
            <option value="" className="bg-menu">All types</option>
            <option value="photo" className="bg-menu">Photos</option>
            <option value="video" className="bg-menu">Videos</option>
          </select>
          <ChevronDown className="pointer-events-none absolute right-2 top-1/2 h-3.5 w-3.5 -translate-y-1/2 text-slate-500" />
        </div>
        <button
          onClick={() => setMissingOnly((v) => !v)}
          aria-pressed={missingOnly}
          title="Show only items whose Telegram file is gone"
          className={`rounded-lg px-2.5 py-1.5 text-xs transition ${
            missingOnly ? "bg-rose-500/10 text-rose-600" : ""
          }`}
        >
          Gone
        </button>
        <SizeToggle size={size} onPick={pickSize} />
        {selecting && (
          <div className="flex items-center gap-2 text-xs">
            <button
              onClick={() =>
                setSelected(allSelected ? new Set() : new Set(visible.map((i) => i.slug)))
              }
              className="rounded-lg px-2.5 py-1.5"
            >
              {allSelected ? "Clear" : "All"}
            </button>
            <span className="text-slate-500">{selected.size} selected</span>
          </div>
        )}
      </div>

      {visible.length === 0 && !loading && (
        <p className="rounded-2xl border border-white/5 bg-white/2 px-4 py-8 text-center text-sm text-slate-400">
          {filtering ? "Nothing matches those filters." : "Nothing here."}
        </p>
      )}
      <div className={`grid ${GRID_CLASSES[size]}`}>
        {visible.map((m, i) => (
          <MediaCard
            key={m.slug}
            item={m}
            friends={friends}
            selectable={selecting}
            selected={selected.has(m.slug)}
            onToggleSelect={toggle}
            compact={size === "sm"}
            onDragStartItems={handleDragStart}
            onView={m.availability === "available" ? () => setOpen(i) : undefined}
          />
        ))}
      </div>

      {(cursor || loading) && (
        <div ref={sentinelRef} className="flex items-center justify-center gap-3 py-6">
          {loading && <Loader2 className="h-5 w-5 animate-spin text-slate-500" />}
          {cursor && !ioSupported && !loading && (
            <button
              onClick={loadMore}
              className="rounded-lg px-4 py-2 text-xs transition"
            >
              Load more
            </button>
          )}
        </div>
      )}

      {notice && <p className="mt-3 text-xs text-teal-300">{notice}</p>}

      {open !== null && visible[open] && (
        <MediaViewer
          items={visible}
          index={open}
          mediaUrl={(m) => `/api/media/${m.slug}`}
          downloadUrl={(m) => `/api/media/${m.slug}?download=1`}
          onNavigate={setOpen}
          onClose={() => setOpen(null)}
          hasMore={!!cursor}
        />
      )}

      {selecting && selected.size > 0 && (
        <div className="fixed inset-x-4 bottom-[calc(4.75rem+env(safe-area-inset-bottom))] z-40 mx-auto flex max-w-md items-center gap-2 rounded-2xl border border-white/10 bg-menu/95 p-2 shadow-2xl backdrop-blur sm:bottom-6">
          <span className="shrink-0 px-2 text-xs text-slate-400">
            {selected.size} · {(selBytes / 1048576).toFixed(1)} MB
          </span>
          <button
            onClick={() => setConfirm("download")}
            className="inline-flex flex-1 items-center justify-center gap-1.5 rounded-xl px-3 py-2.5 text-sm font-semibold transition"
          >
            <Download className="h-4 w-4" />
            Download
          </button>
          {canMove && albums && albums.length > 0 && (
            <div className="relative shrink-0">
              <FolderInput className="pointer-events-none absolute left-2.5 top-1/2 h-4 w-4 -translate-y-1/2 text-slate-400" />
              <ChevronDown className="pointer-events-none absolute right-2 top-1/2 h-4 w-4 -translate-y-1/2 text-slate-500" />
              <select
                disabled={busy}
                value=""
                title="Move selected items into an album"
                aria-label="Move selected items into an album"
                onChange={(e) => {
                  const target = albums.find((a) => String(a.id) === e.target.value);
                  if (target) void doMove(target.id, target.title);
                }}
                className="h-full w-28 cursor-pointer appearance-none rounded-xl py-2.5 pl-8 pr-7 text-sm transition disabled:opacity-40"
              >
                <option value="">Move to…</option>
                {albums.map((a) => (
                  <option key={a.id} value={a.id} className="bg-menu">
                    {a.title}
                  </option>
                ))}
              </select>
            </div>
          )}
          {canMove && inAlbum && (
            <button
              disabled={busy}
              onClick={() => void doMove(null)}
              title="Move selected items back to the vault"
              className="inline-flex shrink-0 items-center justify-center gap-1.5 rounded-xl px-3 py-2.5 text-sm font-medium transition disabled:opacity-40"
            >
              <FolderOutput className="h-4 w-4" />
              Unfile
            </button>
          )}
          {canDelete && (
            <button
              onClick={() => setConfirm("delete")}
              className="inline-flex flex-1 items-center justify-center gap-1.5 rounded-xl bg-rose-500/90 px-3 py-2.5 text-sm font-semibold text-white transition hover:bg-rose-400"
            >
              <Trash2 className="h-4 w-4" />
              Delete
            </button>
          )}
        </div>
      )}

      {confirm === "download" && (
        <ConfirmDialog
          title={`Download ${selected.size} item${selected.size === 1 ? "" : "s"}?`}
          confirmLabel={`Download (${(selBytes / 1048576).toFixed(1)} MB)`}
          busy={busy}
          onConfirm={() => void doDownload()}
          onCancel={() => setConfirm(null)}
        >
          Files are fetched through your vault and saved to your device one after
          another. Items over 20&nbsp;MB stay Telegram-only and are skipped by the
          streamer.
          {progress && <p className="mt-2 text-teal-300">{progress}</p>}
        </ConfirmDialog>
      )}
      {confirm === "delete" && (
        <ConfirmDialog
          title={`Delete ${selected.size} item${selected.size === 1 ? "" : "s"}?`}
          confirmLabel="Delete"
          danger
          busy={busy}
          onConfirm={() => void doDelete()}
          onCancel={() => setConfirm(null)}
        >
          They leave your vault index — the original files stay inside Telegram.
          This can&apos;t be undone.
          {progress && <p className="mt-2 text-teal-300">{progress}</p>}
        </ConfirmDialog>
      )}
    </div>
  );
}
