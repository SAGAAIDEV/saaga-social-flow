#!/usr/bin/env bun
/**
 * The card's only entry point: JSON in, HTML out.
 *
 * ```
 * echo '{"title":"Ship it anyway","description":"…"}' | bun run card/render.tsx
 * ```
 *
 * Stdout is the page and nothing else, because the caller is a Rust process that
 * pipes it straight into a webview — so every diagnostic goes to stderr and a
 * bad payload exits non-zero rather than printing half a card.
 *
 * Run it by hand with `--open` to write the page to a temp file and open it in a
 * browser: iterating on the layout should not need the recorder running.
 */

import { page, BASE_W, BASE_H, type CardProps } from "./Card";
import { parsePayload } from "./input";

interface Payload extends CardProps {
  width?: number;
  height?: number;
}

function usage(message: string): never {
  console.error(`card/render.tsx: ${message}

Reads a JSON object on stdin:
  title        required, the headline
  description  optional, the line under it
  photo        optional, a file:// URL for the camera still
  focus        optional, 0..1, where across the still the subject sits
  theme        optional, "dark" (default) or "light"
  kicker       optional, small orange word above the title
  format       optional, "horizontal" (default) or "vertical"
  width/height optional, output pixels (default ${BASE_W}x${BASE_H})`);
  process.exit(2);
}

const raw = await Bun.stdin.text();
if (!raw.trim()) usage("nothing on stdin");

let payload: Payload;
try {
  payload = JSON.parse(raw);
} catch (error) {
  usage(`stdin is not JSON: ${error}`);
}

let html: string;
try {
  html = page(parsePayload(payload));
} catch (error) {
  usage(String(error));
}

if (process.argv.includes("--open")) {
  const path = `${process.env.TMPDIR ?? "/tmp"}/saaga-card-preview.html`;
  await Bun.write(path, html);
  console.error(`wrote ${path}`);
  Bun.spawn(["open", path]);
} else {
  process.stdout.write(html);
}
