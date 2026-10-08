import { readFileSync } from "node:fs";
import { h, escape } from "./jsx";
import { box, type Rect } from "./layout";
import { titleSize, descriptionSize, titleGap, type Theme } from "./theme";

export const FONT = 'font-family:"Booton",-apple-system,BlinkMacSystemFont,sans-serif;-webkit-font-smoothing:antialiased;';

/** The Booton weights the card sets: description, kicker, title. */
const WEIGHTS: Array<[string, number]> = [["Regular", 400], ["Medium", 500], ["Bold", 700]];

/**
 * Booton, embedded in the page as data URIs.
 *
 * The same files the chapter cards use (`components/assets/fonts`), read here
 * rather than linked: the rasteriser loads the page with read access to the
 * project folder only, so a `file://` font outside it is silently refused and
 * the card draws in the system font. Read once, when the module loads.
 */
export const FONT_FACES: string = WEIGHTS.map(([name, weight]) => {
  const bytes = readFileSync(new URL(`../components/assets/fonts/Booton-${name}.woff2`, import.meta.url));
  return `@font-face{font-family:"Booton";src:url(data:font/woff2;base64,${bytes.toString("base64")}) format("woff2");font-weight:${weight};font-style:normal;font-display:block;}`;
}).join("\n");

/**
 * The chapter card's two arcs (`components/assets/chapter-arc-*.svg`), drawn
 * across `rect` in the theme's colours.
 *
 * The paths are the assets' own; only the fill is the theme's. Stretched to the
 * panel rather than cropped from a 1920 frame, so the curves stay inside the
 * words' half of the card instead of disappearing under the photograph — at the
 * chapter card's proportions: the inner arc is 71% of the panel's width, the
 * outer 87%.
 */
export function Arcs(rect: Rect, theme: Theme): string {
  const arc = (width: number, viewW: number, d: string, fill: string) =>
    <svg style={box({ x: rect.x, y: rect.y, width: Math.round(rect.width * width), height: rect.height })}
      viewBox={`0 0 ${viewW} 1079.82`} preserveAspectRatio="none" xmlns="http://www.w3.org/2000/svg">
      <path d={d} fill={fill} />
    </svg>;
  return arc(1671.67 / 1920, 1671.67, "M1470.18 0H0V1079.82H1470.18C1835.09 602.889 1622.23 161.219 1470.18 0Z", theme.arcOuter)
    + arc(1368.21 / 1920, 1368.21, "M1203.3 0H0V1079.82H1203.3C1501.97 602.889 1327.75 161.219 1203.3 0Z", theme.arcInner);
}
export interface Point { x: number; y: number }
export interface Size { width: number; height: number }
const clamp01 = (v: number): number => (Number.isFinite(v) ? Math.max(0, Math.min(1, v)) : 0.5);
/**
 * The `object-position` that centres `point` — a place in the photograph,
 * 0..1 on each axis — inside a cover-fitted `rect`.
 *
 * `object-position: 30% 50%` does not centre the photograph's 30% mark; it
 * aligns the photograph's 30% mark with the box's 30% mark, and the visible
 * window slides accordingly. To put the face in the middle of the box the
 * window has to be placed so its own centre lands on the face, which needs the
 * photograph's size. This is the same arithmetic the recorder uses to frame the
 * live camera (`src/region/cover.rs::offset_for_point`), at zoom 1.
 *
 * It clamps rather than fails: a face near the edge slides the window flush,
 * as a human operator would. An axis with no overflow answers 0.5, because
 * nothing on that axis can move. Without a size it returns the point itself,
 * which is what the page did before it knew any better.
 */
export function objectPosition(point: Point, rect: Rect, size?: Size): Point {
  if (!size || !(size.width > 0) || !(size.height > 0)) return { x: clamp01(point.x), y: clamp01(point.y) };
  const scale = Math.max(rect.width / size.width, rect.height / size.height);
  const axis = (p: number, span: number, shown: number): number => {
    const overflow = span - shown;
    if (!(overflow > 0.5)) return 0.5;
    return clamp01((clamp01(p) * span - shown / 2) / overflow);
  };
  return { x: axis(point.x, size.width, rect.width / scale), y: axis(point.y, size.height, rect.height / scale) };
}
export function Photo(src: string, focus: Point, rect: Rect, size?: Size): string {
  const at = objectPosition(focus, rect, size);
  // `decoding="sync"`: WebKit decodes a large image off the main thread and
  // paints nothing where it goes until that finishes, so a snapshot taken on
  // the first paint of a fresh photograph came back with an empty column. The
  // rasteriser also waits for the decode itself (see src/card/raster.rs); this
  // is the page's own half of the same promise.
  return <img src={src} alt="" decoding="sync" style={box(rect) + `object-fit:cover;object-position:${at.x * 100}% ${at.y * 100}%;`} />;
}
/**
 * The words, in the chapter card's order: the kicker where its CHAPTER label
 * sits, the thin orange rule, the Bold title, and the description in the
 * muted Regular of its subtitle.
 */
export function TextPanel(title: string, description: string, kicker: string, theme: Theme, rect: Rect, ruleWidth: number): string {
  // Explicit breaks consume real lines as well as characters. Keep long words
  // within the panel and cap secondary copy so the headline retains priority.
  const size = Math.min(titleSize(title), rect.height / (title.trim().split(/\n/).length * 1.15 + 2));
  return <div style={box(rect) + "display:flex;flex-direction:column;justify-content:center;overflow:hidden;overflow-wrap:anywhere;"}>
    {kicker.trim() && <div style={`color:${theme.kicker};font-size:28px;font-weight:500;text-transform:uppercase;margin-bottom:18px;flex-shrink:0;display:-webkit-box;-webkit-box-orient:vertical;-webkit-line-clamp:2;overflow:hidden;`}>{escape(kicker.trim())}</div>}
    <div style={`width:${ruleWidth}px;max-width:100%;height:2px;background:${theme.accent};margin-bottom:22px;flex-shrink:0;`} />
    <div style={`color:${theme.title};font-size:${size}px;font-weight:700;line-height:1.11;white-space:pre-line;overflow:hidden;flex-shrink:1;`}>{escape(title.trim())}</div>
    {description.trim() && <div style={`color:${theme.description};font-size:${descriptionSize(title)}px;font-weight:400;line-height:1.3;margin-top:${titleGap(title)}px;flex-shrink:0;display:-webkit-box;-webkit-box-orient:vertical;-webkit-line-clamp:3;overflow:hidden;`}>{escape(description.trim())}</div>}
  </div>;
}
