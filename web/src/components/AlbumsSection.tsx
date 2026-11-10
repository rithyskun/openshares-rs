"use client";

import { useState } from "react";
import { useRouter } from "next/navigation";
import { FolderOutput, SquareCheck, Trash2 } from "lucide-react";
import { AlbumCard, type AlbumModel } from "./AlbumCard";
import { ConfirmDialog } from "./ConfirmDialog";
import { GRID_CLASSES, SizeToggle, useGridSize } from "./GridControls";
import { apiFetch } from "@/lib/apiFetch";
import { pool } from "@/lib/pool";
import { usePermissions } from "./PermissionsProvider";

/**
 * Gallery's albums section — search by title + the shared S/M/L card density.
 * Select mode turns card taps into selection toggles; the sticky action bar
 * runs batch stop-sharing/delete through the per-album PATCH/DELETE routes so
 * RBAC stays server-side (same pattern as the media grid's batch actions).
 */
export function AlbumsSection({ albums }: { albums: AlbumModel[] }) {
  const router = useRouter();
  const [query, setQuery] = useState("");
  const [selecting, setSelecting] = useState(false);
  const [selected, setSelected] = useState<Set<string>>(new Set());
  const [removed, setRemoved] = useState<Set<string>>(new Set());
  const [confirm, setConfirm] = useState<"delete" | null>(null);
  const [busy, setBusy] = useState(false);
  const [notice, setNotice] = useState<string | null>(null);
  const { size, pickSize } = useGridSize();
  const { canAny } = usePermissions();
  // Select mode only matters when there's a batch action the user may run.
  const canBatch = canAny("album:edit") || canAny("album:delete");
  const q = query.trim().toLowerCase();
  const visible = albums.filter((a) => !removed.has(a.slug) && (!q || a.title.toLowerCase().includes(q)));
  const chosen = visible.filter((a) => selected.has(a.slug));
  const publicCount = chosen.filter((a) => a.isPublic).length;
  const allSelected = visible.length > 0 && selected.size === visible.length;

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

  async function doStopSharing() {
    setBusy(true);
    const targets = chosen.filter((a) => a.isPublic);
    const st = { ok: 0, fail: 0 };
    await pool(targets, 4, async (a) => {
      try {
        const res = await apiFetch(`/api/albums/${a.slug}`, {
          method: "PATCH",
          headers: { "content-type": "application/json" },
          body: JSON.stringify({ isPublic: false }),
        });
        if (res.ok) st.ok++;
        else st.fail++;
      } catch {
        st.fail++;
      }
    });
    const { ok, fail } = st;
    setBusy(false);
    exitSelect();
    setNotice(
      fail
        ? `Stopped sharing ${ok} — ${fail} couldn't be updated (permission or error).`
        : `Stopped sharing ${ok} album${ok === 1 ? "" : "s"}.`
    );
    router.refresh();
  }

  async function doDelete() {
    setBusy(true);
    const st = { ok: 0, fail: 0 };
    await pool(chosen, 4, async (a) => {
      try {
        const res = await apiFetch(`/api/albums/${a.slug}`, { method: "DELETE" });
        if (res.ok) {
          st.ok++;
          setRemoved((prev) => new Set(prev).add(a.slug));
        } else {
          st.fail++;
        }
      } catch {
        st.fail++;
      }
    });
    const { ok, fail } = st;
    setBusy(false);
    setConfirm(null);
    exitSelect();
    setNotice(
      fail
        ? `Deleted ${ok} — ${fail} couldn't be removed (permission or error).`
        : `Deleted ${ok} album${ok === 1 ? "" : "s"} — their items stay in your vault.`
    );
    router.refresh();
  }

  return (
    <section className="space-y-5">
      <div className="flex flex-wrap items-center gap-2">
        <h2 className="text-sm font-semibold uppercase tracking-wide text-slate-400">Albums</h2>
        {canBatch && (
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
        )}
        <input
          value={query}
          onChange={(e) => setQuery(e.target.value)}
          placeholder="Search albums…"
          aria-label="Search albums by title"
          className="min-w-0 flex-1 rounded-lg px-3 py-1.5 text-xs transition focus:border-control-ink focus:outline-none"
        />
        <SizeToggle size={size} onPick={pickSize} />
      </div>

      {selecting && selected.size > 0 && (
        <div className="sticky top-14 z-30 mx-auto flex max-w-md items-center gap-2 rounded-2xl border border-white/10 bg-menu/95 p-2 shadow-2xl backdrop-blur">
          <span className="shrink-0 px-2 text-xs text-slate-400">{selected.size} selected</span>
          <button
            onClick={() => setSelected(allSelected ? new Set() : new Set(visible.map((a) => a.slug)))}
            className="shrink-0 rounded-lg px-2.5 py-1.5 text-xs"
          >
            {allSelected ? "Clear" : "All"}
          </button>
          {publicCount > 0 && canAny("album:edit") && (
            <button
              disabled={busy}
              onClick={() => void doStopSharing()}
              title={`Stop sharing ${publicCount} public album${publicCount === 1 ? "" : "s"}`}
              className="inline-flex flex-1 items-center justify-center gap-1.5 rounded-xl px-3 py-2.5 text-sm font-medium transition disabled:opacity-40"
            >
              <FolderOutput className="h-4 w-4" />
              Stop sharing
            </button>
          )}
          {canAny("album:delete") && (
          <button
            disabled={busy}
            onClick={() => setConfirm("delete")}
            className="inline-flex flex-1 items-center justify-center gap-1.5 rounded-xl bg-rose-500/90 px-3 py-2.5 text-sm font-semibold text-[#fff] transition hover:bg-rose-400 disabled:opacity-40"
          >
            <Trash2 className="h-4 w-4" />
            Delete
          </button>
          )}
        </div>
      )}

      {notice && <p className="text-xs text-teal-300">{notice}</p>}

      {visible.length === 0 ? (
        <p className="rounded-2xl border border-white/5 bg-white/2 px-4 py-8 text-center text-sm text-slate-400">
          No albums match &ldquo;{query.trim()}&rdquo;.
        </p>
      ) : (
        <div className={`grid ${GRID_CLASSES[size]}`}>
          {visible.map((a) => (
            <AlbumCard
              key={a.slug}
              album={a}
              droppable
              selectable={selecting}
              selected={selected.has(a.slug)}
              onToggleSelect={toggle}
            />
          ))}
        </div>
      )}

      {confirm === "delete" && (
        <ConfirmDialog
          title={`Delete ${selected.size} album${selected.size === 1 ? "" : "s"}?`}
          confirmLabel="Delete albums"
          danger
          busy={busy}
          onConfirm={() => void doDelete()}
          onCancel={() => setConfirm(null)}
        >
          The albums and their public/Telegram links stop working immediately — the
          photos and videos inside stay in your vault and in Telegram. This cannot be
          undone.
        </ConfirmDialog>
      )}
    </section>
  );
}
