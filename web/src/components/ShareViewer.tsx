"use client";

export function ShareViewer(props: {
  token: string;
  kind: "photo" | "video";
  availability: string;
}) {
  if (props.availability !== "available") {
    const dead = props.availability === "dead";
    return (
      <div
        className={`m-4 rounded-2xl border p-8 text-center text-sm ${
          dead ? "border-rose-500/20 bg-rose-500/10 text-rose-300" : "border-amber-500/20 bg-amber-500/10 text-amber-300"
        }`}
      >
        {dead
          ? "This file is no longer available — the original Telegram copy can't be reached anymore."
          : "This file is stored in Telegram but exceeds 20 MB, so it can't stream through the Bot API. It remains fully viewable in Telegram chats it was shared to."}
      </div>
    );
  }

  const src = `/api/media/by-token/${props.token}`;

  return props.kind === "photo" ? (
    // eslint-disable-next-line @next/next/no-img-element
    <img src={src} alt="Shared photo" className="max-h-[75vh] w-full bg-black object-contain" />
  ) : (
    <video
      src={src}
      controls
      playsInline
      preload="metadata"
      className="max-h-[75vh] w-full bg-black object-contain"
    />
  );
}
