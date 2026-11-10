"use client";

import Link from "next/link";
import { useRouter } from "next/navigation";
import { apiFetch } from "@/lib/apiFetch";
import { clearSigningKey } from "@/lib/session-keys";
import { useEffect, useRef, useState } from "react";
import { ChevronDown, LogOut, Moon, Settings, Sun, SunMoon, type LucideIcon } from "lucide-react";

type Theme = "auto" | "light" | "dark";
const THEME_KEY = "os-theme";
const THEMES: { id: Theme; icon: LucideIcon; label: string }[] = [
  { id: "auto", icon: SunMoon, label: "Auto" },
  { id: "light", icon: Sun, label: "Light" },
  { id: "dark", icon: Moon, label: "Dark" },
];

function initials(name: string) {
  return (
    name
      .split(/\s+/)
      .slice(0, 2)
      .map((p) => p[0]?.toUpperCase() ?? "")
      .join("") || "?"
  );
}

export function UserMenu({
  name,
  email,
  signedInAt,
}: {
  name: string;
  email: string | null;
  signedInAt: number | null;
}) {
  const router = useRouter();
  const [open, setOpen] = useState(false);
  const [busy, setBusy] = useState(false);
  const [theme, setTheme] = useState<Theme>("auto");
  const ref = useRef<HTMLDivElement>(null);

  useEffect(() => {
    const saved = localStorage.getItem(THEME_KEY);
    if (saved === "light" || saved === "dark" || saved === "auto") setTheme(saved);
  }, []);

  useEffect(() => {
    if (!open) return;
    const onDown = (e: MouseEvent) => {
      if (ref.current && !ref.current.contains(e.target as Node)) setOpen(false);
    };
    const onKey = (e: KeyboardEvent) => e.key === "Escape" && setOpen(false);
    document.addEventListener("mousedown", onDown);
    document.addEventListener("keydown", onKey);
    return () => {
      document.removeEventListener("mousedown", onDown);
      document.removeEventListener("keydown", onKey);
    };
  }, [open]);

  function chooseTheme(t: Theme) {
    setTheme(t);
    localStorage.setItem(THEME_KEY, t);
    document.documentElement.dataset.theme = t;
  }

  async function logout() {
    setBusy(true);
    try {
      await apiFetch("/api/auth/logout", { method: "POST" });
      clearSigningKey();
      router.push("/login");
      router.refresh();
    } finally {
      setBusy(false);
    }
  }

  return (
    <div ref={ref} className="relative">
      <button
        onClick={() => setOpen((o) => !o)}
        aria-expanded={open}
        aria-haspopup="menu"
        className="-mr-1 flex items-center gap-2 rounded-full border-0 bg-transparent p-1 transition hover:bg-white/5 sm:-mr-2 sm:pr-2"
      >
        <span className="grid h-8 w-8 place-items-center rounded-full bg-linear-to-br from-teal-400 to-sky-500 text-xs font-bold text-[#0b0e14]">
          {initials(name)}
        </span>
        <span className="hidden max-w-28 truncate text-sm text-slate-300 sm:block">{name}</span>
        <ChevronDown
          className={`hidden h-3.5 w-3.5 text-slate-500 transition-transform sm:block ${open ? "rotate-180" : ""}`}
        />
      </button>

      {open && (
        <div
          role="menu"
          className="absolute right-0 top-12 z-50 w-64 overflow-hidden rounded-2xl border border-white/10 bg-menu shadow-2xl shadow-black/50"
        >
          <div className="border-b border-white/5 px-4 py-3">
            <p className="truncate text-sm font-medium text-white">{name}</p>
            {email && <p className="truncate text-xs text-slate-500">{email}</p>}
            {signedInAt != null && (
              <p className="mt-1 text-[11px] text-slate-600">
                Signed in {new Date(signedInAt * 1000).toLocaleString()}
              </p>
            )}
          </div>

          <div className="px-3 py-2.5">
            <p className="px-1 pb-1.5 text-[11px] font-medium uppercase tracking-wider text-slate-500">
              Theme
            </p>
            <div className="flex gap-1 rounded-xl bg-black/30 p-1">
              {THEMES.map((t) => (
                <button
                  key={t.id}
                  onClick={() => chooseTheme(t.id)}
                  className={`flex flex-1 items-center justify-center gap-1 rounded-lg border-0 bg-transparent px-2 py-1.5 text-xs transition ${
                    theme === t.id ? "bg-white/10 font-medium text-white" : "text-slate-400 hover:text-white"
                  }`}
                >
                  <t.icon className="h-3.5 w-3.5" />
                  {t.label}
                </button>
              ))}
            </div>
          </div>

          <div className="border-t border-white/5 py-1">
            <Link
              href="/profile"
              onClick={() => setOpen(false)}
              className="flex items-center gap-2 px-4 py-2 text-sm text-slate-300 transition hover:bg-white/5 hover:text-white"
            >
              <Settings className="h-4 w-4" />
              Profile &amp; password
            </Link>
            <button
              onClick={logout}
              disabled={busy}
              className="flex w-full items-center gap-2 border-0 bg-transparent px-4 py-2 text-left text-sm text-slate-300 transition hover:bg-white/5 hover:text-white disabled:opacity-50"
            >
              {busy ? (
                "Signing out…"
              ) : (
                <>
                  <LogOut className="h-4 w-4" />
                  Sign out
                </>
              )}
            </button>
          </div>
        </div>
      )}
    </div>
  );
}
