"use client";

import { useCallback, useEffect, useRef, useState } from "react";
import { ChevronLeft, ChevronRight, Download, X } from "lucide-react";

export type ViewerMedia = {
  slug: string;
  kind: "photo" | "video";
  caption: string | null;
  availability: "available" | "telegram-only" | "dead";
};

const SWIPE_PX = 40;

/**
 * Fullscreen media lightbox shared by the vault grid (MediaGrid) and the
 * public album grid: swipe / arrow-key navigation that skips non-streamable
 * items, a save-to-device button, Esc + body scroll-lock. Callers supply URL
 * builders so the private grid hits `/api/media/:slug` and the public grid
 * appends its `?token=` share credential.
 */
export function MediaViewer({
  items,
  index,
  mediaUrl,
  downloadUrl,
  onNavigate,
  onClose,
  hasMore = false,
}: {
  items: ViewerMedia[];
  index: number;
  /** Full-res stream URL for an item. */
  mediaUrl: (item: ViewerMedia) => string;
  /** Download URL — usually mediaUrl plus a `download=1` flag. */
  downloadUrl: (item: ViewerMedia) => string;
  onNavigate: (index: number) => void;
  onClose: () => void;
  /** More items exist beyond the loaded tail — don't wrap to the start yet. */
  hasMore?: boolean;
}) {
  const [imgReady, setImgReady] = useState(false);
  const touchX = useRef<number | null>(null);

  const total = items.length;
  const cur = items[index];
  const openableCount = items.filter((m) => m.availability === "available").length;

  const step = useCallback(
    (d: number) => {
      setImgReady(false);
      // Skip non-streamable tiles — they can't render in the viewer.
      let j = index;
      for (let n = 0; n < total; n++) {
        j = (j + d + total) % total;
        if (items[j].availability === "available") break;
      }
      // Forward past the loaded tail would wrap to index 0 — hold instead while
      // more pages exist; the parent's prefetch lands them shortly.
      if (d > 0 && j <= index && hasMore) return;
      onNavigate(j);
    },
    [index, items, total, onNavigate, hasMore]
  );

  // Esc/arrows + body scroll-lock while the viewer is open.
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") onClose();
      if (e.key === "ArrowLeft") step(-1);
      if (e.key === "ArrowRight") step(1);
    };
    document.body.style.overflow = "hidden";
    window.addEventListener("keydown", onKey);
    return () => {
      document.body.style.overflow = "";
      window.removeEventListener("keydown", onKey);
    };
  }, [step, onClose]);

  return (
    <div
      className="fixed inset-0 z-50 flex flex-col bg-black/95 backdrop-blur-sm"
      role="dialog"
      aria-modal="true"
      onTouchStart={(e) => {
        touchX.current = e.touches[0].clientX;
      }}
      onTouchEnd={(e) => {
        if (touchX.current === null) return;
        const dx = e.changedTouches[0].clientX - touchX.current;
        if (Math.abs(dx) >= SWIPE_PX) step(dx > 0 ? -1 : 1);
        touchX.current = null;
      }}
    >
      <div className="flex items-center justify-between gap-2 p-3 text-sm text-slate-300">
        <span className="px-1 tabular-nums">
          {index + 1} / {total}
        </span>
        <div className="flex items-center gap-2">
          <a
            href={downloadUrl(cur)}
            className="grid h-10 w-10 place-items-center rounded-full border border-white/15 transition hover:bg-white/10"
            aria-label="Save to device"
          >
            <Download className="h-4 w-4" />
          </a>
          <button
            onClick={onClose}
            className="grid h-10 w-10 place-items-center rounded-full border-0 bg-transparent text-white/90 transition hover:bg-white/10"
            aria-label="Close viewer"
          >
            <X className="h-4 w-4" />
          </button>
        </div>
      </div>

      <div className="relative flex min-h-0 flex-1 items-center justify-center">
        {openableCount > 1 && (
          <button
            onClick={() => step(-1)}
            aria-label="Previous"
            className="absolute left-1 z-10 grid h-12 w-12 place-items-center rounded-full bg-black/40 text-white/90 transition hover:bg-white/10 sm:left-3"
          >
            <ChevronLeft className="h-6 w-6" />
          </button>
        )}
        {cur.kind === "photo" ? (
          <>
            {!imgReady && (
              <span className="absolute h-8 w-8 animate-spin rounded-full border-2 border-white/10 border-t-teal-400" />
            )}
            {/* eslint-disable-next-line @next/next/no-img-element */}
            <img
              src={mediaUrl(cur)}
              alt={cur.caption ?? "Photo"}
              onLoad={() => setImgReady(true)}
              className={`max-h-full max-w-full object-contain transition-opacity duration-200 ${
                imgReady ? "opacity-100" : "opacity-0"
              }`}
            />
          </>
        ) : (
          <video
            key={cur.slug}
            src={mediaUrl(cur)}
            controls
            playsInline
            autoPlay
            className="max-h-full max-w-full bg-black"
          />
        )}
        {openableCount > 1 && (
          <button
            onClick={() => step(1)}
            aria-label="Next"
            className="absolute right-1 z-10 grid h-12 w-12 place-items-center rounded-full bg-black/40 text-white/90 transition hover:bg-white/10 sm:right-3"
          >
            <ChevronRight className="h-6 w-6" />
          </button>
        )}
      </div>

      <div className="min-h-12 px-4 pb-[calc(0.75rem+env(safe-area-inset-bottom))] pt-2 text-center text-sm text-slate-300">
        {cur.caption ?? ""}
      </div>
    </div>
  );
}
