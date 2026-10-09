# Desktop content workflow

The recorder's workflow steps are declared in `src/ui/workflow.rs`. From
recording on:

1. **Video recording** — record the session, then press **Render video**. It
   cuts and renders. When it finishes, the S3 upload starts by itself, and so
   does the social copy if this version has none yet (copy already written is
   never replaced) — so the Buffer rows are there to tick. The title and
   description, the thumbnail and the YouTube upload are their own steps.
   1. (The photo is no longer taken here — see **Thumbnail**.)
   2. Cuts and renders the longform and the vertical chapters, with progress
      beneath the recording controls. The longform opens on the video's own
      title, and every chapter after the first gets a card carrying the
      chapter's own number — the same one the vertical cut, the notes and the
      blog use, so the first card a viewer meets reads "Chapter 02". Three
      boxes directly above **Render video** — **Horizontal**,
      **Vertical**, **Shorts** — decide what is produced; all three are on by
      default and remembered. A render draws only what the ticked outputs still
      lack, so ticking one after a render costs that output alone. The vertical
      longform is the shorts joined, so it renders the chapters either way; an
      unticked output's earlier files are left where they are — and the uploads
      honour the boxes rather than the files: YouTube skips the Short when the
      vertical longform is off, Distribute leaves out whatever is off, and the
      blog does not send a vertical file whose box is off. The composites are
      recorded at 0.25 bits per pixel per frame — 15.6 Mbps for a 1080p master,
      about 1.2 GB per orientation for a ten-minute take — and HyperFrames
      renders the cards and the vertical chapters at its `high` quality; both
      were lower, and a soft master is soft at every stage after it. Those
      renders run a few at a time — as many as the memory the machine can
      spare when the render starts will hold, at roughly 6 GB each and never
      more than the cores allow — and the pool shrinks if the room runs out
      part way. A render refuses to start, saying what to close, when even one
      would not fit; `SCREENCAST_RENDER_WORKERS=N` pins the count for an
      operator who knows better.
   3. **Write title & description** on the Video details pane writes them from
      the completed transcript, using the model selected under Speaking notes:
      a title of up to 60 characters and a one-sentence description of up to
      140. It is a button, not part of the render.
   4. The thumbnail is made on the **Thumbnail** tab, and the YouTube upload
      is the YouTube tab's **Upload** — neither runs off the render.
   5. The S3 upload for Buffer is the Socials tab's button, beside the plan.
      It needs `S3_BUCKET` and a live `aws sso login`.

   The status line under the button says how the render went. A render that loses some of its clips — "5 of 21 render(s)
   failed" — names each one and the reason HyperFrames gave, and leaves that
   reason in a `.log` beside where the clip would have landed under `render/`.
   Pressing Render video again is the way back: every stage is incremental, so
   it draws only the clips that are missing or stale and joins the longforms.
   **Re-render missing**, directly under it, now does exactly the same.
   Everything the press produces lands in the
   **Video details** pane on the right, top to bottom in the order it is
   produced: the notes that steer the copy and the copy itself, a **Figures**
   card listing every figure snipped during the take with what was said over
   it, transcribed, and the blurb written from that — the same figures the Blog
   tab places into the article — and the rendered clips to scrub before
   anything goes out. A pipeline strip at the top of the pane shows which
   stages are done, the thumbnail's review included. Speaking notes keep their
   own panel beside it. There is no Review sub-tab any more.
   The input meter sits directly under the Microphone selector, the full width
   of the column, so that sound is going into the take is visible at a glance —
   a mic that has picked nothing up is a bar that has not moved, right where
   the mic was chosen. It draws the peak level with a meter's ballistics —
   up at once, down at 24 dB/s — green where speech should peak, yellow in the
   last of the headroom, red at the top, and all red once anything has clipped.
   It listens whenever the mic is open, recording or not.
   The **Layout** dropdown offers three pairs. **Talking Head** records the
   camera full-frame; **Split** records the screen beside it; **Outline**
   records exactly what Talking Head records and becomes a different layout at
   render: the chapter opens on you full-frame, and just before you reach your
   first talking point a card slides in from the left (from the top, in the
   vertical) with the chapter heading, pushing you over into the split's
   column (its bottom band). The points list themselves one at a time on the
   beat you say them, and before the cut the card slides away and the chapter
   ends as it began. The points are written from the chapter's transcript by
   the model chosen under Speaking notes, one line each: the model quotes the
   words each point begins on, and the recorder matches the quote back to the
   transcript's word timings and through the disfluency cut to place it. They
   land in `outline/vN/outline.json`, where text and anchors can be edited by
   hand and survive a re-render; the placed times are rewritten on every
   render from the current cut, and `face_y` — where the face tracker last saw
   you in the vertical take, which the vertical band centres on — can be
   corrected there too. An outline chapter is the one kind whose horizontal
   body is drawn by HyperFrames rather than spliced in as cut, so it costs
   what a vertical chapter of the same length costs to render. A chapter
   recorded as outline whose transcript never landed, or with no points, stays
   a talking head; one whose points cannot be written — no OpenRouter key —
   stops the render and says so.
   A chapter menu sits directly under **Start Recording**. Idle, it offers the
   next fresh chapter and every chapter already recorded; picking a recorded
   one turns the button into **Retake Chapter NN**, and pressing it moves that
   chapter's whole earlier take — camera, screen, composed outputs, audio,
   transcript, sidecars and any hand edit — into the take folder's
   `.discarded/` before recording the same number afresh, so a single chapter
   can be redone after the rest are in the can. Nothing moves until the press.
   **Stop** closes the retake and the menu returns to the next fresh chapter;
   **New Chapter** out of a retake opens the first unrecorded number rather
   than the chapter after it, so it never lands on one already recorded. The
   **Retake ⌃⌥T** button is unchanged: it redoes the chapter that is rolling.
