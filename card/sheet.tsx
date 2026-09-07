#!/usr/bin/env bun
/**
 * A contact sheet: every theme against every title length, on one page.
 *
 * The layout has one hard problem — a headline whose length nobody controls has
 * to look deliberate at any size — and it is not a problem you can reason about,
 * only look at. `theme.ts`'s ramp is the answer, and this is how you tell
 * whether the answer is right: change a breakpoint, run this, look.
 *
 * ```
 * bun run card/sheet.tsx [/path/to/a/still.jpg] > sheet.html && open sheet.html
 * ```
 *
 * The photograph is optional and any camera still will do; without one every
 * card draws as a title card, which is also worth seeing.
 */

import { Card, BASE_W, BASE_H, type CardProps } from "./Card";
import { SIZES, type Format } from "./layout";
import { THEMES, type ThemeName } from "./theme";

/** Small enough to see three at once, big enough to judge the type. */
const SCALE = 0.42;

const photoPath = process.argv[2];
const photo = photoPath ? `file://${encodeURI(photoPath)}` : undefined;

/**
 * One title per step of the ramp, so a breakpoint that is in the wrong place
 * shows up as one card that looks unlike its neighbours.
 */
const TITLES: Array<{ title: string; description: string; kicker: string }> = [
  {
    title: "Ship it anyway",
    description: "Why the retry storm took the queue down.",
    kicker: "SAAGA",
  },
  {
    title: "The retry storm that took our queue down",
    description: "Two lines of code, eleven hours, and a bill nobody approved.",
    kicker: "POSTMORTEM",
  },
  {
    title: "How we cut our LLM bill by 80% without changing a single prompt",
    description: "Caching, routing, and the one measurement that made both obvious.",
    kicker: "",
  },
  {
    // Past the last breakpoint: the case that has to still look like a card
    // rather than like a paragraph someone set in bold.
    title:
      "Everything we got wrong about evaluating agents, and the three measurements that finally told us something",
    description: "A year of dashboards nobody read, and what replaced them.",
    kicker: "LESSONS",
  },
];

function tile(props: CardProps): string {
  const { width: BASE_W, height: BASE_H } = SIZES[props.format ?? "horizontal"];
  return `<figure style="margin:0 0 14px;">
  <div style="width:${BASE_W * SCALE}px;height:${BASE_H * SCALE}px;overflow:hidden;">
    <div style="width:${BASE_W}px;height:${BASE_H}px;position:relative;transform:scale(${SCALE});transform-origin:0 0;">${Card(props)}</div>
  </div>
  <figcaption style="font:11px -apple-system,sans-serif;color:#333;padding-top:4px;">
    ${props.format} · ${props.theme} · ${props.title.length} chars
  </figcaption>
</figure>`;
}

const columns = (Object.keys(THEMES) as ThemeName[])
  .map(
    (theme) =>
      `<div>${(["horizontal", "vertical"] as Format[]).map(format => TITLES.map((t) => tile({ ...t, theme, photo, format })).join("")).join("")}</div>`,
  )
  .join("");

console.log(`<!doctype html><meta charset="utf-8">
<style>
  * { margin:0; padding:0; box-sizing:border-box; }
  /* One column, not one per theme: a contact sheet is read by scrolling, and a
     side-by-side pair is narrower than either card needs to be legible. */
  body { background:#8a8a8a; padding:14px; width:${BASE_W * SCALE + 28}px; }
</style>
${columns}`);
