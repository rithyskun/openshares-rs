"use client";

/**
 * Generic confirm modal — bottom sheet on phones (thumb reach), centered dialog
 * on larger screens. Used for batch delete/download and other destructive acts.
 */
export function ConfirmDialog({
  title,
  children,
  confirmLabel = "Confirm",
  danger = false,
  busy = false,
  onConfirm,
  onCancel,
}: {
  title: string;
  children?: React.ReactNode;
  confirmLabel?: string;
  danger?: boolean;
  busy?: boolean;
  onConfirm: () => void;
  onCancel: () => void;
}) {
  return (
    <div
      className="fixed inset-0 z-50 flex items-end justify-center p-4 sm:items-center"
      role="dialog"
      aria-modal="true"
      aria-label={title}
    >
      <button
        aria-label="Cancel"
        onClick={onCancel}
        disabled={busy}
        className="absolute inset-0 border-0 bg-black/60 backdrop-blur-sm"
      />
      <div className="relative w-full max-w-sm rounded-2xl border border-white/10 bg-menu p-5 pb-[calc(1.25rem+env(safe-area-inset-bottom))] shadow-2xl">
        <h3 className="text-base font-semibold text-white">{title}</h3>
        {children && <div className="mt-2 text-sm text-slate-400">{children}</div>}
        <div className="mt-5 flex gap-2">
          <button
            disabled={busy}
            onClick={onCancel}
            className="flex-1 rounded-xl px-4 py-2.5 text-sm transition disabled:opacity-50"
          >
            Cancel
          </button>
          <button
            disabled={busy}
            onClick={onConfirm}
            className={`flex-1 rounded-xl px-4 py-2.5 text-sm font-semibold transition disabled:opacity-50 ${
              danger ? "bg-rose-500 text-white/90 hover:bg-rose-400" : ""
            }`}
          >
            {busy ? "Working…" : confirmLabel}
          </button>
        </div>
      </div>
    </div>
  );
}
