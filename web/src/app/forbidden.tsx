import Link from "next/link";
import { ArrowLeft, Ban } from "lucide-react";

export default function Forbidden() {
  return (
    <div className="grid min-h-[calc(100dvh-14rem)] place-items-center">
      <div className="text-center">
        <Ban className="mx-auto h-12 w-12 text-slate-500" />
        <h1 className="mt-4 text-2xl font-bold text-white">403 — no access</h1>
        <p className="mt-2 text-sm text-slate-400">
          You&apos;re signed in, but this area needs a permission your account doesn&apos;t have.
        </p>
        <Link
          href="/gallery"
          className="mt-6 inline-flex items-center gap-1.5 rounded-xl bg-control px-5 py-2.5 text-sm font-medium text-control-ink transition hover:brightness-95"
        >
          <ArrowLeft className="h-4 w-4" />
          Back to your vault
        </Link>
      </div>
    </div>
  );
}
