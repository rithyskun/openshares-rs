import { Aperture } from "lucide-react";

/** Brand mark — gradient tile + aperture glyph. Literal colors: accents don't theme. */
export function LogoMark({ className = "h-7 w-7" }: { className?: string }) {
  return (
    <span
      className={`grid shrink-0 place-items-center rounded-lg bg-linear-to-br from-teal-400 via-sky-500 to-blue-600 text-white/90 shadow-[inset_0_1px_0_0_rgba(255,255,255,0.35),0_1px_2px_0_rgba(0,0,0,0.25)] ${className}`}
    >
      <Aperture className="h-[60%] w-[60%]" />
    </span>
  );
}
