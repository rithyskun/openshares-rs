"use client";

import { useEffect, useRef, useState } from "react";
import { ArrowRight, Send } from "lucide-react";
import { useRouter } from "next/navigation";
import { apiFetch } from "@/lib/apiFetch";

type LinkStart = { code: string; bot_username: string | null; bot_link: string | null };

export function ConnectTelegramCard() {
  const router = useRouter();
  const [data, setData] = useState<LinkStart | null>(null);
  const [err, setErr] = useState<string | null>(null);

  // Once a code is minted, poll until the bot links this account, then refresh the page.
  // Bounded at 10 minutes so a forgotten tab can't poll forever.
  const alive = useRef(false);
  useEffect(() => {
    if (!data) return;
    alive.current = true;
    const deadline = Date.now() + 10 * 60_000;
    const poll = async () => {
      try {
        const res = await apiFetch("/api/profile/link", { cache: "no-store" });
        const body = (await res.json()) as { telegram_id?: string | null };
        if (!alive.current) return;
        if (body.telegram_id) {
          router.refresh();
          return;
        }
      } catch {
        /* keep polling */
      }
      if (!alive.current) return;
      if (Date.now() > deadline) {
        setErr("Timed out waiting — check the bot, then generate a fresh code.");
        return;
      }
      setTimeout(poll, 3000);
    };
    const t = setTimeout(poll, 3000);
    return () => {
      alive.current = false;
      clearTimeout(t);
    };
  }, [data, router]);

  async function mint() {
    setErr(null);
    const res = await apiFetch("/api/profile/link", { method: "POST" });
    if (!res.ok) {
      const body = (await res.json().catch(() => ({}))) as { error?: string };
      setErr(body.error ?? "Could not generate a code.");
      return;
    }
    setData((await res.json()) as LinkStart);
  }

  return (
    <div className="rounded-2xl border border-sky-500/20 bg-sky-500/[0.06] p-5">
      <div className="flex items-start gap-3">
        <Send className="mt-0.5 h-6 w-6 shrink-0 text-sky-400" />
        <div className="min-w-0 flex-1">
          <h3 className="font-semibold text-white">Connect your Telegram</h3>
          {!data ? (
            <>
              <p className="mt-1 text-sm text-slate-400">
                Link a Telegram account so its photos and videos land in this vault.
              </p>
              <button
                onClick={mint}
                className="mt-3 rounded-xl bg-[#2AABEE] px-4 py-2 text-sm font-semibold text-[#fff] transition hover:brightness-110"
              >
                Generate /link code
              </button>
            </>
          ) : (
            <div className="mt-2 space-y-2 text-sm">
              <p className="text-slate-400">
                Send this command from your Telegram account
                {data.bot_username ? (
                  <>
                    {" to "}
                    <a
                      href={data.bot_link ?? `https://t.me/${data.bot_username}`}
                      target="_blank"
                      rel="noreferrer"
                      className="text-sky-400 hover:underline"
                    >
                      @{data.bot_username}
                    </a>
                  </>
                ) : (
                  " to your bot"
                )}
                :
              </p>
              <div className="flex items-center gap-2">
                <code className="select-all rounded-lg bg-black/40 px-3 py-2 font-mono text-teal-300">
                  /link {data.code}
                </code>
                {data.bot_link && (
                  <a
                    href={data.bot_link}
                    target="_blank"
                    rel="noreferrer"
                    className="inline-flex items-center gap-1 rounded-lg bg-[#2AABEE]/90 px-3 py-1.5 text-xs font-medium text-[#fff] hover:brightness-110"
                  >
                    Open bot
                    <ArrowRight className="h-3.5 w-3.5" />
                  </a>
                )}
              </div>
              <p className="flex items-center gap-2 text-xs text-slate-500">
                Waiting for confirmation…
                <span className="inline-block h-2 w-2 animate-pulse rounded-full bg-sky-400" />
              </p>
            </div>
          )}
          {err && <p className="mt-2 text-sm text-rose-400">{err}</p>}
        </div>
      </div>
    </div>
  );
}
