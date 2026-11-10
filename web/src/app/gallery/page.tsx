import { apiServerFetch, apiServerJson, canPerm, requireApiPerm } from "@/lib/api-server";
import { botUsername } from "@/lib/meta";
import { type Friend, type MediaItem } from "@/components/MediaCard";
import { MediaGrid } from "@/components/MediaGrid";
import { ConnectTelegramCard } from "@/components/ConnectTelegramCard";
import { type AlbumModel } from "@/components/AlbumCard";
import { AlbumsSection } from "@/components/AlbumsSection";
import { UploadZone } from "@/components/UploadZone";
import { RefreshButton } from "@/components/RefreshButton";
import { Camera, Send } from "lucide-react";

export const dynamic = "force-dynamic";

const PAGE = 60;

/** `GET /api/friends` row — superset of MediaCard's Friend (name resolved server-side). */
type ApiFriend = Friend & {
  first_name: string | null;
  last_name: string | null;
  shared: number;
};

/**
 * `GET /api/albums` row — the gallery's AlbumModel wire shape
 * (`album_json` on the Rust side: itemCount + coverSlug + views/likes +
 * botUsername, camelized at the edge like `/api/media`'s items).
 * Expired `is_public` flags are lazy-revoked server-side, same as the
 * old page's inline UPDATE.
 */
type ApiAlbum = AlbumModel;

/**
 * `GET /api/media/summary` → `{count, bytes}` — vault-wide totals for the
 * header (the old page's `COUNT(*)`/`SUM(file_size)`). Until the endpoint
 * exists the header falls back to summing the first page — the rest of
 * the gallery still renders instead of 500ing on the missing route.
 */
async function mediaTotals(items: MediaItem[]): Promise<{ count: number; bytes: number }> {
  const fallback = {
    count: items.length,
    bytes: items.reduce((n, i) => n + (i.fileSize ?? 0), 0),
  };
  const res = await apiServerFetch("/api/media/summary").catch(() => null);
  if (!res?.ok) return fallback;
  return res.json() as Promise<{ count: number; bytes: number }>;
}

export default async function GalleryPage() {
  const user = await requireApiPerm("media:view");
  const perms = new Set(user.permissions ?? []);
  const canUpload = canPerm(perms, user.id, "media:upload");
  const canDelete = canPerm(perms, user.id, "media:delete", user.id);

  // First page only — MediaGrid pages the rest via /api/media (scope=vault).
  const [{ items, nextCursor: mediaCursor }, { friends }, { albums: albumRows }, bot] =
    await Promise.all([
      apiServerJson<{ items: MediaItem[]; nextCursor: string | null }>(
        `/api/media?scope=vault&limit=${PAGE}`
      ),
      apiServerJson<{ friends: ApiFriend[] }>("/api/friends"),
      apiServerJson<{ albums: ApiAlbum[] }>("/api/albums"),
      botUsername(),
    ]);
  const totals = await mediaTotals(items);

  // API rows carry botUsername already (album_json reads the settings);
  // /api/meta only fills in when a stale/older API omits it.
  const albums: AlbumModel[] = albumRows.map((a) => ({ ...a, botUsername: a.botUsername ?? bot }));

  return (
    <div className="space-y-6">
      <header className="flex items-start justify-between gap-4">
        <div>
          <h1 className="text-2xl font-bold text-white">Your vault</h1>
          <p className="mt-1 text-sm text-slate-400">
            {totals.count} item{totals.count === 1 ? "" : "s"} · {(totals.bytes / 1048576).toFixed(1)} MB
            stored in Telegram
          </p>
        </div>
        <RefreshButton />
      </header>

      {!user.telegram_id && <ConnectTelegramCard />}

      {canUpload && <UploadZone />}

      {albums.length > 0 && <AlbumsSection albums={albums} />}

      {totals.count === 0 ? (
        <div className="rounded-3xl border border-white/5 bg-white/2 px-6 py-14 text-center">
          <Camera className="mx-auto h-10 w-10 text-slate-500" />
          <h2 className="mt-4 text-lg font-semibold text-white">Nothing here yet</h2>
          <p className="mx-auto mt-2 max-w-md text-sm text-slate-400">
            {user.telegram_id
              ? "Send a photo or video to your bot in Telegram — it will appear here instantly."
              : "After you connect Telegram, send photos and videos to the bot to fill your library."}
          </p>
          {bot && (
            <a
              href={`https://t.me/${bot}`}
              target="_blank"
              rel="noreferrer"
              className="mt-5 inline-flex items-center gap-2 rounded-xl bg-[#2AABEE] px-5 py-2.5 text-sm font-semibold text-[#fff] transition hover:brightness-110"
            >
              <Send className="h-4 w-4" />
              Open @{bot} in Telegram
            </a>
          )}
        </div>
      ) : (
        <MediaGrid
          scope="vault"
          initialItems={items}
          initialCursor={mediaCursor}
          friends={friends}
          canDelete={canDelete}
          albums={albums}
        />
      )}
    </div>
  );
}
