import { describe, expect, test } from "bun:test";
import { parsePayload } from "./input";
import { page } from "./page";
import { layout } from "./layout";

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
    for (const bad of [null, [], {}, { title: "" }, { title: "x", width: "720" }, { title: "x", height: Infinity }, { title: "x", description: {} }, { title: "x", format: "square" }, { title: "x", focus: NaN }, { title: "x", og: "yes" }]) {
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
