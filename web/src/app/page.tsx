import Link from "next/link";
import { Handshake, Images, Vault } from "lucide-react";
import { LogoMark } from "@/components/LogoMark";
import { maybeApiUser } from "@/lib/api-server";
import { siteName } from "@/lib/meta";

export const dynamic = "force-dynamic";

export default async function LandingPage() {
  const user = await maybeApiUser();
  const name = await siteName();

  return (
    <div className="space-y-16">
      <section className="pt-8">
        <LogoMark className="mb-5 h-12 w-12" />
        <p className="mb-3 inline-flex items-center gap-2 rounded-full border border-teal-500/20 bg-teal-500/10 px-3 py-1 text-xs text-teal-300">
          <span className="h-1.5 w-1.5 rounded-full bg-teal-400" />
          Private media vault · share on your terms
        </p>
        <h1 className="max-w-3xl text-4xl font-bold leading-tight tracking-tight text-white sm:text-5xl">
          Keep every photo & video in your own vault.
          <span className="block bg-linear-to-r from-teal-300 to-sky-400 bg-clip-text text-transparent">
            Share them with friends from your dashboard.
          </span>
        </h1>
        <p className="mt-4 max-w-2xl text-slate-400">
          {name} is a personal media vault. Collect your photos and clips in one private
          gallery, organize them into albums, and hand them to friends — no cloud bill, no
          clutter.
        </p>
        {user && (
          <div className="mt-6 flex flex-wrap items-center gap-3">
            <Link
              href="/gallery"
              className="rounded-xl bg-control px-5 py-2.5 font-semibold text-control-ink transition hover:brightness-95"
            >
              Open gallery
            </Link>
          </div>
        )}
      </section>

      <section className="grid gap-4 sm:grid-cols-3">
        {[
          {
            icon: Vault,
            title: "Your private vault",
            text: "Save photos, videos, and files once. They're indexed, searchable, and streamable whenever you need them.",
          },
          {
            icon: Images,
            title: "Clean web gallery",
            text: "Browse everything in one grid: captions, sizes, durations, and one-tap streaming or download.",
          },
          {
            icon: Handshake,
            title: "Share with friends",
            text: "Invite friends with a one-time link, or hand out an expiring public link for any item or album.",
          },
        ].map((f) => (
          <div
            key={f.title}
            className="rounded-2xl border border-white/5 bg-white/3 p-5 transition hover:border-white/10"
          >
            <f.icon className="h-6 w-6 text-teal-300" />
            <h3 className="mt-3 font-semibold text-white">{f.title}</h3>
            <p className="mt-1 text-sm text-slate-400">{f.text}</p>
          </div>
        ))}
      </section>
    </div>
  );
}
