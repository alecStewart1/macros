"use client";

import { useState } from "react";

/** A screenshot frame that degrades to a labeled placeholder until the image
 *  file exists. Drop the file at public/<src> and it appears automatically. */
export function Shot({ src, alt }: { src: string; alt: string }) {
  const [ok, setOk] = useState(true);

  return (
    <div
      style={{
        position: "relative",
        aspectRatio: "16 / 10",
        background:
          "linear-gradient(135deg, var(--surface), var(--card-hover))",
      }}
    >
      {ok ? (
        // eslint-disable-next-line @next/next/no-img-element
        <img
          src={src}
          alt={alt}
          onError={() => setOk(false)}
          style={{
            position: "absolute",
            inset: 0,
            width: "100%",
            height: "100%",
            objectFit: "cover",
            objectPosition: "top",
          }}
        />
      ) : (
        <div
          className="mono"
          style={{
            position: "absolute",
            inset: 0,
            display: "flex",
            flexDirection: "column",
            gap: 8,
            alignItems: "center",
            justifyContent: "center",
            color: "var(--text3)",
            fontSize: 13,
          }}
        >
          <span style={{ fontSize: 26, opacity: 0.5 }}>( )</span>
          <span>{src}</span>
        </div>
      )}
    </div>
  );
}
