import type { Metadata, Viewport } from "next";
import "./globals.css";

const DESCRIPTION =
  "Macros is an Emacs-style, GPU-rendered code editor scripted in Steel (Scheme). Modal editing, a built-in git porcelain, LSP, projects, org-mode, an embedded terminal — every keybinding and command is yours to redefine.";

export const metadata: Metadata = {
  metadataBase: new URL("https://macros.dev"),
  title: {
    default: "Macros — the editor you program",
    template: "%s — Macros",
  },
  description: DESCRIPTION,
  applicationName: "Macros",
  keywords: [
    "Macros",
    "text editor",
    "code editor",
    "Emacs",
    "Steel",
    "Scheme",
    "Lisp",
    "modal editing",
    "vim",
    "evil-mode",
    "LSP",
    "magit",
    "GPUI",
    "GPU rendered",
    "macOS",
    "Windows",
    "Linux",
  ],
  authors: [{ name: "The Macros team" }],
  openGraph: {
    type: "website",
    siteName: "Macros",
    title: "Macros — the editor you program",
    description: DESCRIPTION,
    url: "https://macros.dev",
  },
  twitter: {
    card: "summary_large_image",
    title: "Macros — the editor you program",
    description: DESCRIPTION,
  },
  icons: {
    icon: "/favicon-32.png",
    apple: "/brand/icon-256.png",
  },
};

export const viewport: Viewport = {
  width: "device-width",
  initialScale: 1,
};

export default function RootLayout({
  children,
}: {
  children: React.ReactNode;
}) {
  return (
    <html lang="en">
      <body>{children}</body>
    </html>
  );
}
