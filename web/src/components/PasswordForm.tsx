"use client";

import { useState } from "react";
import { Check } from "lucide-react";
import { apiFetch } from "@/lib/apiFetch";
import { encryptPayload } from "@/lib/envelope";

export function PasswordForm() {
  const [current, setCurrent] = useState("");
  const [next, setNext] = useState("");
  const [confirm, setConfirm] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState("");
  const [done, setDone] = useState(false);

  async function submit(e: React.FormEvent) {
    e.preventDefault();
    if (busy) return;
    setError("");
    if (next !== confirm) {
      setError("New passwords don't match.");
      return;
    }
    setBusy(true);
    try {
      // Same hybrid envelope as login when the deployment publishes an RSA key.
      let body = JSON.stringify({ current, next });
      try {
        const keyRes = await apiFetch("/api/auth/rsa-key");
        const key = (await keyRes.json()) as { enabled?: boolean; publicKey?: string; keyId?: string };
        if (key.enabled && key.publicKey && key.keyId) {
          body = JSON.stringify({
            payload: await encryptPayload(JSON.stringify({ current, next, ts: Date.now() }), key.publicKey, key.keyId),
          });
        }
      } catch {
        setError("Encryption isn't available — try again in a moment.");
        return;
      }
      const res = await apiFetch("/api/profile/password", {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body,
      });
      const data = (await res.json().catch(() => ({}))) as { error?: string };
      if (!res.ok) {
        setError(data.error || "Couldn't change password.");
        return;
      }
      setDone(true);
      setCurrent("");
      setNext("");
      setConfirm("");
    } finally {
      setBusy(false);
    }
  }

  if (done) {
    return (
      <div className="mt-4 flex items-start gap-2 rounded-xl border border-teal-500/25 bg-teal-500/10 px-4 py-3 text-sm text-teal-200">
        <Check className="mt-0.5 h-4 w-4 shrink-0" />
        <span>
          Password changed — other devices were signed out.{" "}
          <button
            onClick={() => setDone(false)}
            className="border-0 bg-transparent p-0 text-inherit underline underline-offset-2"
          >
            Change again
          </button>
        </span>
      </div>
    );
  }

  return (
    <form onSubmit={submit} className="mt-4 space-y-3">
      <input
        type="password"
        required
        autoComplete="current-password"
        value={current}
        onChange={(e) => setCurrent(e.target.value)}
        placeholder="Current password"
        className="w-full rounded-xl px-4 py-2.5 text-sm outline-none transition focus:border-control-ink"
      />
      <input
        type="password"
        required
        minLength={8}
        autoComplete="new-password"
        value={next}
        onChange={(e) => setNext(e.target.value)}
        placeholder="New password (8+ characters)"
        className="w-full rounded-xl px-4 py-2.5 text-sm outline-none transition focus:border-control-ink"
      />
      <input
        type="password"
        required
        autoComplete="new-password"
        value={confirm}
        onChange={(e) => setConfirm(e.target.value)}
        placeholder="Repeat new password"
        className="w-full rounded-xl px-4 py-2.5 text-sm outline-none transition focus:border-control-ink"
      />
      {error && <p className="text-sm text-rose-300">{error}</p>}
      <button
        type="submit"
        disabled={busy}
        className="w-full rounded-xl py-2.5 text-sm font-medium transition disabled:opacity-60"
      >
        {busy ? "Updating…" : "Update password"}
      </button>
    </form>
  );
}
