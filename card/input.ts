import type { CardProps } from "./Card";
import { SIZES, OG_SIZE, type Format } from "./layout";
/** Validate the JSON boundary before emitting any HTML. */
export function parsePayload(value: unknown): CardProps & { width: number; height: number } {
  if (!value || typeof value !== "object" || Array.isArray(value)) throw new Error("expected a JSON object");
  const p = value as Record<string, unknown>;
  if (typeof p.title !== "string" || !p.title.trim()) throw new Error("title must be a non-empty string");
  for (const key of ["description", "kicker", "photo"]) {
    if (p[key] !== undefined && typeof p[key] !== "string") throw new Error(`${key} must be a string`);
  }
  if (p.theme !== undefined && p.theme !== "dark" && p.theme !== "light") throw new Error("theme must be dark or light");
  if (p.format !== undefined && p.format !== "horizontal" && p.format !== "vertical") throw new Error("format must be horizontal or vertical");
  if (p.og !== undefined && typeof p.og !== "boolean") throw new Error("og must be boolean");
  for (const key of ["width", "height"]) {
    if (p[key] !== undefined && (typeof p[key] !== "number" || !Number.isInteger(p[key]) || (p[key] as number) <= 0 || (p[key] as number) > 8192)) throw new Error(`${key} must be an integer between 1 and 8192`);
  }
  for (const key of ["focus", "focusY"]) {
    if (p[key] !== undefined && (typeof p[key] !== "number" || !Number.isFinite(p[key]))) throw new Error(`${key} must be finite`);
  }
  let photoSize: { width: number; height: number } | undefined;
  if (p.photoSize !== undefined) {
    const s = p.photoSize;
    const pair = Array.isArray(s) && s.length === 2 && s.every((n) => typeof n === "number" && Number.isFinite(n) && n > 0);
    if (!pair) throw new Error("photoSize must be [width, height], both positive");
    photoSize = { width: (s as number[])[0], height: (s as number[])[1] };
  }
  // A link preview is always landscape, so an OG image composes from the
  // horizontal artboard whatever the project's own format is. Enforced here
  // rather than trusted to the caller: a portrait design filled into 1200x630
  // would crop away more than half of it, and nothing downstream would say so.
  const format: Format = p.og
    ? "horizontal"
    : (p.format as Format ?? (Number(p.height) > Number(p.width) ? "vertical" : "horizontal"));
  const base = p.og ? OG_SIZE : SIZES[format];
  return { ...p, title: p.title, description: p.description ?? "", format, photoSize, width: p.width ?? base.width, height: p.height ?? base.height } as ReturnType<typeof parsePayload>;
}
