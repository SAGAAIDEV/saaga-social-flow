# Thumbnail: a step of its own, between recording and YouTube

Built on the `thumbnail-tab` branch as planned below. Where it differs: the
Approve button is labelled **Approve thumbnail**, and the styles shared by the
recording page and the Thumbnail tab moved into `ui/templates/studio.html`.

## Context

Thumbnails had their own tab until `be9fc50` (2026-09-08). That commit folded
them into the recording page because "the render button now draws the artwork
set". Today one press of **Render video and thumbnails** runs this chain
(`src/app/mod.rs`, the `pipeline` flag):

1. `run_render` takes the photo (`take_still`, which also grabs the screen
   and aims the crop at the face), then `launch_render`.
2. The render lands, then the title and description are written
   (`app::video_brief`).
3. `continue_pipeline_after_copy` either finds `card::assets::ready` already
   true or calls `draw_card`.
4. When the card commits (`app/card.rs`, `committed`), the chain calls
   `finish_pipeline_with_upload`, which runs `youtube_after_render` and
   `host_after_render`. The first YouTube upload and the S3 upload for Buffer
   both start on their own.

Nobody looks at the artwork before it goes public. The Artwork card on the
recording page (`src/ui/templates/video.html`, section 2) only shows what has
already been uploaded. The only check is a hash: `card::assets::current`
confirms the set matches the photo and design, not that anyone has seen it.

The artwork set (`thumbnails/artwork.json`, `thumbnails/sets/<id>/`) is used
in four places:

- the YouTube upload and Replace thumbnail, through `stage::Stages::publish`
  and `stage::Stages::thumbnail` (`has_thumbnail`);
- the blog (`blog/mod.rs`, `assets::ready`, then `snapshot`);
- S3 hosting (`distribute/mod.rs`, `assets::ready`);
- the re-render shortcut in `continue_pipeline_after_copy`.

## Decisions

1. **The workflow gets a seventh step.** `workflow::STEPS` becomes
   `project, plan, video, thumbnail, youtube, blog, socials`. The pane key is
   `thumbnail`, with the label "Thumbnail". The window still opens on `draft`.

2. **Render drafts, then stops.** The photo is still taken when Render is
   pressed, because the camera preview is on the recording tab. The copy and
   the first artwork set are still drawn automatically. When the card commits,
   the chain no longer uploads. It stops with the line "Thumbnail drafted:
   review and approve it on the Thumbnail tab."
   - Re-render missing leaves the photo alone. If the copy is unchanged and the
     set on disk is already approved, the approval still holds and the chain
     goes straight to the uploads, as it does today. If the set is ready but
     not approved, it stops for review.

3. **Approval is a record tied to one artwork set.** A new file,
   `thumbnails/approval.json`, holds `{ set_id, card_hash, source_hash,
   approved_at }`. A new function, `card::assets::approved(root) -> Result<Set>`,
   is `ready()` plus a check that `approval.set_id == set.id`. Any redraw mints
   a new set id, and a new photo or design change already fails `current()`,
   so the approval lapses on its own. Nothing has to clear it. It lives in a
   separate file so that `Job::commit` stays as it is.

4. **Approve starts the uploads.** The Thumbnail tab's Approve button writes
   the approval and then runs what `finish_pipeline_with_upload` runs today.
   That function is split so the upload half (`start_uploads`) is callable
   without the `pipeline` flag. Its existing refusals carry over unchanged:
   already on YouTube, not connected, or the gate is shut. Approving a redrawn
   set for a video that is already live does not upload a second copy. The
   status line points to **Replace thumbnail** on the YouTube tab instead.

5. **The gates read approval instead of readiness.**
   - In `stage.rs`, `has_thumbnail` uses `assets::approved`. The reason
     strings point to the new tab: "Artwork not drawn — press Render" and
     "Thumbnail not approved — review it on the Thumbnail tab".
   - The `publish`, `thumbnail` (Replace) and `blog` gates all pick this up
     through `has_thumbnail`. The `distribute` gate gains the same condition.
   - `blog/mod.rs` and `distribute/mod.rs` call `approved` where they call
     `ready` today, so a stale or unreviewed picture can never be published by
     any route.

