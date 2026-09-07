# Desktop content workflow

The recorder has five primary steps, declared in `src/ui/workflow.rs`:

1. **Video recording** — record the session, render the video, and review the result.
   Record and Review are sub-tabs of this step. **Render video** on Record runs
   the cut-and-render pipeline, with progress beneath the recording controls, and
   when it finishes it **writes the title and description** from the completed
   transcript using the model selected under Speaking notes: a title of up to 60
   characters and a one-sentence description of up to 140. There is no separate
   Render tab and no generate button. On Record, **Video details** holds one small
   input — notes to steer that copy (key idea, audience, takeaway), remembered per
   project — and shows what the last render wrote. Notes only guide the emphasis
   and never replace the transcript. The written copy is shared with thumbnails
   and YouTube automatically; generate fresh artwork after it changes. Edit the
   copy on the YouTube tab if the render's words are wrong — a later render keeps
   an edit made there rather than rewriting it. Existing speaking-note controls
   remain in the adjacent Speaking notes panel.
2. **Thumbnails** — capture or import your photo, enter the title, then press
   **Generate artwork set**. This creates horizontal, portrait and OG images together.
3. **YouTube** — edit and save the title and description, choose visibility,
   connect the channel, and upload the longform with the selected thumbnail.
4. **Blog (Strapi)** — write and review the companion article, then send it to Strapi.
   The existing CMS preview and publishing controls remain here.
5. **Socials** — generate and edit platform copy; upload video media to create public
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
| 720×1280 portrait | Strapi `thumbnailVertical`; downloadable/social export |
| 1200×630 OG | Strapi `ogImage`; LinkedIn and Facebook image posts |

The current set is recorded in `thumbnails/artwork.json`, with file hashes and measured
JPEG dimensions. A failed generation leaves the previous complete set active. Editing
the saved design or changing the photo requires regeneration before publishing. The
blog and YouTube workflows require the complete set; image upload failures are surfaced.
YouTube can retry/update the thumbnail on an existing upload without duplicating the video.

Upload media exports `thumbnail`, `thumbnail-vertical`, and `og-image` as public assets.
A Buffer plan uses the OG asset as an image post for longform LinkedIn/Facebook copy;
vertical clips remain video posts. Changing the exported image changes the approval
identity. Upload jobs freeze their images outside the render cache, so regenerating
artwork during an upload cannot replace or delete that job's inputs.

Buffer rejects `video.thumbnailUrl`; it cannot attach these JPEGs as custom covers
on social video posts. Supported alternatives are image posts and blog links whose
preview reads `ogImage`. See the [Buffer asset reference](https://developers.buffer.com/reference.html).
