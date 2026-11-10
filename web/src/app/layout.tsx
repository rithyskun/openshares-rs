import type { Metadata, Viewport } from "next";
import Link from "next/link";
import "./globals.css";
import { UserMenu } from "@/components/UserMenu";
import { maybeApiUser } from "@/lib/api-server";
import { siteName } from "@/lib/meta";
import { PermissionsProvider } from "@/components/PermissionsProvider";
import { Images, ShieldCheck, Users } from "lucide-react";
import { LogoMark } from "@/components/LogoMark";

// Runs before paint so the saved theme applies with no flash.
const THEME_INIT = `try{document.documentElement.dataset.theme=localStorage.getItem("os-theme")||"auto"}catch(e){document.documentElement.dataset.theme="auto"}`;

export async function generateMetadata(): Promise<Metadata> {
  const name = await siteName();
  return {
    title: `${name} — Telegram-backed photo & video library`,
    description:
      `${name} is a personal media vault. Collect your photos and clips in one private gallery, organize them into albums, and share them with friends.`,
  };
}

export const viewport: Viewport = {
  width: "device-width",
  initialScale: 1,
  viewportFit: "cover", // lets the bottom tab bar draw into the iOS safe area
  themeColor: "#0b0e14",
};

export default async function RootLayout({ children }: { children: React.ReactNode }) {
  const user = await maybeApiUser();
  const name = await siteName();
  // /api/me carries the effective permission keys — one read per render.
  const permissions = new Set(user?.permissions ?? []);
  const showAdmin = permissions.has("*") || permissions.has("admin:access");
  return (
    <html lang="en" data-theme="auto" suppressHydrationWarning>
      <head>
        <script dangerouslySetInnerHTML={{ __html: THEME_INIT }} />
      </head>
      <body className="flex h-dvh flex-col overflow-hidden text-slate-200 antialiased">
        <header className="sticky top-0 z-40 border-b border-white/5 bg-paper/80 backdrop-blur">
          <div className="shell flex h-14 items-center justify-between">
            <Link href="/" className="flex items-center gap-2 font-semibold tracking-tight">
              <LogoMark />
              {name}
            </Link>
            <nav className="flex items-center gap-1 text-sm">
              {user ? (
                <>
                  <div className="hidden items-center gap-1 sm:flex">
                    <Link
                      href="/gallery"
                      className="rounded-lg px-3 py-1.5 text-slate-300 transition hover:bg-white/5 hover:text-white"
                    >
                      Gallery
                    </Link>
                    <Link
                      href="/friends"
                      className="rounded-lg px-3 py-1.5 text-slate-300 transition hover:bg-white/5 hover:text-white"
                    >
                      Friends
                    </Link>
                    {showAdmin && (
                      <Link
                        href="/admin"
                        className="rounded-lg px-3 py-1.5 text-slate-300 transition hover:bg-white/5 hover:text-white"
                      >
                        Admin
                      </Link>
                    )}
                  </div>
                  <UserMenu
                    name={user.name || "friend"}
                    email={user.email}
                    signedInAt={user.sessionCreatedAt ?? null}
                  />
                </>
              ) : (
                <Link
                  href="/login"
                  className="rounded-lg bg-control px-3 py-1.5 font-medium text-control-ink transition hover:brightness-95"
                >
                  Log in
                </Link>
              )}
            </nav>
          </div>
        </header>
        <main className="scroll-shell">
          <div className="shell pb-28 pt-6 sm:pb-20 sm:pt-8">
            <PermissionsProvider userId={user?.id ?? null} permissions={[...permissions]}>
              {children}
            </PermissionsProvider>
          </div>
        </main>
        {user && (
          // Mobile tab bar — primary nav within thumb reach; desktop uses the header links.
          <nav className="fixed inset-x-0 bottom-0 z-40 border-t border-white/5 bg-paper/95 pb-[env(safe-area-inset-bottom)] backdrop-blur sm:hidden">
            <div className="flex h-14 items-stretch">
              <Link
                href="/gallery"
                className="flex flex-1 flex-col items-center justify-center gap-0.5 text-[11px] text-slate-300 transition hover:text-white"
              >
                <Images className="h-5 w-5" />
                Gallery
              </Link>
              <Link
                href="/friends"
                className="flex flex-1 flex-col items-center justify-center gap-0.5 text-[11px] text-slate-300 transition hover:text-white"
              >
                <Users className="h-5 w-5" />
                Friends
              </Link>
              {showAdmin && (
                <Link
                  href="/admin"
                  className="flex flex-1 flex-col items-center justify-center gap-0.5 text-[11px] text-slate-300 transition hover:text-white"
                >
                  <ShieldCheck className="h-5 w-5" />
                  Admin
                </Link>
              )}
            </div>
          </nav>
        )}
      </body>
    </html>
  );
}
