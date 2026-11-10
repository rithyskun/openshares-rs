import Link from "next/link";
import { ArrowRight, Lock } from "lucide-react";

export default function Unauthorized() {
  return (
    <div className="grid min-h-[calc(100dvh-14rem)] place-items-center">
      <div className="text-center">
        <Lock className="mx-auto h-12 w-12 text-slate-500" />
        <h1 className="mt-4 text-2xl font-bold text-white">Session expired</h1>
        <p className="mt-2 text-sm text-slate-400">
          You were signed in, but the session is no longer valid — sign in again to continue.
        </p>
        <Link
          href="/login"
          className="mt-6 inline-flex items-center gap-1.5 rounded-xl bg-control px-5 py-2.5 text-sm font-medium text-control-ink transition hover:brightness-95"
        >
          Sign in
          <ArrowRight className="h-4 w-4" />
        </Link>
      </div>
    </div>
  );
}
