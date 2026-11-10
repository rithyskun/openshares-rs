"use client";

import { useRef, useState } from "react";
import { Check, ImagePlus } from "lucide-react";
import { useRouter } from "next/navigation";
import { pool } from "@/lib/pool";
import { apiFetch, csrfToken } from "@/lib/apiFetch";

type Phase = "idle" | "creating" | "uploading" | "done" | "error";

const fmtMb = (n: number) => (n < 1048576 ? `${(n / 1024).toFixed(0)} KB` : `${(n / 1048576).toFixed(1)} MB`);

/** POST one file via XHR — fetch() can't report request-body progress; xhr.upload can. */
function uploadFile(
  slug: string,
  file: File,
  onLoaded: (loaded: number) => void
): Promise<{ ok: boolean; error?: string }> {
  return new Promise((resolve) => {
    const xhr = new XMLHttpRequest();
    xhr.open("POST", `/api/albums/${slug}/files`);
    const csrf = csrfToken();
    if (csrf) xhr.setRequestHeader("x-csrf-token", csrf);
    xhr.upload.addEventListener("progress", (e) => {
      if (e.lengthComputable) onLoaded(e.loaded);
    });
    xhr.addEventListener("load", () => {
      if (xhr.status >= 200 && xhr.status < 300) {
        resolve({ ok: true });
        return;
      }
      let error = `failed (${xhr.status})`;
      try {
        error = (JSON.parse(xhr.responseText) as { error?: string }).error ?? error;
      } catch { /* non-JSON body */ }
      resolve({ ok: false, error });
    });
    xhr.addEventListener("error", () => resolve({ ok: false, error: "network error" }));
    const form = new FormData();
    form.append("file", file);
    xhr.send(form);
  });
}

