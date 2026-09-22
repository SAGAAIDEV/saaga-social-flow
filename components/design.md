# SAAGA video template styling

Style reference noted by the user: **SAAGA Brand Guidelines, page 2**.
Implementation references: the sibling landing app's Booton font declarations
and color tokens, and the approved direction in `artifacts/content-workflow-deck`.

Chapter cards and outline cards follow the Figma file "Video Layouts and
Thumbnails", section **Chapter Card** (node 67:987): the left-aligned card with
peach arcs, and Topic Card option 2.

Use Booton Medium (500) for supporting text and point text, Semibold (600) for
labels, Bold (700) for titles and Heavy (800) for the chapter number. Fonts must be local and declared inside each template.
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
| chapter peach | #FDEEE6 | Chapter card inner arc, portrait chapter surface, outline cards |
| chapter blush | #FFF4EE | Chapter card outer arc |
| chapter muted | #555554 | The CHAPTER label |
| badge card | #FCFCFB | Outline badges for points already covered |
| badge border | #CACAC5 | Outline badge outline |

Chapter cards and footage openers share one hierarchy, left-aligned: a muted
CHAPTER label centred over a large orange Heavy number, a thin orange rule,
then the Bold title. Landscape: label 41px, number 188px, 392px x 2px rule,
title 107px, 122px from the left edge and lifted 77px above centre, on white
with two peach arcs (`assets/chapter-arc-outer.svg`, `-inner.svg`). Portrait
(Figma drawn at 375 wide, x2.88): label 64px, number 292px, 648px rule, title
104px, 89px from the left, centred, on flat chapter peach. Titles wrap without
clipping, including long single words. The SAAGA mark is no longer part of the
chapter signature; the split layout still draws its own inline badge.

Keep the full-bleed screen and camera geometry, framing variables, audio tracks
and chapter durations. Do not add identity strips
back to the vertical or talking-head layouts. Speaker strips use an off-white
surface, dark medium-weight type and an orange top rule.

The two `outline-*` layouts are talking heads that a card cuts into. The
chapter opens full frame, exactly as the talking-head layouts do; 1.2 seconds
before the speaker reaches their first point a chapter-peach card slides in —
from the right in landscape, from the top in portrait — and pushes the camera
into a 784px column on the left (the bottom band in portrait) in the same
0.8-second move. The card's heading is the Topic Card pill: the number and
CHAPTER in one orange-outlined pill, then the chapter title with a short orange
rule under it. Points appear beneath one at a time on the beat they are spoken,
whipping in from below, each behind a rounded-square number badge: the current
point's badge is filled orange, the ones before it step back to card white with
dark digits; the text stays black. 1.6 seconds before the cut the card slides
away and the frame is a talking head again. A chapter with no points, or too
short to open, hold and close, stays a talking head throughout. Landscape sizes
are Figma's (title 78px, badges 85px, point text 40px); portrait is Figma x2.88
(title 91px, badges 99px, point text 52px). A card with five to eight points
adds `is-dense`, which scales everything by 0.75 in landscape and 0.62 in
portrait so eight points fit; it is decided from the count, not measured. The motion is transforms only — the card and the camera
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

Cards reveal the label and number, then draw the rule, then raise the title, in
an overlapping 0.7-second sequence, shortened proportionally for one-second cards. `holdFromStart` and
footage openers are composed in frame zero. Openers leave with a 24px lift as
the existing blur/exposure treatment resolves; cards hold until the cut.

Generated inter-chapter cards display **source chapter number minus one**:
source chapter 2 produces card 01. Source IDs, media paths, titles and vertical
opener numbers keep their original numbering.

When adding an asset, update `library.json`. Assets used by generated chapter
renders must also be copied by `src/edit/compose.rs`; required font files must
be included in `src/preflight.rs`. A template looking correct in isolation
does not prove the generated render workspace contains its dependencies.
