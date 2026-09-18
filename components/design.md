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
| ink | #1D1D1D | Dark chapter canvas; text on light panels |
| paper | #F6F6F6 | Light panels; text on dark footage |
| muted | #626262 | Supporting text on light panels |
| orange | #EB5201 | Brand rules, camera accents and marks |
| peach | #FFEEE5 | Camera fallback surfaces |
| line | #D1D1CC | Fine panel borders |
| orange-on-dark | #FF9561 | Readable accent text over darkened footage |
| radius | 24px | Rectangular camera card corners |

Chapter cards use a dark canvas and a compact inline chapter label/number above
the title. The topic is the focal point (112px landscape, 88px portrait); an
orange rule and upright SAAGA mark form a quiet signature below it. All four
chapter variants share the same hierarchy, with the existing shader treatment
under footage openers. Titles wrap without clipping, including long single words.
The shared badge SVG is static; the split layout's existing inline badge
animation remains controlled by its seekable timeline.

Keep the full-bleed screen and camera geometry, framing variables, audio tracks
and chapter durations. Do not add identity strips
back to the vertical or talking-head layouts. Speaker strips use an off-white
surface, dark medium-weight type and an orange top rule.

Chapter CSS and motion have one editable source under `components/chapter/`.
Run `node scripts/sync-chapters.mjs` after edits; run the same command with
`--check` to verify that every template contains the current shared code.
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
