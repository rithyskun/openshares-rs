"use client";

import { useState } from "react";
import { Check, Download, Heart, Send } from "lucide-react";
import { apiFetch } from "@/lib/apiFetch";

/**
 * Instagram-style action row for /s/<token>: like heart, copy-link plane,
 * download icon — with the live "N likes" line underneath. One like per
 * visitor (server cookie-dedupes); once liked the heart stays filled.
 */
export function ShareActions({
  likeEndpoint,
  initialLikes,
  initialLiked = false,
  downloadHref = null,
}: {
  likeEndpoint: string;
  initialLikes: number;
  initialLiked?: boolean;
  downloadHref?: string | null;
}) {
  const [likes, setLikes] = useState(initialLikes);
  const [liked, setLiked] = useState(initialLiked);
  const [busy, setBusy] = useState(false);
  const [copied, setCopied] = useState(false);

  async function like() {
    if (liked || busy) return;
    setBusy(true);
    try {
      const res = await apiFetch(likeEndpoint, { method: "POST" });
      if (res.ok) {
        const data = (await res.json().catch(() => ({}))) as { likes?: number };
        setLikes(typeof data.likes === "number" ? data.likes : likes + 1);
        setLiked(true);
      }
    } finally {
      setBusy(false);
    }
  }

  async function copyLink() {
    try {
      await navigator.clipboard.writeText(window.location.href);
      setCopied(true);
      setTimeout(() => setCopied(false), 1500);
    } catch {
      // clipboard unavailable — nothing to show
    }
  }

  // Chromeless glyphs over the always-black share page — text-white/90 is
  // literal (text-white would follow the app theme and vanish on black).
  const iconBtn =
    "grid h-9 w-9 place-items-center rounded-full border-0 bg-transparent text-white/90 transition hover:bg-white/10";

  return (
    <div>
      <div className="flex items-center gap-1 px-3 pb-1 pt-2">
        <button
          type="button"
          onClick={() => void like()}
          disabled={busy || liked}
          aria-pressed={liked}
          aria-label={liked ? "Liked" : "Like"}
          className={`${iconBtn} ${liked ? "text-rose-500" : ""} disabled:cursor-default`}
        >
          <Heart className={`h-6 w-6 ${liked ? "fill-current" : ""}`} />
        </button>
        <button
          type="button"
          onClick={() => void copyLink()}
          aria-label="Copy link"
          title="Copy link"
          className={iconBtn}
        >
          {copied ? <Check className="h-6 w-6 text-teal-400" /> : <Send className="h-6 w-6" />}
        </button>
        <span className="flex-1" />
        {downloadHref && (
          <a
            href={downloadHref}
            aria-label="Save to device"
            title="Save to device"
            className={iconBtn}
          >
            <Download className="h-6 w-6" />
          </a>
        )}
      </div>
      <p className="px-3 pb-1 text-sm font-semibold text-white">
        {likes} like{likes === 1 ? "" : "s"}
      </p>
    </div>
  );
}
