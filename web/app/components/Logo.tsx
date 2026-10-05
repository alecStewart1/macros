import { useId } from "react";
import Link from "next/link";

/** The Macros mark — "()" stroked in the viridis heat ramp on a dark ink
 *  squircle, matching the app icon — plus the monospace "(macros)"
 *  wordmark. Used in the nav and footer. */
export function Logo({ size = 26 }: { size?: number }) {
  return (
    <Link
      href="/"
      style={{
        display: "inline-flex",
        alignItems: "center",
        gap: 9,
        textDecoration: "none",
        color: "var(--text)",
      }}
    >
      <Mark size={size} />
      <span
        className="mono"
        style={{
          fontSize: size * 0.62,
          fontWeight: 600,
          letterSpacing: "0.5px",
          color: "var(--text)",
        }}
      >
        (macros)
      </span>
    </Link>
  );
}

export function Mark({ size = 26 }: { size?: number }) {
  // Mirrors assets/brand/logo.svg: "()" in the viridis heat ramp on dark ink.
  // useId keeps gradient ids unique when the mark appears twice on a page
  // (nav + footer), so one instance can't hijack the other's paint.
  const id = useId().replace(/:/g, "");
  return (
    <svg
      width={size}
      height={size}
      viewBox="0 0 1024 1024"
      xmlns="http://www.w3.org/2000/svg"
      aria-hidden="true"
      style={{ display: "block", borderRadius: size * 0.22 }}
    >
      <defs>
        <linearGradient id={`${id}-bg`} x1="0" y1="0" x2="0" y2="1">
          <stop offset="0" stopColor="#1b1826" />
          <stop offset="1" stopColor="#0b0a10" />
        </linearGradient>
        <linearGradient
          id={`${id}-heat`}
          gradientUnits="userSpaceOnUse"
          x1="0"
          y1="774"
          x2="0"
          y2="250"
        >
          <stop offset="0" stopColor="#365c8d" />
          <stop offset="0.35" stopColor="#1fa187" />
          <stop offset="0.7" stopColor="#a0da39" />
          <stop offset="1" stopColor="#fde725" />
        </linearGradient>
      </defs>
      <rect x="32" y="32" width="960" height="960" rx="224" fill={`url(#${id}-bg)`} />
      {/* Edge stroke: much heavier than logo.svg's 3-unit hairline because at
          16–32 px a hairline renders to nothing; the mark needs a visible rim
          to separate from the dark nav/footer. Inset by half the stroke width
          (12) so the whole stroke stays inside the squircle instead of half
          of it being clipped by the svg's border-radius. */}
      <rect
        x="44"
        y="44"
        width="936"
        height="936"
        rx="212"
        fill="none"
        stroke="#f2eff7"
        strokeOpacity="0.14"
        strokeWidth="24"
      />
      <g fill="none" stroke={`url(#${id}-heat)`} strokeWidth="80" strokeLinecap="round">
        <path d="M430,288 Q260,512 430,736" />
        <path d="M594,288 Q764,512 594,736" />
      </g>
    </svg>
  );
}
