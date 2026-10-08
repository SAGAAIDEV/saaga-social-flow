/**
 * SAAGA's palette and the type ramp, in one place.
 *
 * Copied from `saaga-landing/src/app/globals.css` rather than imported: this
 * package renders a picture with `bun run` and no dependency graph, and reaching
 * into another repo's build to read six hex values would trade that for a
 * coupling that breaks whenever the site reorganises.
 */

export const BRAND = {
  black: "#1D1D1D",
  darkGrey: "#626262",
  orange: "#EB5201",
  white: "#F6F6F6",
  lightGray: "#D1D1CC",
  mustard: "#FFEEE5",
  lightMustard: "#FFF9F6",
  blue: "#579DC4",
  // The chapter card's own colours (Figma "Chapter Card", node 67:987), so a
  // thumbnail reads as the same family as the cards inside the video.
  paper: "#FFFFFF",
  chapterPeach: "#FDEEE6",
  chapterBlush: "#FFF4EE",
  chapterMuted: "#555554",
} as const;

export type ThemeName = "light" | "dark";

export interface Theme {
  /** Behind the text panel on the landscape card, under the arcs. */
  panel: string;
  /** The portrait card's panel: flat, as the portrait chapter card is. */
  portraitPanel: string;
  /** The chapter card's two arcs, sweeping in from the left behind the type. */
  arcOuter: string;
  arcInner: string;
  /** The kicker, set like the chapter card's CHAPTER label. */
  kicker: string;
  /** Behind the whole card, seen only in the seam and the scrim. */
  ground: string;
  title: string;
  description: string;
  accent: string;
  /** The hairline between the photo and the panel. */
  rule: string;
}

/**
 * Light is the chapter card's look and the default; dark is the one that
 * survives a YouTube grid, where every neighbouring thumbnail is fighting for
 * the same eye.
 */
export const THEMES: Record<ThemeName, Theme> = {
  light: {
    panel: BRAND.paper,
    portraitPanel: BRAND.chapterPeach,
    arcOuter: BRAND.chapterBlush,
    arcInner: BRAND.chapterPeach,
    kicker: BRAND.chapterMuted,
    ground: BRAND.lightMustard,
    title: BRAND.black,
    description: BRAND.chapterMuted,
    accent: BRAND.orange,
    rule: BRAND.lightGray,
  },
  // The chapter card's layout on ink, arcs and all, a shade apart so they read
  // as the same shapes without competing with the headline.
  dark: {
    panel: BRAND.black,
    portraitPanel: BRAND.black,
    arcOuter: "#222220",
    arcInner: "#292927",
    kicker: "#A5A5A0",
    ground: "#141414",
    title: BRAND.white,
    // Not `darkGrey` on black: #626262 on #1D1D1D is a 2.6:1 contrast ratio and
    // illegible at the 210px-wide thumbnail YouTube actually shows.
    description: "#A5A5A0",
    accent: BRAND.orange,
    rule: "#333331",
  },
};

/**
 * How big the title is set, by how much of it there is.
 *
 * A ramp rather than measured text, because measuring would mean laying the
 * page out twice and the second pass would have to happen inside the browser.
 * The breakpoints are in characters and were chosen against the panel width at
 * 1280x720: 26 characters is about two words that fill a line at the largest
 * size, and past 90 the title is a sentence and stops being a thumbnail.
 *
 * Every size is in the 1280-wide card's own pixels; the renderer scales the
 * whole page for other output sizes, so these never change.
 */
export function titleSize(title: string): number {
  const length = title.trim().length;
  if (length <= 26) return 108;
  if (length <= 44) return 88;
  if (length <= 64) return 72;
  if (length <= 90) return 60;
  return 50;
}

/**
 * The description tracks the title down, but never below readable.
 *
 * The floor is the load-bearing number. YouTube shows a thumbnail about 210px
 * wide in a grid, which is a sixth of this card — so 30px here is 5px there, and
 * that is already the point where a second line stops being read and starts
 * being texture. Anything smaller is decoration.
 */
export function descriptionSize(title: string): number {
  return Math.max(30, Math.round(titleSize(title) * 0.33));
}

/**
 * The gap between the title and the description.
 *
 * Proportional to the title, because the space a headline needs under it scales
 * with how big it is — but floored, because a long title shrinks to 50px and a
 * 17px gap puts the description inside its descenders.
 */
export function titleGap(title: string): number {
  return Math.max(24, Math.round(titleSize(title) * 0.34));
}
