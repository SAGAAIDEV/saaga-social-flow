/** Thumbnail composition. Geometry, typography and page output are independent. */
import { h } from "./jsx";
import { THEMES, type ThemeName } from "./theme";
import { layout, box, SIZES, type Format } from "./layout";
import { Photo, TextPanel, FONT } from "./components";
export const BASE_W = SIZES.horizontal.width;
export const BASE_H = SIZES.horizontal.height;
export interface CardProps {
  title: string;
  description: string;
  photo?: string;
  focus?: number;
  theme?: ThemeName;
  kicker?: string;
  format?: Format;
  og?: boolean;
}
export function Card(props: CardProps): string {
  const { title, description, photo, kicker = "" } = props;
  const t = THEMES[props.theme ?? "dark"] ?? THEMES.dark;
  const l = layout(props.format ?? "horizontal", Boolean(photo));
  const focus = Number.isFinite(props.focus) ? Math.max(0, Math.min(1, props.focus!)) : 0.5;
  return <div data-format={props.format ?? "horizontal"} style={box({ x: 0, y: 0, ...l }) + `background:${t.ground};overflow:hidden;` + FONT}>
    {photo && Photo(photo, focus, l.photo)}
    <div style={box(l.panel) + `background:${t.panel};`} />
    {photo && <div style={box(l.seam) + `background:${t.accent};`} />}
    {TextPanel(title, description, kicker, t, l.text)}
  </div>;
}
export { page } from "./page";
