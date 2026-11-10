import Link from "next/link";
import { ArrowLeft, Images } from "lucide-react";
import { apiServerJson, canPerm, requireApiPerm } from "@/lib/api-server";
import { botUsername } from "@/lib/meta";
import { type Friend, type MediaItem } from "@/components/MediaCard";
import { MediaGrid } from "@/components/MediaGrid";
import { AlbumActions, type AlbumModel } from "@/components/AlbumCard";
import { UploadZone } from "@/components/UploadZone";

export const dynamic = "force-dynamic";

const PAGE = 60;

/** `GET /api/friends` row — superset of MediaCard's Friend. */
type ApiFriend = Friend & {
  first_name: string | null;
  last_name: string | null;
  shared: number;
};

/**
 * `GET /api/albums/{slug}` — the album head (`album_json`: the row plus
 * `itemCount`, `coverSlug`, views/likes, `botUsername`). 404 for missing
 * AND non-owned albums (non-enumeration, like the old page); an expired
 * `is_public` flag is lazy-revoked server-side before the row is returned.
 */
type ApiAlbum = AlbumModel;

export default async function AlbumPage({ params }: { params: Promise<{ slug: string }> }) {
  const { slug } = await params;
  const user = await requireApiPerm("media:view");

  const albumRow = await apiServerJson<ApiAlbum>(`/api/albums/${encodeURIComponent(slug)}`, {
    allow404: true,
  });

  const perms = new Set(user.permissions ?? []);
  const canEdit = canPerm(perms, user.id, "album:edit", albumRow.ownerId);
  const canUpload = canPerm(perms, user.id, "media:upload");
  const canDelete = canPerm(perms, user.id, "media:delete", albumRow.ownerId);

  // First page only — MediaGrid pages the rest via /api/media (scope=album:slug).
  const [{ items, nextCursor: mediaCursor }, { friends }, bot] = await Promise.all([
    apiServerJson<{ items: MediaItem[]; nextCursor: string | null }>(
      `/api/media?scope=${encodeURIComponent("album:" + albumRow.slug)}&limit=${PAGE}`
    ),
    apiServerJson<{ friends: ApiFriend[] }>("/api/friends"),
    botUsername(),
  ]);

  // API row carries botUsername already; /api/meta only fills older APIs.
  const album: AlbumModel = { ...albumRow, botUsername: albumRow.botUsername ?? bot };
  const itemCount = album.itemCount;

  return (
    <div className="space-y-6">
      <div className="space-y-2">
        <Link href="/gallery" className="inline-flex items-center gap-1 text-xs text-slate-400 hover:text-slate-200">
          <ArrowLeft className="h-3.5 w-3.5" />
          Your vault
        </Link>
        <h1 className="text-2xl font-bold text-white">{album.title}</h1>
        <div className="flex flex-wrap items-center gap-3">
          <span className="text-sm text-slate-400">
            {itemCount} item{itemCount === 1 ? "" : "s"}
            {album.views ? ` · ${album.views} view${album.views === 1 ? "" : "s"}` : ""}
            {album.likes ? ` · ${album.likes} like${album.likes === 1 ? "" : "s"}` : ""}
            {album.isPublic &&
              ` · public until ${album.publicExpiresAt ? new Date(album.publicExpiresAt * 1000).toLocaleDateString(undefined, { month: "short", day: "numeric" }) : "—"} at /a/${album.token}`}
          </span>
        </div>
        {canEdit && <AlbumActions album={album} />}
      </div>

      {canEdit && canUpload && <UploadZone targetSlug={album.slug} />}

      {itemCount === 0 ? (
        <div className="rounded-3xl border border-white/5 bg-white/2 px-6 py-14 text-center">
          <Images className="mx-auto h-10 w-10 text-slate-500" />
          <h2 className="mt-3 font-semibold text-white">Empty album</h2>
          <p className="mt-1 text-sm text-slate-400">Choose photos or videos above to fill it.</p>
        </div>
      ) : (
        <MediaGrid
          scope={`album:${album.slug}`}
          initialItems={items}
          initialCursor={mediaCursor}
          friends={friends}
          canDelete={canDelete}
          inAlbum
        />
      )}
    </div>
  );
}
