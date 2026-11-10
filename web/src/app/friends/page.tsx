import { requireApiPerm } from "@/lib/api-server";
import { ConnectTelegramCard } from "@/components/ConnectTelegramCard";
import { FriendsPanel } from "@/components/FriendsPanel";

export const dynamic = "force-dynamic";

export default async function FriendsPage() {
  const user = await requireApiPerm("media:view");
  return (
    <div className="space-y-6">
      <h1 className="text-2xl font-bold text-white">Friends</h1>
      {!user.telegram_id && (
        <p className="text-xs text-slate-500">
          Tip: even without connecting your own Telegram you can invite friends — they just need the
          bot to accept your invite.
        </p>
      )}
      <FriendsPanel />
      {!user.telegram_id && <ConnectTelegramCard />}
    </div>
  );
}
