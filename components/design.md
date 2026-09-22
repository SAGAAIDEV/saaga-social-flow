# SAAGA video template styling

Style reference noted by the user: **SAAGA Brand Guidelines, page 2**.
Implementation references: the sibling landing app's Booton font declarations
and color tokens, and the approved direction in `artifacts/content-workflow-deck`.

Use Booton Medium (500) for supporting text and chapter numbers, Semibold (600)
for headings and labels. Fonts must be local and declared inside each template.
The matching `--saaga-*` variables live on each template's `#root`, where the
HyperFrames compiler can scope them correctly.

| Token | Value | Role |
| --- | --- | --- |
| ink | #1D1D1D | Primary text on light surfaces |
| paper | #F6F6F6 | Shared canvas, chapter-card and layout surface |
| muted | #626262 | Supporting text on light panels |
| orange | #EB5201 | Brand rules, camera accents and marks |
| peach | #FFEEE5 | Camera fallback surfaces |
| line | #D1D1CC | Fine panel borders |
| orange-on-dark | #FF9561 | Reserved accent for photographic media when needed |
| radius | 24px | Rectangular camera card corners |

Chapter cards and footage openers use the same off-white canvas, dark type and
orange accents as the layouts. The chapter label and number are 40px; the topic
is the focal point (144px landscape, 112px portrait, 88px inside outline cards).
An orange rule and upright SAAGA mark form a quiet signature below it. All four
chapter variants share the same hierarchy. Titles wrap without clipping,
including long single words.
The shared badge SVG is static; the split layout's existing inline badge
animation remains controlled by its seekable timeline.

Keep the full-bleed screen and camera geometry, framing variables, audio tracks
and chapter durations. Do not add identity strips
back to the vertical or talking-head layouts. Speaker strips use an off-white
surface, dark medium-weight type and an orange top rule.

The two `outline-*` layouts are talking heads that a card cuts into. The
chapter opens full frame, exactly as the talking-head layouts do; 1.2 seconds
before the speaker reaches their first point a paper card slides in — from the
left in landscape, from the top in portrait — carrying the chapter heading at
card scale (88px in both orientations), and pushes the camera over into the
split layout's column (its bottom band) in the same 0.8-second move. Points
appear beneath the heading one at a time on the beat they are spoken, whipping
in from below; the point before steps back to muted and its badge from filled
orange to a muted outline, so the current point is the one that reads. 1.6
seconds before the cut the card slides away and the frame is a talking head
again. A chapter with no points, or too short to open, hold and close, stays a
talking head throughout. Badge digits are 32px and point text is 48px, keeping
the outline readable in both formats. The motion is transforms only — the card and the camera
wrapper translate, nothing is scaled or cropped — so the face the recorder
framed is the face in the column; `focusX` (landscape) and `focusY` (portrait)
say where in the frame the face sits so the resting column or band centres on
it. The points arrive as one JSON string variable (`outlinePoints`,
`{"points":[{"text","at"}]}`), capped at eight lines of 56 characters by the
recorder's `outline` stage, which also keeps the first point after 2.6 seconds
so the card has a talking head to arrive over.

Chapter CSS and motion have one editable source under `components/chapter/`,
embedded in the chapter card, both talking-head layouts, the vertical split and
both outline layouts. Run `node scripts/sync-chapters.mjs` after edits; run the
same command with `--check` to verify that every template contains the current
shared code.
The code is embedded into each template so render workspaces need no new
runtime dependencies. Per-template root variables specify size and color.

Cards reveal metadata, title and signature in an overlapping 0.7-second
sequence, shortened proportionally for one-second cards. `holdFromStart` and
footage openers are composed in frame zero. Openers leave with a 24px lift as
the existing blur/exposure treatment resolves; cards hold until the cut.

Generated inter-chapter cards display **source chapter number minus one**:
source chapter 2 produces card 01. Source IDs, media paths, titles and vertical
opener numbers keep their original numbering.

When adding an asset, update `library.json`. Assets used by generated chapter
renders must also be copied by `src/edit/compose.rs`; required font files must
be included in `src/preflight.rs`. A template looking correct in isolation
does not prove the generated render workspace contains its dependencies.
