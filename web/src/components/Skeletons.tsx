/** Shared loading UI for App Router loading.tsx boundaries (server-safe). */

export function Spinner({ label = "Loading…" }: { label?: string }) {
  return (
    <div className="flex flex-col items-center justify-center gap-3 py-24 text-slate-400">
      <span className="h-8 w-8 animate-spin rounded-full border-2 border-white/10 border-t-teal-400" />
      <span className="text-sm">{label}</span>
    </div>
  );
}

function Tile() {
  return (
    <div className="animate-pulse overflow-hidden rounded-2xl border border-white/5 bg-white/3">
      <div className="aspect-square w-full bg-white/5" />
      <div className="space-y-2 p-3">
        <div className="h-2.5 w-2/3 rounded bg-white/5" />
        <div className="h-2.5 w-1/3 rounded bg-white/5" />
      </div>
    </div>
  );
}

/** Gallery/album lookalike: title row + tile grid shimmer. */
export function VaultSkeleton() {
  return (
    <div className="space-y-6">
      <div className="animate-pulse space-y-2">
        <div className="h-7 w-40 rounded-lg bg-white/5" />
        <div className="h-4 w-56 rounded bg-white/5" />
      </div>
      <div className="grid grid-cols-2 gap-3 sm:grid-cols-3 sm:gap-4 lg:grid-cols-4">
        {Array.from({ length: 8 }, (_, i) => (
          <Tile key={i} />
        ))}
      </div>
    </div>
  );
}
