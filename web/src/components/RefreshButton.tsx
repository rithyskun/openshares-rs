"use client";

import { useTransition } from "react";
import { useRouter } from "next/navigation";
import { RotateCw } from "lucide-react";

export function RefreshButton() {
  const router = useRouter();
  const [pending, startTransition] = useTransition();

  return (
    <button
      type="button"
      onClick={() => startTransition(() => router.refresh())}
      disabled={pending}
      title="Refresh"
      aria-label="Refresh"
      className="grid h-9 w-9 place-items-center rounded-xl transition disabled:opacity-50"
    >
      <RotateCw className={`h-4 w-4 ${pending ? "animate-spin" : ""}`} />
    </button>
  );
}
