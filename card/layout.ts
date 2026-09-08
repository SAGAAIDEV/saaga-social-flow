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
/**
 * Where the photograph and the text panel sit.
 *
 * Words first, then the photograph, in reading order: left then right on the
 * landscape card, top then bottom on the portrait one. The panel always starts
 * at the origin and the photo takes the far edge. On the portrait poster that
 * is also what keeps the presenter's face out from under a phone player's
 * top-edge controls and crop.
 */
export function layout(format: Format, photo: boolean) {
  const { width, height } = SIZES[format];
  const vertical = format === "vertical";
  const split = photo ? Math.round((vertical ? height : width) * 0.46) : 0;
  const panel = { x: 0, y: 0,
    width: vertical ? width : width - split, height: vertical ? height - split : height };
  const pad = vertical ? 52 : 64;
  const photoRect = vertical
    ? { x: 0, y: panel.height, width, height: split }
    : { x: panel.width, y: 0, width: split, height };
  return { width, height, vertical, panel,
    photo: photoRect,
    text: { x: panel.x + pad, y: panel.y + pad, width: panel.width - pad * 2, height: panel.height - pad * 2 },
    // The accent hairline sits on the edge the photo and the panel share.
    seam: vertical
      ? { x: 0, y: photoRect.y, width, height: 3 }
      : { x: photoRect.x, y: 0, width: 3, height } };
}
