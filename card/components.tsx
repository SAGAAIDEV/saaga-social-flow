import { h, escape } from "./jsx";
import { box, type Rect } from "./layout";
import { titleSize, descriptionSize, titleGap, type Theme } from "./theme";

export const FONT = 'font-family:-apple-system,BlinkMacSystemFont,"SF Pro Display","Inter",sans-serif;-webkit-font-smoothing:antialiased;';
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
export function TextPanel(title: string, description: string, kicker: string, theme: Theme, rect: Rect): string {
  // Explicit breaks consume real lines as well as characters. Keep long words
  // within the panel and cap secondary copy so the headline retains priority.
  const size = Math.min(titleSize(title), rect.height / (title.trim().split(/\n/).length * 1.15 + 2));
  return <div style={box(rect) + "display:flex;flex-direction:column;justify-content:center;overflow:hidden;overflow-wrap:anywhere;"}>
    {kicker.trim() && <div style={`color:${theme.accent};font-size:24px;font-weight:700;letter-spacing:.12em;text-transform:uppercase;margin-bottom:20px;flex-shrink:0;display:-webkit-box;-webkit-box-orient:vertical;-webkit-line-clamp:2;overflow:hidden;`}>{escape(kicker.trim())}</div>}
    <div style={`color:${theme.title};font-size:${size}px;font-weight:800;line-height:1.04;letter-spacing:-.032em;white-space:pre-line;overflow:hidden;flex-shrink:1;`}>{escape(title.trim())}</div>
    {description.trim() && <div style={`color:${theme.description};font-size:${descriptionSize(title)}px;font-weight:450;line-height:1.34;margin-top:${titleGap(title)}px;border-left:4px solid ${theme.accent};padding-left:20px;flex-shrink:0;display:-webkit-box;-webkit-box-orient:vertical;-webkit-line-clamp:3;overflow:hidden;`}>{escape(description.trim())}</div>}
  </div>;
}