2. **Thumbnail** — take the photo, draw the artwork, review it, and approve it. The
   three pictures at a size that shows them, the photo and screen grab they
   were drawn from, and the corrections: **Retake photo** (the camera keeps
   running on every tab, so the countdown works from here — the new still
   appears when it ends), **Retake screen** (the slide as the layout frames
   it, or — when the layout has no screen, Talking Head say — the whole
   display with this app's windows left out), **Choose photo…** and **Redraw
   artwork**, with the design controls (kicker, theme, focus) and the optional
   AI image experiments folded away beneath them. The artwork's title is the
   one **Write** puts on it; until then an empty title draws the video's — the
   YouTube title, else the project's name, never the folder's timestamp — and
   the first draw saves it to the design. **Approve thumbnail** only
   records the approval; it uploads nothing. The YouTube upload and Replace
   thumbnail, the blog and the S3 copy for Buffer all read only an approved
   set. The approval is recorded in `thumbnails/approval.json` against the
   set's id, so a redraw, a new photo or a design edit needs approving again;
   nothing has to clear it. A project that was already on YouTube before this
   step existed counts as approved until it is approved once.
3. **YouTube** — edit and save the title and description, choose visibility,
   connect the channel, and upload (or re-upload) the longform by hand. The
   **Visibility** picker is both the setting the next upload goes up with and
   a control on the video already up: moving it makes the longform, and then
   the Short, public, unlisted or private on YouTube straight away, and the
   ledger records the change with its time. Changing a live video needs the
   "Manage your YouTube account" permission, which connections made before
   this asked for do not carry — the first change says so and asks for one
   more press of **Connect…**. While an upload is running the change is held
   and applied by the next Upload press.
4. **Blog (Strapi)** — write and review the companion article, then publish it live at
   `/blog/{slug}`; the ledger row records whether Strapi actually published it.
   Deliberately not part of the render's chain: the blog carries the portrait
   poster and the OG image, and this is where they get checked first. When the
   blog is blocked on artwork the reason is the specific one — not drawn, a
   photo or design changed since the set was drawn, or not approved yet —
   rather than a generic "generate artwork".
   Before anything is uploaded, the post is measured against the CMS's field
   limits — a Strapi `string` is a 255-character column in Postgres whether the
   schema says so or not, and a pull quote past it used to come back as a bare
   500 after the three images were already in the media library — and the
   picked author and category are read back from the CMS the publish goes to,
   so an id from a list read off a local Strapi cannot land on production under
   someone else's name. The Blog tab says where its lists came from when that
   is not the configured CMS; Refresh re-reads them. A fresh draft is checked
   against the same limits as it is written and the model is asked to shorten
   what is over; a draft already on disk that is over shows a **Needs fixing**
   card on the Blog tab, each field editable in place with a live count, with
   **Save fixes** and **Shorten with the model** as the two ways out. Publish is
   off until the card is empty. The long description is held to one sentence of
   200 characters the same way: not a CMS limit, but it prints under the heading.
   The existing CMS preview and publishing controls remain here.
5. **Socials** — generate and edit platform copy. The Buffer rows build
   themselves from the copy and the public URLs the S3 upload left behind, each
   video going to the channels its shape goes to; tick a row and it posts ten
   seconds later (see `docs/schedule-loop.md`). The Buffer pane's S3 line checks
   every rendered video against that record — on S3 as it is now, rendered again
   since the upload, or never uploaded — and the rows wait until all of them are
   up. **Upload to S3** beside it runs the upload by hand for the misses (the
   render starts it by itself); an object already at its key is not sent again,
   so a needless press is cheap. Analytics and Reflect are secondary tabs here.

Edit and Substack are absent from the navigation. Existing recording data, saved
edits, and backend modules are retained. The order guides the work without requiring
an external publication just to prepare a local social draft.

Working notes and draft copy persist across recording versions in `video-brief.json`.
Generation automatically updates thumbnail text and YouTube metadata. Valid title and
description edits stay in sync; an incomplete title remains saved locally while the
last valid publishing copy is retained.

YouTube copy is saved independently in the project's `youtube-metadata.json`.
Older projects can start from existing longform YouTube copy in `posts.json`; new
projects start with their project title and an empty description. Social posts are
no longer a prerequisite for YouTube upload.

Social generation includes the companion article's title/description and recorded
public YouTube/blog URLs. Private/unlisted YouTube URLs and draft CMS links are not
provided as promotional links. Blog publication status comes from the local blog
ledger; a publication performed separately in Strapi is not automatically detected.
Review generated links before queueing. The full YouTube video is uploaded directly;
Buffer handles social posts and vertical clips.

## Buffer integration

Reference implementation found in the sibling SAAGA checkout:

`../../saaga/solve/src/lib/tools/services/buffer/`

Relevant files:

- `client/buffer-graphql.ts`: GraphQL transport and HTTP-200 error-union handling.
- `client/get-buffer-credentials.ts`: SAAGA's connector-bound OAuth/API-key lookup.
- `create-post/definition.ts`: post creation, video assets, service-specific metadata,
  link attachments and threads.

The recorder uses its existing Rust integration in `src/schedule/buffer.rs`, now
aligned with SAAGA's `https://api.buffer.com` endpoint and typed error handling.
The endpoint is also documented in the [Buffer quick start](https://developers.buffer.com/guides/getting-started.html).

Recorder credentials remain `BUFFER_API_KEY` and optional `BUFFER_ORG_ID` in its
existing environment configuration. SAAGA's encrypted Convex connector credentials
are not copied into the desktop app. Scheduling still uses the saved local plan
and its approval state; this UI cleanup does not send or delete any remote posts.

Validation covers stage ordering, independent YouTube metadata, social publication
context, UI templates/events, and Buffer error handling. Live upload and scheduling
are separate user actions.

## Artwork handoff

The procedural renderer is local HTML/TSX rasterized by WebKit. It uses the saved
photo, title, SAAGA palette and typography. No image-model call is needed. Nano Banana,
Gemini Pro Image and GPT-5 Image remain available under optional AI experiments;
those experiments do not replace the artwork used for publishing.

| Artwork | Destination |
|---|---|
| 1280×720 horizontal | YouTube thumbnail; Strapi `thumbnail` |
| 720×1280 portrait | Strapi `thumbnailVertical`; downloadable/social export. Words on top, photo underneath — the top of a portrait player is where the crop and the controls land. |
| 1200×630 OG | Strapi `ogImage`; LinkedIn and Facebook image posts |

When the photo is taken, the card's focus point is set from the face tracker's
anchor, so the photo box is cropped around the face rather than the frame's centre.
The two Focus boxes on the Thumbnail tab nudge it; the vertical one only moves
anything when the still is taller than its box.

The current set is recorded in `thumbnails/artwork.json`, with file hashes and measured
JPEG dimensions. A failed generation leaves the previous complete set active. Editing
the saved design or changing the photo makes the set stale, and **Redraw artwork** on the
Thumbnail tab draws it again — nothing redraws it on its own. The blog, YouTube and S3
workflows require a current, approved set and name the reason when it is not; image
upload failures are surfaced.
YouTube can retry/update the thumbnail on an existing upload without duplicating the video:
**Upload to YouTube** re-sets it while the render is byte-identical to what went up, and
**Replace thumbnail** beside it pushes the approved artwork from the Thumbnail tab onto
the newest longform already on YouTube whatever the render is now, then the selected vertical
artwork onto the Short if there is one. Each replacement is a new row in `youtube.jsonl` with
`thumbnail_at`, so the tab can say when the poster last changed.

The S3 upload (Socials tab) exports `thumbnail`, `thumbnail-vertical`, and `og-image` as public assets.
A Buffer plan uses the OG asset as an image post for the longform's Facebook copy. The
longform goes to LinkedIn as the video itself, one plan row per connected LinkedIn channel
(the SAAGA Solve page and the personal profile), each approved, queued and deduped on its
own. Buffer caps a LinkedIn video upload at 1 GB and transcodes it to 720p. Vertical clips
remain video posts. Changing the exported image changes the approval
identity. Upload jobs freeze their images outside the render cache, so regenerating
artwork during an upload cannot replace or delete that job's inputs.

Buffer rejects `video.thumbnailUrl`; it cannot attach these JPEGs as custom covers
on social video posts. Supported alternatives are image posts and blog links whose
preview reads `ogImage`. See the [Buffer asset reference](https://developers.buffer.com/reference.html).
