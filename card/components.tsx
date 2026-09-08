import { h, escape } from "./jsx";
import { box, type Rect } from "./layout";
import { titleSize, descriptionSize, titleGap, type Theme } from "./theme";

export const FONT = 'font-family:-apple-system,BlinkMacSystemFont,"SF Pro Display","Inter",sans-serif;-webkit-font-smoothing:antialiased;';
export function Photo(src: string, focus: number, rect: Rect): string {
  // `decoding="sync"`: WebKit decodes a large image off the main thread and
  // paints nothing where it goes until that finishes, so a snapshot taken on
  // the first paint of a fresh photograph came back with an empty column. The
  // rasteriser also waits for the decode itself (see src/card/raster.rs); this
  // is the page's own half of the same promise.
  return <img src={src} alt="" decoding="sync" style={box(rect) + `object-fit:cover;object-position:${focus * 100}% 50%;`} />;
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
