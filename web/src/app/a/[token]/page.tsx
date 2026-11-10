import Link from "next/link";
import type { Metadata } from "next";
import { cache } from "react";
import { apiServerFetch } from "@/lib/api-server";
import { appUrl, siteName } from "@/lib/meta";
import { ArrowLeft, Hourglass } from "lucide-react";
import { PublicAlbumGrid, type PublicItem } from "@/components/PublicAlbumGrid";
import { LikeButton } from "@/components/LikeButton";
import { hasLiked } from "@/lib/likes";

export const dynamic = "force-dynamic";

/**
 * `GET /api/public/album?token=<t>` — extended response for the page.
 * The base route already returns `{items, nextCursor}` for grid
 * pagination; the page needs the album head too (the old ALBUM_SELECT
 * join — `PublicAlbumHead` on the Rust side — camelized at the edge):
 *
 *   album: { title, ownerName, likes, publicExpiresAt, itemCount, coverSlug }
 *
 * `itemCount` is the full album size (`media::count_in_album`), not the
 * page size; `coverSlug` the first live item's slug (`og:image`). The
 * endpoint keeps the old page's side effects: expired shares 404 with a
 * lazy `is_public` revoke, live views increment `albums.views`.
 */
export type PublicAlbumHead = {
  title: string;
  ownerName: string | null;
  likes: number;
  publicExpiresAt: number | null;
  itemCount: number;
  coverSlug: string | null;
};

type PublicAlbumPage_ = {
  album: PublicAlbumHead;
  items: PublicItem[];
  nextCursor: string | null;
};

const PAGE = 60;

/** Album head + first page — null on missing/expired/turned-off links.
 *  cache() dedupes the generateMetadata + page fetch within one render. */
const fetchPublicAlbum = cache(async function fetchPublicAlbum(
  token: string
): Promise<PublicAlbumPage_ | null> {
  const res = await apiServerFetch(
    `/api/public/album?token=${encodeURIComponent(token)}&limit=${PAGE}`
  );
  if (!res.ok) return null;
  const body = (await res.json()) as Partial<PublicAlbumPage_>;
  if (!body.album) return null;
  return body as PublicAlbumPage_;
});

export async function generateMetadata({ params }: { params: Promise<{ token: string }> }): Promise<Metadata> {
  const { token } = await params;
  const [data, name, origin] = await Promise.all([fetchPublicAlbum(token), siteName(), appUrl()]);
  const album = data?.album ?? null;
  if (!album) return { title: name };
  const title = album.title || "Shared album";
  const description = `Photo album shared by ${album.ownerName || "someone"} via ${name}`;
  const image = album.coverSlug
    ? `${origin}/api/media/${album.coverSlug}?token=${token}&thumb=1`
    : undefined;
  return {
    title,
    description,
    openGraph: {
      title,
      description,
      images: image ? [image] : undefined,
      type: "website",
      siteName: name,
    },
    twitter: { card: image ? "summary_large_image" : "summary", title, images: image ? [image] : undefined },
  };
}

export default async function PublicAlbumPage({ params }: { params: Promise<{ token: string }> }) {
  const { token } = await params;
  const name = await siteName();
  const data = await fetchPublicAlbum(token);

  if (!data) {
    return (
      <div className="mx-auto max-w-sm pt-20 text-center">
        <Hourglass className="mx-auto h-10 w-10 text-slate-500" />
        <h1 className="mt-4 text-xl font-semibold text-white">Link expired</h1>
        <p className="mt-2 text-sm text-slate-400">
          This album link has expired or sharing was turned off. Ask the owner for a fresh link.
        </p>
        <Link href="/" className="mt-6 inline-flex items-center gap-1 text-teal-300 hover:underline">
          <ArrowLeft className="h-3.5 w-3.5" />
          {name}
        </Link>
      </div>
    );
  }

  const { album, items, nextCursor } = data;
  const liked = await hasLiked("album", token);

  return (
    <div className="mx-auto max-w-3xl">
      <div className="mb-4 space-y-3">
        <div className="flex items-start justify-between gap-3">
          <div className="min-w-0">
            <h1 className="truncate text-xl font-semibold text-white">{album.title}</h1>
            <p className="mt-1 text-xs text-slate-400">
              {album.itemCount} item{album.itemCount === 1 ? "" : "s"} · shared by{" "}
              <span className="text-slate-200">{album.ownerName || "someone"}</span>
            </p>
            <div className="mt-2.5">
              <LikeButton
                endpoint={`/api/public/album/like?token=${token}`}
                initialLikes={album.likes}
                initialLiked={liked}
              />
            </div>
          </div>
          <Link href="/" className="shrink-0 pt-1 text-xs text-slate-500 hover:text-slate-300">
            {name}
          </Link>
        </div>

      </div>

      <PublicAlbumGrid items={items} token={token} initialCursor={nextCursor} itemCount={album.itemCount} />

      <p className="mt-4 text-center text-xs text-slate-500">
        Shared via {name} · link expires{" "}
        {album.publicExpiresAt
          ? new Date(album.publicExpiresAt * 1000).toLocaleDateString(undefined, {
              month: "short",
              day: "numeric",
            })
          : "soon"}
      </p>
    </div>
  );
}
