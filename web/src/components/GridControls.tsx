"use client";

import { useEffect, useState } from "react";

/** Card density — persisted so the choice survives navigation. */
export type GridSize = "sm" | "md" | "lg";

export const GRID_CLASSES: Record<GridSize, string> = {
  sm: "grid-cols-3 gap-2 sm:grid-cols-4 sm:gap-3 lg:grid-cols-6",
  md: "grid-cols-2 gap-3 sm:grid-cols-3 sm:gap-4 lg:grid-cols-4",
  lg: "grid-cols-1 gap-4 sm:grid-cols-2 lg:grid-cols-3",
};

export function useGridSize() {
  const [size, setSize] = useState<GridSize>("md");
  useEffect(() => {
    const s = localStorage.getItem("os-grid-size");
    if (s === "sm" || s === "md" || s === "lg") setSize(s);
  }, []);
  const pickSize = (s: GridSize) => {
    setSize(s);
    try {
      localStorage.setItem("os-grid-size", s);
    } catch {
      /* private mode — keep the session choice */
    }
  };
  return { size, pickSize };
}

export function SizeToggle({ size, onPick }: { size: GridSize; onPick: (s: GridSize) => void }) {
  return (
    <div className="flex overflow-hidden rounded-lg border border-white/10" role="group" aria-label="Card size">
      {(["sm", "md", "lg"] as const).map((s) => (
        <button
          key={s}
          onClick={() => onPick(s)}
          title={{ sm: "Small cards", md: "Medium cards", lg: "Large cards" }[s]}
          className={`px-2.5 py-1.5 text-[11px] font-semibold transition ${
            size === s ? "bg-control-ink text-control" : ""
          }`}
        >
          {s === "sm" ? "S" : s === "md" ? "M" : "L"}
        </button>
      ))}
    </div>
  );
}