6. **What goes on the Thumbnail tab.** It gets a new `thumbnail.html`
   template, rendered by a new `update_thumbnail_view` that takes the `art`
   half of `update_video_view`:
   - **Header and state:** Not drawn, Drafted (needs review), Approved (with
     time), or Stale. Stale shows the `artwork_notice` text.
   - **The title the artwork is drawn from,** read-only, with a pointer to the
     YouTube tab for edits, same as the recording page has now.
   - **The set:** landscape, portrait and link preview at a usable size. This
     is the reason the tab exists.
   - **Approve:** the primary button. It is disabled while drawing, while
     stale, while nothing is drawn, or when the set is already approved.
   - **Photo:** the still and the screen grab, plus Retake photo, Retake
     screen and Choose photo…, moved as they are. `take_still` reads
     `live.preview.latest_camera_frame()`, which keeps running on any tab, so
     the countdown works from here. You just don't see the preview, so the new
     still appears when the countdown finishes.
   - **Redraw artwork, the Design accordion and AI image experiments,** moved
     as they are, along with their JavaScript (forms, drop zone, countdown,
     portrait file).

7. **The recording page gets shorter.** The Artwork card comes off
   `video.html`. The pipeline strip keeps its Artwork dot and adds the
   approved state: done means approved, and "review" is a new middle state.
   The strip also gets one line saying where to approve. The thumbnail
   progress bar and status line under the Render button stay, because they
   narrate the draft.

8. **Existing projects.** A project already on YouTube
   (`publish::longform` or `short` is some) with no `approval.json` counts as
   approved for its current set. Its picture has already gone public, and
   without this rule its blog gate would shut on the first launch after the
   change. Every other project has to be approved once.

## Implementation steps

1. `card/assets.rs`: add the `Approval` struct, `approve(root, &Set)`,
   `approved(root)`, and the backfill rule from decision 8. Unit tests:
   approval lapses on redraw, on a photo change and on a design change; the
   backfill applies only when there is a YouTube upload.
2. `stage.rs`: switch `has_thumbnail` to `approved`, add the condition to
   `distribute`, and reword the reasons. Update the gate tests.
3. `blog/mod.rs`, `distribute/mod.rs`: change `ready` to `approved`.
4. `app/mod.rs` and `app/card.rs`: split `finish_pipeline_with_upload` into
   "stop for review" and `start_uploads`. On commit, stop unless the new set
   is already approved. Make the same check in `continue_pipeline_after_copy`.
5. `ui/mod.rs`: add `Action::ApproveThumbnail`, a `thumbnail` pane
   (`WebPane`, attached the way `plan_pane` is), and register it in the
   `panes` list given to `workflow::attach`.
6. `ui/templates/thumbnail.html`: build it from section 2 of `video.html`
   plus the Approve header. Strip that section and its JavaScript out of
   `video.html`.
7. `app/mod.rs`: add `update_thumbnail_view`, and call it everywhere
   `update_video_view` runs after an artwork, still or thumbnail event (card
   raster events, `ThumbnailEvent`, `take_still`, `capture_screen`, project
   switch).
8. `ui/workflow.rs`: seven steps; the test asserts the order
   `draft < thumbnail < youtube` and drops the `!contains("thumbnails")`
   line. Rewrite the module comment that explains why the tab was removed.
9. Docs: the `desktop-workflow.md` steps, plus the `app/mod.rs` header
   comment.

## Verification

- `cargo test`: the asset approval tests, gate tests, workflow test and
  template render.
- Visual check by you. I won't launch the recorder myself, because it takes
  the camera and mic. On a project with one recording:
  1. Press Render. The chain should stop at "Thumbnail drafted", with nothing
     on YouTube and nothing on S3.
  2. On the Thumbnail tab, press Redraw, then Approve. The YouTube and S3
     uploads should start.
  3. Retake the photo. The tab should read Stale and Approve should be off;
     the blog gate should name the Thumbnail tab.
  4. Re-render missing with nothing changed. The approval should hold and no
     review should be asked for.

## Open questions

- Should the window switch to the Thumbnail tab when the draft lands? The
  default is no, because the status line says where to go and switching tabs
  mid-task is jarring.
- Should the AI image experiments get their own Approve? Today their
  candidates never feed publishing. The default is to keep it that way.
