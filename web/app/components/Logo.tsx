import Link from "next/link";

/** The Macros mark — near-black "()" echoing the app icon — plus the
 *  monospace "(macros)" wordmark. Used in the nav and footer. */
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
        <linearGradient id="macros-bg" x1="0" y1="0" x2="0" y2="1">
          <stop offset="0" stopColor="#f1ece2" />
          <stop offset="1" stopColor="#e2d9c8" />
        </linearGradient>
      </defs>
      <rect x="32" y="32" width="960" height="960" rx="224" fill="url(#macros-bg)" />
      <g
        fill="none"
        stroke="#16140f"
        strokeWidth="64"
        strokeLinecap="round"
      >
        <path d="M430,288 Q260,512 430,736" />
        <path d="M594,288 Q764,512 594,736" />
      </g>
    </svg>
  );
}
