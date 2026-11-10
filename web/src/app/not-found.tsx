import Link from "next/link";
import { ArrowLeft, FileQuestion } from "lucide-react";
import { siteName } from "@/lib/meta";

export default async function NotFound() {
  const name = await siteName();
  return (
    <div className="grid min-h-[calc(100dvh-14rem)] place-items-center">
      <div className="text-center">
        <FileQuestion className="mx-auto h-12 w-12 text-slate-500" />
        <h1 className="mt-4 text-2xl font-bold text-white">404 — not found</h1>
        <p className="mt-2 text-sm text-slate-400">
          That page doesn&apos;t exist, or the link is stale.
        </p>
        <Link
          href="/"
          className="mt-6 inline-flex items-center gap-1.5 rounded-xl bg-control px-5 py-2.5 text-sm font-medium text-control-ink transition hover:brightness-95"
        >
          <ArrowLeft className="h-4 w-4" />
          Back to {name}
        </Link>
      </div>
    </div>
  );
}
