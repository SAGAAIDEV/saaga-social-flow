import { h } from "./jsx";
import { Card, type CardProps } from "./Card";
import { SIZES } from "./layout";
/**
 * The card at the requested output size.
 *
 * The composition is always the format's own artboard and only the scale
 * changes, so every destination is the same picture rather than a second design
 * that has to be kept in step.
 *
 * `og` fills instead of fitting. 1200x630 is 7% wider in aspect than 1280x720,
 * so fitting it would put bars down both sides of a link preview; filling it
 * takes 22px off the top and bottom of a card padded by 64, which costs nothing.
 */
export function page(props: CardProps & { width: number; height: number }): string {
  const format = props.format ?? (props.height > props.width ? "vertical" : "horizontal");
  const base = SIZES[format];
  const fit = props.og ? Math.max : Math.min;
  const scale = fit(props.width / base.width, props.height / base.height);
  const x = (props.width - base.width * scale) / 2;
  const y = (props.height - base.height * scale) / 2;
  return "<!doctype html>" + <html><head><meta charset="utf-8" /><style>{`
    * { margin:0;padding:0;box-sizing:border-box; }
    html,body { width:${props.width}px;height:${props.height}px;overflow:hidden;background:${props.theme === "light" ? "#F6F6F6" : "#1D1D1D"}; }
    #card { position:absolute;left:${x}px;top:${y}px;width:${base.width}px;height:${base.height}px;transform:scale(${scale});transform-origin:0 0; }
  `}</style></head><body><div id="card">{Card({ ...props, format })}</div></body></html>;
}
