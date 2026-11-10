"use client";

import { useState } from "react";
import { Heart } from "lucide-react";
import { apiFetch } from "@/lib/apiFetch";

/**
 * Heart button for public share pages (/s, /a). One like per visitor — the
 * server dedupes via cookie and returns the authoritative count; once liked
 * the button stays filled and ignores further taps.
 */
export function LikeButton({
  endpoint,
  initialLikes,
  initialLiked = false,
}: {
  endpoint: string;
  initialLikes: number;
  initialLiked?: boolean;
}) {
  const [likes, setLikes] = useState(initialLikes);
  const [liked, setLiked] = useState(initialLiked);
  const [busy, setBusy] = useState(false);

  async function like() {
    if (liked || busy) return;
    setBusy(true);
    try {
      const res = await apiFetch(endpoint, { method: "POST" });
      if (res.ok) {
        const data = (await res.json().catch(() => ({}))) as { likes?: number };
        setLikes(typeof data.likes === "number" ? data.likes : likes + 1);
        setLiked(true);
      }
    } finally {
      setBusy(false);
    }
  }

  return (
    <button
      type="button"
      onClick={() => void like()}
      disabled={busy || liked}
      aria-pressed={liked}
      className={`inline-flex items-center gap-1.5 rounded-full px-3.5 py-1.5 text-xs font-medium transition ${
        liked ? "bg-rose-500/10 text-rose-600" : ""
      } disabled:cursor-default`}
    >
      <Heart className={`h-3.5 w-3.5 ${liked ? "fill-current" : ""}`} />
      {likes} like{likes === 1 ? "" : "s"}
    </button>
  );
}
