/** Thumbnail composition. Geometry, typography and page output are independent. */
import { h } from "./jsx";
import { THEMES, type ThemeName } from "./theme";
import { layout, box, SIZES, type Format } from "./layout";
import { Photo, TextPanel, FONT, type Size } from "./components";
export const BASE_W = SIZES.horizontal.width;
export const BASE_H = SIZES.horizontal.height;
export interface CardProps {
  title: string;
  description: string;
  photo?: string;
  /** Where the presenter is in the photograph, 0..1 across; centred in the box. */
  focus?: number;
  /** And down; only moves anything when the photograph is taller than its box. */
  focusY?: number;
  /** The photograph's pixel size, which the crop needs to centre the point. */
  photoSize?: Size;
  theme?: ThemeName;
  kicker?: string;
  format?: Format;
  og?: boolean;
}
export function Card(props: CardProps): string {
  const { title, description, photo, kicker = "" } = props;
  const t = THEMES[props.theme ?? "dark"] ?? THEMES.dark;
  const l = layout(props.format ?? "horizontal", Boolean(photo));
  const clamp = (v: number | undefined): number => (Number.isFinite(v) ? Math.max(0, Math.min(1, v!)) : 0.5);
  const focus = { x: clamp(props.focus), y: clamp(props.focusY) };
  return <div data-format={props.format ?? "horizontal"} style={box({ x: 0, y: 0, ...l }) + `background:${t.ground};overflow:hidden;` + FONT}>
    {photo && Photo(photo, focus, l.photo, props.photoSize)}
    <div style={box(l.panel) + `background:${t.panel};`} />
    {photo && <div style={box(l.seam) + `background:${t.accent};`} />}
    {TextPanel(title, description, kicker, t, l.text)}
  </div>;
}
export { page } from "./page";