export function UploadZone({ targetSlug }: { targetSlug?: string }) {
  const router = useRouter();
  const inputRef = useRef<HTMLInputElement>(null);
  const dragDepth = useRef(0);
  const [title, setTitle] = useState("");
  const [phase, setPhase] = useState<Phase>("idle");
  const [status, setStatus] = useState("");
  const [errors, setErrors] = useState<string[]>([]);
  const [today] = useState(() => new Date().toLocaleDateString());
  const [dragging, setDragging] = useState(false);
  const [progress, setProgress] = useState<{ file: string; index: number; total: number; loaded: number; bytes: number } | null>(null);

  const busy = phase === "creating" || phase === "uploading";
  const pct = progress && progress.bytes > 0 ? Math.min(100, Math.round((progress.loaded / progress.bytes) * 100)) : 0;

  async function begin(fileList: File[]) {
    const files = fileList.filter((f) => f.type.startsWith("image/") || f.type.startsWith("video/"));
    if (files.length === 0) return;
    const name = (title || new Date().toLocaleString()).trim().slice(0, 200) || "Untitled album";

    setErrors([]);
    setPhase("creating");
    setStatus("Creating album…");

    let slug: string | undefined = targetSlug;
    if (!slug) {
      const created = await apiFetch("/api/albums", {
        method: "POST",
        headers: { "content-type": "application/json" },
        body: JSON.stringify({ title: name }),
      });
      if (!created.ok) {
        const body = (await created.json().catch(() => ({}))) as { error?: string };
        setPhase("error");
        setStatus("");
        setErrors([body.error ?? "Could not create the album."]);
        return;
      }
      slug = ((await created.json()) as { slug: string }).slug;
    }

    setPhase("uploading");
    const fail: string[] = [];
    const totalBytes = files.reduce((s, f) => s + f.size, 0);
    const loaded = files.map(() => 0); // per-file bytes in flight
    const st = { done: 0 };
    const report = (label: string) =>
      setProgress({
        file: label,
        index: st.done,
        total: files.length,
        loaded: loaded.reduce((a, b) => a + b, 0),
        bytes: totalBytes,
      });
    // Pool of 3 parallel uploads — each is an independent Worker→Telegram
    // round-trip, so batches finish ~3x faster than one-at-a-time.
    await pool(files, 3, async (file, i) => {
      report(file.name);
      const res = await uploadFile(slug!, file, (l) => {
        loaded[i] = l;
        report(file.name);
      });
      if (!res.ok) fail.push(`${file.name}: ${res.error}`);
      loaded[i] = file.size;
      st.done++;
      report(file.name);
    });

    setProgress(null);
    setPhase(fail.length ? "error" : "done");
    setStatus(fail.length ? `Finished with ${fail.length} failure${fail.length === 1 ? "" : "s"}.` : `Uploaded ${files.length} file${files.length === 1 ? "" : "s"}.`);
    setErrors(fail);
    if (!fail.length) setTitle("");
    if (inputRef.current) inputRef.current.value = "";
    router.refresh();
  }

  return (
    <div
      onDragEnter={(e) => {
        e.preventDefault();
        dragDepth.current++;
        if (!busy) setDragging(true);
      }}
      onDragOver={(e) => e.preventDefault()}
      onDragLeave={() => {
        dragDepth.current = Math.max(0, dragDepth.current - 1);
        if (dragDepth.current === 0) setDragging(false);
      }}
      onDrop={(e) => {
        e.preventDefault();
        dragDepth.current = 0;
        setDragging(false);
        if (!busy) void begin(Array.from(e.dataTransfer.files));
      }}
      className={`rounded-2xl border p-4 transition ${
        dragging ? "border-teal-400/60 bg-teal-500/5" : "border-white/10 bg-white/3"
      }`}
    >
      <div className="flex flex-wrap items-end gap-2">
        <label className="min-w-52 flex-1">
          <span className="mb-1 block text-xs text-slate-400">Album title</span>
          <input
            value={title}
            onChange={(e) => setTitle(e.target.value)}
            placeholder={`Album — ${today}`}
            className="w-full rounded-xl px-3 py-2 text-sm outline-none focus:border-control-ink"
            disabled={busy}
          />
        </label>
        <label className="inline-flex w-full cursor-pointer items-center justify-center gap-2 rounded-xl border border-control-line bg-control px-4 py-2.5 text-center text-sm text-control-ink transition hover:brightness-95 sm:w-auto">
          <ImagePlus className="h-4 w-4" />
          Choose photos / videos
          <input
            ref={inputRef}
            type="file"
            accept="image/*,video/*"
            multiple
            className="hidden"
            onChange={(e) => void begin(Array.from(e.target.files ?? []))}
            disabled={busy}
          />
        </label>
      </div>

      {progress && (
        <div className="mt-3 space-y-1.5">
          <div className="flex items-center justify-between text-xs">
            <span className="truncate text-slate-300">
              {progress.index}/{progress.total} done — {progress.file}
            </span>
            <span className="shrink-0 pl-2 text-slate-500">
              {pct}% · {fmtMb(progress.loaded)}/{fmtMb(progress.bytes)}
            </span>
          </div>
          <div className="h-1.5 overflow-hidden rounded-full bg-white/5">
            <div
              className="h-full rounded-full bg-teal-500 transition-all duration-150"
              style={{ width: `${pct}%` }}
            />
          </div>
        </div>
      )}

      {phase !== "idle" && !progress && status && (
        <p className="mt-2 flex items-center gap-1.5 text-xs text-teal-300">
          {phase === "done" && <Check className="h-3.5 w-3.5 shrink-0" />}
          {status}
        </p>
      )}
      {errors.length > 0 && (
        <ul className="mt-2 space-y-0.5 text-xs text-rose-400">
          {take(errors, 5).map((e) => (
            <li key={e}>{e}</li>
          ))}
          {errors.length > 5 && <li>… and {errors.length - 5} more</li>}
        </ul>
      )}
      <p className="mt-2 text-[11px] text-slate-500">
        Drag &amp; drop files here or tap choose — they upload to your Telegram storage (up to 50 MB each)
        and stream back on demand.
      </p>
    </div>
  );
}

const take = <T,>(a: T[], n: number) => a.slice(0, n);
