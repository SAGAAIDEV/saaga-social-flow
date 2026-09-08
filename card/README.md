# Thumbnail cards

One photo and one design produce the whole artwork set: **Render video and thumbnails**
draws all three destinations from the same saved card and activates them together, and
**Redraw artwork** on the Video details pane does the same by hand.

| Destination | Size | Composed on |
|---|---|---|
| `horizontal` | 1280 × 720 | the landscape artboard — YouTube, the blog's video thumbnail |
| `vertical` | 720 × 1280 | the portrait artboard — the portrait blog poster, vertical exports |
| `og` | 1200 × 630 | the landscape artboard — the blog's link preview and social image posts |

The OG image is the horizontal thumbnail at another resolution, not a second design.
1200 × 630 is 7% wider in aspect than 1280 × 720, so it is filled rather than fitted:
22px comes off the top and bottom of a card padded by 64, which costs nothing and
avoids bars down both sides of a link preview.

Both cards read words first, then the presenter: the landscape card puts the headline
on the left and the photo on the right, the portrait card puts the headline on top and
the photo beneath it — which also keeps the presenter out from under a portrait
player's top-edge controls and crop.
Without a photo, text uses the full frame. Focus controls the horizontal photo crop —
it has roughly half the travel in portrait, where the photo box is much closer to the
camera's own aspect. Keep headlines concise; secondary copy is limited to three lines.

The **format** picker chooses the aspect ratio asked of the image models under
*Optional AI image experiments*. It does not change the artwork set, which is always
all three, and it is deliberately not part of a set's identity — changing it must not
retire finished pictures or block a publish.

```sh
cargo run -- card --title 'Ship it anyway' --still photo.jpg --out /tmp/card.jpg
cargo run -- card --all-formats --title 'Ship it anyway' --still photo.jpg --out /tmp/project
cd card
bun test
bun run sheet.tsx > /tmp/card-sheet.html
```

The renderer accepts JSON on stdin (`bun run render.tsx`). `format` chooses the design
space and `og` selects the link-preview size; optional `width` and `height` scale the
composition to any output. Dimensions-only callers infer orientation. Sizes that do not
match the artboard's aspect are centred with background padding, except `og`, which
fills. An `og` payload is always composed landscape whatever `format` says.

- `layout.ts`: the two design spaces, the OG output size, and photo/text geometry.
- `components.tsx`: photo and typography components.
- `theme.ts`: brand palette and type sizing.
- `Card.tsx`: composition.
- `page.tsx`: HTML document, output scaling, and fill vs fit.
- `input.ts`: JSON validation.
- `render.tsx`: command-line entry point.

On the Rust side `card/assets.rs` owns the set — `Kind` is the only place each
destination's size and artboard are written down, and a set is activated atomically
once every render has been measured and hashed. `thumbnail/format.rs` supplies the
aspect ratio sent to image models; `thumbnail/generation.rs` owns that generation job.
