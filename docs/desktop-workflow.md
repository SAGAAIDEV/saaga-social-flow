# Desktop content workflow

The recorder has four primary steps, declared in `src/ui/workflow.rs`:

1. **Video recording** — record the session, then press **Render video and
   thumbnails**. That one press does the whole run, in this order:
   1. Takes your photo from the camera (and the screen, when the layout has one).
      A render requires a still: it refuses to start only when no frame can be
      taken and there is none from before to fall back on.
   2. Cuts and renders the longform and the vertical chapters, with progress
      beneath the recording controls.
   3. Writes the title and description from the completed transcript, using the
      model selected under Speaking notes: a title of up to 60 characters and a
      one-sentence description of up to 140. An edit made on the YouTube tab is
      kept rather than rewritten.
   4. Draws the artwork set — horizontal, portrait and OG — from the photo and
      that copy. Skipped when the set on disk already matches both.
   5. Uploads the longform to YouTube with the horizontal artwork, at the
      visibility chosen on the YouTube tab — but only for a project that has
      never been uploaded. A re-render of a video already on YouTube stops here
      and says so; publishing it again as a new video is the YouTube tab's
      button, so tightening a cut never mints a duplicate on the channel. When
      the project has a vertical cut it follows as a Short, and the status line
      and the Video pane's **On YouTube** card list both links — the longform's
      and the Short's — rather than the last one to land. The YouTube tab lists the pair with a copy button on each and one for both.

   The chain stops at the first failure and the status line under the button
   says which step. Everything the press produces lands in the **Video details**
   pane on the right, top to bottom in the order it is produced: the notes that
   steer the copy and the copy itself, the artwork set with the photo it was
   drawn from, and the rendered clips to scrub before anything else goes out.
   **Retake photo**, **Retake screen** and **Redraw artwork** are the
   corrections — the screen on its own, so a good photo survives a slide
   change; the design
   controls (kicker, theme, focus) and the optional AI image experiments are
   folded away beneath them. Between the artwork and the clips, a **Figures**
   card lists every figure snipped during the take with what was said over it,
   transcribed, and the blurb written from that — the same figures the Blog tab
   places into the article. A pipeline strip at the top of the pane shows which
   of the five stages are done. Speaking notes keep their own panel beside it.
   There is no Thumbnails tab and no Review sub-tab any more.
2. **YouTube** — edit and save the title and description, choose visibility,
   connect the channel, and upload (or re-upload) the longform by hand.
3. **Blog (Strapi)** — write and review the companion article, then publish it live at
   `/blog/{slug}`; the ledger row records whether Strapi actually published it.
   Deliberately not part of the render's chain: the blog carries the portrait
   poster and the OG image, and this is where they get checked first. When the
   blog is blocked on artwork the reason is the specific one — a photo or design
   changed since the set was drawn — rather than a generic "generate artwork".
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
4. **Socials** — generate and edit platform copy; upload video media to create public
   asset URLs; build the Buffer plan, review/approve it, and queue it. Analytics and
   Reflect are secondary tabs here.

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
The two Focus boxes on the Video details pane nudge it; the vertical one only moves
anything when the still is taller than its box.

The current set is recorded in `thumbnails/artwork.json`, with file hashes and measured
JPEG dimensions. A failed generation leaves the previous complete set active. Editing
the saved design or changing the photo makes the set stale; the next render redraws it,
and **Redraw artwork** on Video details does so by hand. The blog and YouTube workflows
require a current set and name the reason when it is not; image upload failures are
surfaced.
YouTube can retry/update the thumbnail on an existing upload without duplicating the video.

Upload media exports `thumbnail`, `thumbnail-vertical`, and `og-image` as public assets.
A Buffer plan uses the OG asset as an image post for longform LinkedIn/Facebook copy;
vertical clips remain video posts. Changing the exported image changes the approval
identity. Upload jobs freeze their images outside the render cache, so regenerating
artwork during an upload cannot replace or delete that job's inputs.

Buffer rejects `video.thumbnailUrl`; it cannot attach these JPEGs as custom covers
on social video posts. Supported alternatives are image posts and blog links whose
preview reads `ogImage`. See the [Buffer asset reference](https://developers.buffer.com/reference.html).
