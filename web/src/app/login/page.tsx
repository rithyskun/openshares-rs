import { redirect } from "next/navigation";
import { maybeApiUser } from "@/lib/api-server";
import { EmailPasswordForm } from "@/components/EmailPasswordForm";

export const dynamic = "force-dynamic";

export default async function LoginPage() {
  if (await maybeApiUser()) redirect("/gallery");
  return (
    <div className="grid min-h-[calc(100dvh-12rem)] place-items-center">
      <div className="w-full max-w-sm space-y-5">
        <h1 className="text-center text-2xl font-bold text-white">Welcome back</h1>
        <EmailPasswordForm />
      </div>
    </div>
  );
}
