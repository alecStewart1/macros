import localFont from "next/font/local";

/* The editor's own chart glyph font (assets/fonts/MacrosPlot-Regular.ttf):
   braille + block elements drawn at exactly a 0.6 em monospace cell, so text
   charts stay column-aligned with IBM Plex Mono. Applied site-wide from the
   root layout as `--font-plot`, so home-page art and docs code blocks share it. */
export const plotFont = localFont({
  src: "./MacrosPlot-Regular.ttf",
  // Chart glyphs only (blocks, ■, braille). The font also carries a blank
  // "m" (gpui won't load a font without one); limiting the range keeps every
  // letter on the text font instead.
  declarations: [{ prop: "unicode-range", value: "U+2580-25A0, U+2800-28FF" }],
  variable: "--font-plot",
  display: "swap",
  adjustFontFallback: false,
});
