import { describe, expect, test } from "bun:test";
import { parsePayload } from "./input";
import { page } from "./page";
import { layout } from "./layout";
import { objectPosition } from "./components";

describe("thumbnail formats", () => {
  for (const format of ["horizontal", "vertical"] as const) {
    test(`${format} produces a complete frame with and without a photo`, () => {
      const p = parsePayload({ title: "Ship it", format });
      expect([p.width, p.height]).toEqual(format === "vertical" ? [720, 1280] : [1280, 720]);
      for (const photo of [false, true]) {
        const l = layout(format, photo);
        expect(l.text.x + l.text.width).toBeLessThanOrEqual(l.width);
        expect(l.text.y + l.text.height).toBeLessThanOrEqual(l.height);
        expect(l.panel.width * l.panel.height + l.photo.width * l.photo.height).toBe(l.width * l.height);
        expect(page({ ...p, photo: photo ? "file:///photo.jpg" : undefined })).toContain('transform:scale(1)');
      }
    });
  }
  /** Both cards read words first, then the presenter: top-down, or left-right. */
  test("the text panel leads and the photo takes the far edge", () => {
    const v = layout("vertical", true);
    expect(v.panel.y).toBe(0);
    expect(v.photo.y).toBe(v.panel.height);
    expect(v.photo.y + v.photo.height).toBe(v.height);
    expect(v.seam.y).toBe(v.photo.y);
    const h = layout("horizontal", true);
    expect(h.panel.x).toBe(0);
    expect(h.photo.x).toBe(h.panel.width);
    expect(h.photo.x + h.photo.width).toBe(h.width);
    expect(h.seam.x).toBe(h.photo.x);
  });
  test("legacy dimension-only callers get portrait composition", () => {
    expect(page({ title: "Portrait", description: "", width: 720, height: 1280 })).toContain('data-format="vertical"');
  });
  test("arbitrary output sizes fit the whole design without distortion", () => {
    expect(page({ title: "Square", description: "", width: 1000, height: 1000 })).toContain('transform:scale(0.78125)');
  });
  test("user text is escaped", () => {
    const html = page(parsePayload({ title: '<script>alert("x")</script>', description: "A & B" }));
    expect(html).not.toContain('<script>');
    expect(html).toContain('A &amp; B');
  });
  test("malformed inputs fail at the boundary", () => {
    for (const bad of [null, [], {}, { title: "" }, { title: "x", width: "720" }, { title: "x", height: Infinity }, { title: "x", description: {} }, { title: "x", format: "square" }, { title: "x", focus: NaN }, { title: "x", focusY: "0.3" }, { title: "x", photoSize: [1920] }, { title: "x", photoSize: [0, 1080] }, { title: "x", og: "yes" }]) {
      expect(() => parsePayload(bad)).toThrow();
    }
  });
});

describe("the OG image is the thumbnail at another resolution", () => {
  const brief = { title: "Everything we got wrong about evaluating agents", description: "A year of dashboards nobody read.", kicker: "LESSONS", photo: "file:///still.jpg" };

  /** The whole point: one composition, two output sizes. */
  test("it composes on the horizontal artboard and fills 1200x630", () => {
    const og = parsePayload({ ...brief, og: true });
    expect([og.width, og.height]).toEqual([1200, 630]);
    const html = page(og);
    // 1200/1280 is the larger of the two ratios, so the width lands exactly and
    // the overflow is vertical: 22.5px off each of the top and bottom.
    expect(html).toContain('width:1280px;height:720px;transform:scale(0.9375)');
    expect(html).toContain('left:0px;top:-22.5px;');
  });

  test("the crop stays inside the padding, so no text or seam is lost", () => {
    const l = layout("horizontal", true);
    const lost = (l.height - 630 / 0.9375) / 2;
    expect(lost).toBeCloseTo(24, 0);
    expect(lost).toBeLessThan(l.text.y - l.panel.y);
  });

  /** Same geometry in, same geometry out — only the scale may differ. */
  test("every element is the thumbnail's, laid out identically", () => {
    // The body only: the <style> block carries the page frame, which is the one
    // thing that is *meant* to differ between the two output sizes.
    const geometry = (html: string) =>
      html.split("<body>")[1].match(/left:-?[\d.]+px;top:-?[\d.]+px;width:\d+px;height:\d+px/g);
    expect(geometry(page(parsePayload({ ...brief, og: true }))))
      .toEqual(geometry(page(parsePayload(brief))));
  });

  /** A portrait project still gets a landscape link preview. */
  test("a vertical design cannot be filled into a landscape preview", () => {
    const og = parsePayload({ ...brief, format: "vertical", og: true });
    expect(og.format).toBe("horizontal");
    expect(page(og)).toContain('data-format="horizontal"');
  });
});

describe("the photo crop centres on the face", () => {
  const still = { width: 1920, height: 1080 };
  const h = layout("horizontal", true).photo;
  const v = layout("vertical", true).photo;

  /** The landscape column shows under half the still's width, so where the
   *  window sits is everything. A face a quarter of the way across has to end
   *  up in the middle of the column, not a quarter of the way across it. */
  test("a face left of centre slides the window left, and lands centred", () => {
    const at = objectPosition({ x: 0.25, y: 0.5 }, h, still);
    const scale = Math.max(h.width / still.width, h.height / still.height);
    const shown = h.width / scale;
    const left = at.x * (still.width - shown);
    expect(left + shown / 2).toBeCloseTo(0.25 * still.width, 6);
    expect(at.x).toBeLessThan(0.25);
    expect(at.y).toBe(0.5);
  });
  test("a centred face is the centred crop, in both layouts", () => {
    expect(objectPosition({ x: 0.5, y: 0.5 }, h, still)).toEqual({ x: 0.5, y: 0.5 });
    expect(objectPosition({ x: 0.5, y: 0.5 }, v, still)).toEqual({ x: 0.5, y: 0.5 });
  });
  test("a face at the edge slides the window flush rather than off the still", () => {
    expect(objectPosition({ x: 0.02, y: 0.5 }, h, still).x).toBe(0);
    expect(objectPosition({ x: 0.98, y: 0.5 }, h, still).x).toBe(1);
  });
  /** Neither photo box is taller than a landscape still, so the vertical
   *  axis has nothing to move and answers centred whatever the face does. */
  test("an axis with no overflow stays centred", () => {
    expect(objectPosition({ x: 0.5, y: 0.1 }, h, still).y).toBe(0.5);
    expect(objectPosition({ x: 0.5, y: 0.9 }, v, still).y).toBe(0.5);
    // A portrait camera does overflow vertically, and then y moves.
    expect(objectPosition({ x: 0.5, y: 0.2 }, h, { width: 1080, height: 1920 }).y).toBeLessThan(0.5);
  });
  test("without the still's size the point is used as the position, as before", () => {
    expect(objectPosition({ x: 0.3, y: 0.5 }, h)).toEqual({ x: 0.3, y: 0.5 });
    expect(objectPosition({ x: NaN, y: 2 }, h)).toEqual({ x: 0.5, y: 1 });
  });
  test("the page carries the centred position, not the raw point", () => {
    const html = page(parsePayload({ title: "Face", photo: "file:///still.jpg", focus: 0.25, focusY: 0.5, photoSize: [1920, 1080] }));
    expect(html).toContain("object-position:");
    expect(html).not.toContain("object-position:25% 50%");
    const legacy = page(parsePayload({ title: "Face", photo: "file:///still.jpg", focus: 0.25 }));
    expect(legacy).toContain("object-position:25% 50%");
  });
});
