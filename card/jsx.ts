/**
 * A JSX factory that returns HTML, in about forty lines.
 *
 * The whole reason this file exists rather than a `react` dependency: the card
 * is a static picture rendered once and rasterised. Nothing about it re-renders,
 * responds to an event or holds state, so a virtual DOM would be machinery for a
 * job with no moving parts — and it would put this package behind an `npm
 * install` against a private registry whose token expires daily.
 *
 * Bun transpiles TSX with `jsxFactory: "h"` out of the box, so `bun run` on a
 * `.tsx` file is the entire toolchain.
 */

/** Anything that can appear between tags. `false`/`null`/`undefined` render as
 *  nothing, so `{cond && <div/>}` works the way it does in React. */
export type Child = string | number | false | null | undefined | Child[];

/** Tags with no closing form. Emitting `</img>` is invalid and WebKit's parser
 *  recovers from it in ways that move the layout. */
const VOID = new Set(["img", "br", "hr", "meta", "link", "input", "source"]);

/**
 * Escapes text going into markup.
 *
 * Applied to children and to attribute values, never to the `style` strings
 * this file's components build — those are ours. A title arrives from a text
 * field a person typed, so an unescaped `<` in it would silently eat the rest
 * of the card.
 */
export function escape(value: string): string {
  return value
    .replace(/&/g, "&amp;")
    .replace(/</g, "&lt;")
    .replace(/>/g, "&gt;")
    .replace(/"/g, "&quot;");
}

function flatten(children: Child[]): string {
  return children
    .flat(Infinity)
    .filter((child): child is string | number =>
      child !== false && child !== null && child !== undefined,
    )
    .map((child) => (typeof child === "number" ? String(child) : child))
    .join("");
}

/**
 * `h("div", {class: "a"}, ...)` → `<div class="a">…</div>`.
 *
 * A function component is called with its props, exactly as the classic runtime
 * does, so `<Card title="…"/>` works with no other machinery.
 */
export function h(
  tag: string | ((props: Record<string, unknown>) => string),
  props: Record<string, unknown> | null,
  ...children: Child[]
): string {
  if (typeof tag === "function") {
    return tag({ ...(props ?? {}), children });
  }
  const attrs = Object.entries(props ?? {})
    .filter(([, value]) => value !== false && value != null)
    .map(([name, value]) => ` ${name}="${escape(String(value))}"`)
    .join("");
  if (VOID.has(tag)) {
    return `<${tag}${attrs}>`;
  }
  return `<${tag}${attrs}>${flatten(children)}</${tag}>`;
}

/** `<>…</>`, for a component returning siblings. */
export function Fragment({ children }: { children?: Child[] }): string {
  return flatten(children ?? []);
}

/** Children already escaped by `escape`, or markup we built ourselves. */
export function raw(html: string): string {
  return html;
}
