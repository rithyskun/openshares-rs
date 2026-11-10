"use client";

import { useEffect, useRef, useState } from "react";
import { Ban, Download, Film, Loader2, Play } from "lucide-react";
import { MediaViewer } from "./MediaViewer";
import { apiFetch } from "@/lib/apiFetch";

export type PublicItem = {
  slug: string;
  kind: "photo" | "video";
  caption: string | null;
  availability: "available" | "telegram-only" | "dead";
};

/**
 * Public album grid — tiles open the shared MediaViewer lightbox, and the
 * header's "save all" walks every streamable item — the primary mobile flow
 * for share-link recipients (no tabs or new windows involved).
 */
export function PublicAlbumGrid({
  items: initialItems,
  initialCursor,
  itemCount,
  token,
}: {
  items: PublicItem[];
  initialCursor: string | null;
  itemCount: number;
  token: string;
}) {
  const [items, setItems] = useState(initialItems);
  const [cursor, setCursor] = useState<string | null>(initialCursor);
  const [loading, setLoading] = useState(false);
  const [open, setOpen] = useState<number | null>(null);
  const [saving, setSaving] = useState(false);
  const [progress, setProgress] = useState<string | null>(null);
  const [notice, setNotice] = useState<string | null>(null);
  const sentinelRef = useRef<HTMLDivElement>(null);

  async function fetchPage(cur: string): Promise<{ items: PublicItem[]; nextCursor: string | null }> {
    const res = await apiFetch(
      `/api/public/album?token=${encodeURIComponent(token)}&cursor=${encodeURIComponent(cur)}`
    );
    if (!res.ok) throw new Error(`album page failed (${res.status})`);
    return res.json() as Promise<{ items: PublicItem[]; nextCursor: string | null }>;
  }

  function loadMore() {
    if (!cursor || loading) return;
    setLoading(true);
    fetchPage(cursor)
      .then((data) => {
        setItems((prev) => [...prev, ...data.items]);
        setCursor(data.nextCursor);
      })
      .catch(() => {})
      .finally(() => setLoading(false));
  }

  // Infinite scroll — append the next page when the sentinel nears view.
  const ioSupported = typeof IntersectionObserver !== "undefined";
  useEffect(() => {
    const el = sentinelRef.current;
    if (!el || !cursor || loading || !ioSupported) return;
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
  }, [cursor, loading, ioSupported]);

  // Feed-style viewer: near the loaded tail, prefetch so swiping keeps going.
  useEffect(() => {
    if (open === null || !cursor || loading) return;
    if (open >= items.length - 8) loadMore();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [open, items.length, cursor, loading]);

  /** Drain every remaining page — used by Save all so nothing is skipped. */
  async function allItems(): Promise<PublicItem[]> {
    const list = [...items];
    let cur = cursor;
    while (cur) {
      const data = await fetchPage(cur);
      list.push(...data.items);
      cur = data.nextCursor;
    }
    if (list.length !== items.length) {
      setItems(list);
      setCursor(null);
    }
    return list;
  }

  const total = items.length;
  const mediaUrl = (m: PublicItem, extra = "") => `/api/media/${m.slug}?token=${token}${extra}`;
  const openableCount = items.filter((m) => m.availability === "available").length;
  const openable = (m: PublicItem) => m.availability === "available";

  async function downloadAll() {
    const list = (await allItems()).filter(openable);
    if (!list.length || saving) return;
    setSaving(true);
    setNotice(null);

    // Best path (Chromium): pick a folder once, stream every file straight to
    // disk — no per-file download prompts and no giant in-memory zip.
    const picker = (
      window as Window & { showDirectoryPicker?: () => Promise<FileSystemDirectoryHandle> }
    ).showDirectoryPicker;
    if (picker) {
      try {
        const dir = await picker.call(window);
        let n = 0;
        for (const m of list) {
          n++;
          setProgress(`Saving ${n}/${list.length}…`);
          try {
            const res = await fetch(mediaUrl(m));
            if (!res.ok || !res.body) continue;
            const ext = m.kind === "video" ? "mp4" : "jpg";
            const base =
              (m.caption ?? m.slug).replace(/[\\/:*?"<>|]/g, "_").trim().slice(0, 60) || m.slug;
            const fh = await dir.getFileHandle(
              `${String(n).padStart(3, "0")}-${base}.${ext}`,
              { create: true }
            );
            const w = await fh.createWritable();
            await res.body.pipeTo(w);
          } catch {
            // One bad file shouldn't sink the batch — keep going.
          }
        }
        setNotice(`Saved ${list.length} file${list.length === 1 ? "" : "s"} to your folder.`);
        setSaving(false);
        setProgress(null);
        return;
      } catch (err) {
        if ((err as DOMException)?.name === "AbortError") {
          // User cancelled the folder picker — stop quietly.
          setSaving(false);
          setProgress(null);
          return;
        }
        // FS API present but denied/failed — fall back to plain downloads.
      }
    }

    // Fallback: sequential browser downloads (Safari/Firefox/mobile).
    let n = 0;
    for (const m of list) {
      n++;
      setProgress(`Saving ${n}/${list.length}…`);
      const a = document.createElement("a");
      a.href = mediaUrl(m, "&download=1");
      a.download = "";
      document.body.appendChild(a);
      a.click();
      a.remove();
      // Browsers throttle rapid downloads — a short gap keeps them all flowing.
      await new Promise((r) => setTimeout(r, 500));
    }
    setSaving(false);
    setProgress(null);
    const skipped = total - list.length;
    setNotice(
      `Started ${list.length} download${list.length === 1 ? "" : "s"} — check your files.` +
        (skipped ? ` ${skipped} item${skipped === 1 ? "" : "s"} can't be saved here (over 20 MB or gone).` : "")
    );
  }

  return (
    <>
      {openableCount > 0 && (
        <div className="mb-3 flex items-center justify-between gap-2">
          <span className="text-xs text-slate-500">
            {openableCount}
            {cursor ? "+" : ""} of {itemCount} saveable
          </span>
          <button
            onClick={() => void downloadAll()}
            disabled={saving}
            className="inline-flex items-center gap-2 rounded-xl px-4 py-2 text-xs font-semibold transition disabled:opacity-50"
          >
            {saving ? (
              "Saving…"
            ) : (
              <>
                <Download className="h-3.5 w-3.5" />
                Save all to device
              </>
            )}
          </button>
        </div>
      )}
      {progress && <p className="mb-2 text-xs text-teal-300">{progress}</p>}
      {notice && <p className="mb-3 text-xs text-teal-300">{notice}</p>}
      <div className="grid grid-cols-2 gap-2 sm:grid-cols-3 sm:gap-3">
        {items.map((m, i) => (
          <figure
            key={m.slug}
            className="overflow-hidden rounded-2xl border border-white/10 bg-black/40"
          >
            {!openable(m) ? (
              <div className="flex aspect-square flex-col items-center justify-center gap-2 p-4 text-center text-xs text-amber-300">
                {m.availability === "dead" ? <Ban className="h-8 w-8" /> : <Film className="h-8 w-8" />}
                {m.availability === "dead"
                  ? "No longer available — the original Telegram file is gone."
                  : "Stored in Telegram — over 20 MB, not streamable here."}
              </div>
            ) : (
              <button
                type="button"
                onClick={() => setOpen(i)}
                className="relative block aspect-square w-full border-0"
              >
                {/* eslint-disable-next-line @next/next/no-img-element */}
                <img
                  src={mediaUrl(m, "&thumb=1")}
                  alt={m.caption ?? (m.kind === "video" ? "Video" : "Photo")}
                  loading="lazy"
                  className="h-full w-full object-cover"
                />
                {m.kind === "video" && (
                  <span className="absolute inset-0 grid place-items-center">
                    <span className="grid h-11 w-11 place-items-center rounded-full bg-black/60 text-white">
                      <Play className="h-5 w-5" fill="currentColor" />
                    </span>
                  </span>
                )}
              </button>
            )}
            {m.caption && (
              <figcaption className="truncate border-t border-white/5 px-3 py-2 text-xs text-slate-300">
                {m.caption}
              </figcaption>
            )}
          </figure>
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

      {open !== null && items[open] && (
        <MediaViewer
          items={items}
          index={open}
          mediaUrl={(m) => mediaUrl(m)}
          downloadUrl={(m) => mediaUrl(m, "&download=1")}
          onNavigate={setOpen}
          onClose={() => setOpen(null)}
          hasMore={!!cursor}
        />
      )}
    </>
  );
}
