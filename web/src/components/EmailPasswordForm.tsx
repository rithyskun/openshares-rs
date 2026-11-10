"use client";

import { useState } from "react";
import { useRouter, useSearchParams } from "next/navigation";
import { apiFetch, apiJson } from "@/lib/apiFetch";
import { encryptPayload } from "@/lib/envelope";
import { setSigningKey } from "@/lib/session-keys";

export function EmailPasswordForm() {
  const router = useRouter();
  const params = useSearchParams();
  const [busy, setBusy] = useState(false);
  const [err, setErr] = useState<string | null>(null);

  async function submit(e: React.FormEvent<HTMLFormElement>) {
    e.preventDefault();
    if (busy) return;
    setBusy(true);
    setErr(null);
    const form = new FormData(e.currentTarget);
    const credentials = {
      email: String(form.get("email") ?? ""),
      password: String(form.get("password") ?? ""),
    };

    // When the deployment publishes an RSA key, credentials cross the wire
    // inside the hybrid RSA+AES envelope — plaintext never leaves the page.
    let requestBody: string;
    try {
      const keyRes = await apiFetch("/api/auth/rsa-key");
      const key = (await keyRes.json()) as { enabled?: boolean; publicKey?: string; keyId?: string };
      requestBody = key.enabled && key.publicKey && key.keyId
        ? JSON.stringify({ payload: await encryptPayload(JSON.stringify({ ...credentials, ts: Date.now() }), key.publicKey, key.keyId) })
        : JSON.stringify(credentials);
    } catch {
      setBusy(false);
      setErr("Encryption isn't available — try again in a moment.");
      return;
    }

    const res = await apiFetch("/api/auth/login", {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: requestBody,
    });
    setBusy(false);
    if (res.ok) {
      try {
        const data = await apiJson<{ signingKey?: string | null }>(res);
        if (data.signingKey) setSigningKey(data.signingKey);
      } catch { /* signingKey is best-effort — /api/me re-issues it */ }
      // Return to the page the expired session bounced away from — same-origin
      // paths only, never an open redirect.
      const next = params.get("next");
      router.push(next?.startsWith("/") && !next.startsWith("//") ? next : "/gallery");
      router.refresh();
      return;
    }
    const body = (await res.json().catch(() => ({}))) as { error?: string };
    setErr(body.error ?? `Failed (${res.status})`);
  }

  return (
    <form
      onSubmit={submit}
      aria-busy={busy}
      className="space-y-3 rounded-2xl border border-white/10 bg-white/3 p-5"
    >
      <h2 className="font-semibold text-white">Email &amp; password</h2>
      <Field
        label="Email"
        name="email"
        type="email"
        placeholder="you@example.com"
        autoComplete="email"
        disabled={busy}
      />
      <Field
        label="Password"
        name="password"
        type="password"
        placeholder="••••••••"
        autoComplete="current-password"
        disabled={busy}
      />
      {err && <p className="text-sm text-rose-400">{err}</p>}
      <button
        disabled={busy}
        className="flex w-full items-center justify-center gap-2 rounded-xl px-4 py-2.5 font-semibold transition disabled:cursor-not-allowed disabled:opacity-60"
      >
        {busy && (
          <span className="h-4 w-4 animate-spin rounded-full border-2 border-[#0b0e14]/30 border-t-[#0b0e14]" />
        )}
        {busy ? "Signing in…" : "Log in"}
      </button>
      <p className="pt-1 text-center text-xs text-slate-500">
        Private vault — access by invitation only.
      </p>
    </form>
  );
}

function Field(props: React.InputHTMLAttributes<HTMLInputElement> & { label: string }) {
  const { label, ...rest } = props;
  return (
    <label className="block">
      <span className="mb-1 block text-sm text-slate-400">{label}</span>
      <input
        {...rest}
        required
        className="w-full rounded-xl px-3 py-2.5 text-sm outline-none transition focus:border-control-ink focus:ring-2 focus:ring-control-ink/20 disabled:opacity-50"
      />
    </label>
  );
}
