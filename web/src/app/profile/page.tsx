import { requireApiUser } from "@/lib/api-server";
import { PasswordForm } from "@/components/PasswordForm";

export default async function ProfilePage() {
  const user = await requireApiUser();
  return (
    <div className="mx-auto max-w-md space-y-6">
      <div>
        <h1 className="text-xl font-semibold">Profile</h1>
        <p className="mt-1 text-sm text-slate-400">Your account and sign-in security.</p>
      </div>

      <div className="rounded-2xl border border-white/10 bg-white/5 p-5">
        <dl className="space-y-2 text-sm">
          <div className="flex justify-between gap-4">
            <dt className="text-slate-400">Name</dt>
            <dd className="truncate font-medium">{user.name}</dd>
          </div>
          <div className="flex justify-between gap-4">
            <dt className="text-slate-400">Email</dt>
            <dd className="truncate">{user.email ?? "—"}</dd>
          </div>
          <div className="flex justify-between gap-4">
            <dt className="text-slate-400">Telegram</dt>
            <dd>{user.telegram_id ? "Linked" : "Not linked"}</dd>
          </div>
          {user.sessionCreatedAt != null && (
            <div className="flex justify-between gap-4">
              <dt className="text-slate-400">Signed in</dt>
              <dd>{new Date(user.sessionCreatedAt * 1000).toLocaleString()}</dd>
            </div>
          )}
        </dl>
      </div>

      <div className="rounded-2xl border border-white/10 bg-white/5 p-5">
        <h2 className="text-sm font-medium">Change password</h2>
        <p className="mt-0.5 text-xs text-slate-500">
          Other devices get signed out; this session stays active.
        </p>
        <PasswordForm />
      </div>
    </div>
  );
}
