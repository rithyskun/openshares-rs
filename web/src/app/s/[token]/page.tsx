import Link from "next/link";
import type { Metadata } from "next";
import { cache } from "react";
import { apiServerFetch } from "@/lib/api-server";
import { appUrl, siteName } from "@/lib/meta";
import { ArrowLeft, CircleHelp, Hourglass } from "lucide-react";
import { ShareViewer } from "@/components/ShareViewer";
import { ShareActions } from "@/components/ShareActions";
import { LogoMark } from "@/components/LogoMark";
import { hasLiked } from "@/lib/likes";

export const dynamic = "force-dynamic";

/**
 * `GET /api/public/share/{token}` — the page's share row (the old
 * SHARE_SELECT join, camelized at the edge like `/api/media`'s items).
 * 404 when no live 'link' share exists for the token; an existing but
 * expired share still returns 200 so the page can say "Link expired"
 * instead of "not found" — the client checks `expiresAt` itself.
 * The endpoint owns the `shares.views` increment the old page did inline.
 */
export type PublicShare = {
  slug: string;
  kind: "photo" | "video";
  caption: string | null;
  width: number | null;
  height: number | null;
  duration: number | null;
  fileSize: number | null;
  availability: string;
  ownerName: string | null;
  createdAt: number;
  expiresAt: number | null;
  likes: number;
};

const now = () => Math.floor(Date.now() / 1000);

/** Instagram-style compact age: "now", "5m", "3h", "2d", then a short date. */
function timeAgo(ts: number): string {
  const s = Math.max(0, now() - ts);
  if (s < 60) return "now";
  const m = Math.floor(s / 60);
  if (m < 60) return `${m}m`;
  const h = Math.floor(m / 60);
  if (h < 24) return `${h}h`;
  const d = Math.floor(h / 24);
  if (d < 7) return `${d}d`;
  return new Date(ts * 1000).toLocaleDateString(undefined, { month: "short", day: "numeric" });
}

/** Share row for OG metadata + the page — null on missing/gone links.
 *  cache() dedupes the generateMetadata + page fetch within one render. */
const fetchShare = cache(async function fetchShare(token: string): Promise<PublicShare | null> {
  const res = await apiServerFetch(`/api/public/share/${encodeURIComponent(token)}`);
  if (!res.ok) return null;
  return res.json() as Promise<PublicShare>;
});

export async function generateMetadata({ params }: { params: Promise<{ token: string }> }): Promise<Metadata> {
  const { token } = await params;
  const [row, name, origin] = await Promise.all([fetchShare(token), siteName(), appUrl()]);
  if (!row || (row.expiresAt !== null && row.expiresAt <= now())) return { title: name };
  const title = row.caption?.trim().slice(0, 90) || `${row.kind === "video" ? "Video" : "Photo"} shared via ${name}`;
  return {
    title,
    description: `Shared by ${row.ownerName || "someone"} via ${name}`,
    openGraph: {
      title,
      description: `${row.kind === "video" ? "Video" : "Photo"} shared by ${row.ownerName || "someone"} via ${name}`,
      images: [`${origin}/api/media/${row.slug}?token=${token}&thumb=1`],
      type: "website",
      siteName: name,
    },
    twitter: { card: "summary_large_image", title, images: [`${origin}/api/media/${row.slug}?token=${token}&thumb=1`] },
  };
}

export default async function SharePage({ params }: { params: Promise<{ token: string }> }) {
  const { token } = await params;
  const name = await siteName();
  const row = await fetchShare(token);

  if (!row) {
    return (
      <div className="mx-auto max-w-sm pt-20 text-center">
        <CircleHelp className="mx-auto h-10 w-10 text-slate-500" />
        <h1 className="mt-4 text-xl font-semibold text-white">Link not found</h1>
        <p className="mt-2 text-sm text-slate-400">
          This share link was revoked or never existed.
        </p>
        <Link href="/" className="mt-6 inline-flex items-center gap-1 text-teal-300 hover:underline">
          <ArrowLeft className="h-3.5 w-3.5" />
          {name}
        </Link>
      </div>
    );
  }

  const expired = row.expiresAt !== null && row.expiresAt <= now();
  if (expired) {
    return (
      <div className="mx-auto max-w-sm pt-20 text-center">
        <Hourglass className="mx-auto h-10 w-10 text-slate-500" />
        <h1 className="mt-4 text-xl font-semibold text-white">Link expired</h1>
        <p className="mt-2 text-sm text-slate-400">
          This share link has expired. Ask the owner for a fresh one.
        </p>
        <Link href="/" className="mt-6 inline-flex items-center gap-1 text-teal-300 hover:underline">
          <ArrowLeft className="h-3.5 w-3.5" />
          {name}
        </Link>
      </div>
    );
  }

  const liked = await hasLiked("share", token);

  return (
    // Fullscreen takeover — the share reads like an IG post, not an app page.
    <div className="fixed inset-0 z-50 overflow-y-auto bg-black text-white">
      <div className="mx-auto min-h-full w-full max-w-135 sm:border-x sm:border-white/10">
        <div className="sticky top-0 z-10 flex h-12 items-center justify-between border-b border-white/10 bg-black/80 px-2 backdrop-blur">
          <Link
            href="/"
            aria-label="Back"
            className="grid h-9 w-9 place-items-center rounded-full transition hover:bg-white/10"
          >
            <ArrowLeft className="h-5 w-5" />
          </Link>
          <span className="flex items-center gap-1.5 text-sm font-semibold">
            <LogoMark />
            {name}
          </span>
          <span className="w-9" />
        </div>

        <ShareViewer token={token} kind={row.kind} availability={row.availability} />

        <ShareActions
          likeEndpoint={`/api/public/share/${token}/like`}
          initialLikes={row.likes}
          initialLiked={liked}
          downloadHref={
            row.availability === "available" ? `/api/media/by-token/${token}?download=1` : null
          }
        />

        <div className="px-3 pb-[calc(2rem+env(safe-area-inset-bottom))] pt-1">
          <p className="text-sm leading-relaxed">
            <span className="font-semibold">{row.ownerName || "someone"}</span>
            {row.caption ? ` ${row.caption}` : ""}
          </p>
          <p className="mt-2 text-xs text-slate-500">
            {timeAgo(row.createdAt)} · Shared via{" "}
            <Link href="/" className="hover:text-slate-300 hover:underline">
              {name}
            </Link>
            {row.expiresAt
              ? ` · expires ${new Date(row.expiresAt * 1000).toLocaleDateString(undefined, { month: "short", day: "numeric" })}`
              : ""}
          </p>
        </div>
      </div>
    </div>
  );
}
