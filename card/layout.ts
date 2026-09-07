/** Fixed design spaces: each orientation composes independently, then scales. */
export type Format = "horizontal" | "vertical";
export const SIZES = {
  horizontal: { width: 1280, height: 720 },
  vertical: { width: 720, height: 1280 },
} as const;
/**
 * The OG image's output size, and the only place it is written down on this
 * side. It is not a design space: an OG image is the horizontal card at another
 * resolution, so nothing composes at 1200x630 — see `page`.
 */
export const OG_SIZE = { width: 1200, height: 630 } as const;
export interface Rect { x: number; y: number; width: number; height: number }
export function box(r: Rect): string {
  return `position:absolute;left:${r.x}px;top:${r.y}px;width:${r.width}px;height:${r.height}px;`;
}
export function layout(format: Format, photo: boolean) {
  const { width, height } = SIZES[format];
  const vertical = format === "vertical";
  const split = photo ? Math.round((vertical ? height : width) * 0.46) : 0;
  const panel = { x: vertical ? 0 : split, y: vertical ? split : 0,
    width: vertical ? width : width - split, height: vertical ? height - split : height };
  const pad = vertical ? 52 : 64;
  return { width, height, vertical, panel,
    photo: { x: 0, y: 0, width: vertical ? width : split, height: vertical ? split : height },
    text: { x: panel.x + pad, y: panel.y + pad, width: panel.width - pad * 2, height: panel.height - pad * 2 },
    seam: { x: panel.x, y: panel.y, width: vertical ? width : 3, height: vertical ? 3 : height } };
}
