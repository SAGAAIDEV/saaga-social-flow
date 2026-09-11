//! Build HyperFrames workspaces from cut chapter files.

use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};

use super::cut;

pub const CARD_SECONDS: f64 = 3.0;
/// Where the workspace records the encoder quality its renders were made at.
///
/// Declared as a source of every job, so changing [`super::render::QUALITY`]
/// re-renders everything exactly once. Without it a quality change passed
/// every freshness check — the compositions had not changed — and the shorts
/// on disk stayed at the old setting for as long as the project lived.
pub const QUALITY_FILE: &str = "render-quality.txt";
pub const OPENER_SECONDS: f64 = 2.6;
const HF_VERSION: &str = "0.7.107";

const HYPERFRAMES_JSON: &str = r#"{
  "$schema": "https://hyperframes.heygen.com/schema/hyperframes.json",
  "registry": "https://raw.githubusercontent.com/heygen-com/hyperframes/main/registry",
  "paths": {
    "blocks": "compositions",
    "components": "compositions/components",
    "assets": "assets"
  },
  "media": {
    "autoProxy": true
  }
}
"#;

/// What a rendered composition is, which decides the resolution it renders at.
///
/// There is no `Body`: a chapter body is passed through rather than rendered — see
/// [`Segment`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Card,
    Vertical,
}

#[derive(Debug, Clone)]
pub struct Job {
    pub id: String,
    pub kind: Kind,
    pub composition: String,
    /// Every file this job renders *from*, absolute. The renderer skips a job whose
    /// output is newer than all of these, so a rerun after one edited chapter does
    /// not re-render the other twenty. Declared here because only the composer knows
    /// what it wired up; an empty list means "cannot prove freshness, always render".
    pub sources: Vec<PathBuf>,
}

/// One piece of the longform, in playing order.
///
/// The distinction is what a browser is actually *for*. A title card is drawn — fonts,
/// a fading pattern, text that has to be laid out — so it has to be rendered. A chapter
/// body is the cut take at full frame with nothing over it; putting that through a
/// headless browser re-decodes and re-encodes finished footage to produce a copy of
/// itself, and a worse one (HyperFrames writes Constrained Baseline, where the cut is
/// High) built from a 64kbit mp3 rather than the cut's own 192kbit audio.
///
/// So a body is named, not rendered.
#[derive(Debug, Clone)]
pub enum Segment {
    /// A composition a browser has to draw.
    Render(Job),
    /// A finished file that goes into the longform as it is.
    Passthrough(PathBuf),
}

impl Segment {
    pub fn job(&self) -> Option<&Job> {
        match self {
            Segment::Render(job) => Some(job),
            Segment::Passthrough(_) => None,
        }
    }
}

#[derive(Debug, Clone)]
pub struct Plan {
    pub horizontal: PathBuf,
    pub vertical: PathBuf,
    /// The longform, in order: card, body, card, body… Only the cards are rendered.
    pub h_segments: Vec<Segment>,
    pub v_jobs: Vec<Job>,
    /// What this plan was asked for. `h_segments` is empty when the horizontal
    /// longform is off and `v_jobs` when neither vertical output is; the
    /// renderer reads `vertical` here to know whether to join the chapters.
    pub targets: crate::config::RenderTargets,
}

impl Plan {
    /// Everything a browser has to draw for this plan.
    pub fn render_count(&self) -> usize {
        self.h_segments.iter().filter(|s| s.job().is_some()).count() + self.v_jobs.len()
    }

    pub fn is_empty(&self) -> bool {
        self.h_segments.is_empty() && self.v_jobs.is_empty()
    }
}

/// The HyperFrames library this crate renders from.
///
/// Vendored into `components/` rather than reached for in a sibling checkout.
/// It used to resolve to `../screencast/components`, which lives inside a
/// *private, personal* repo — so a new team member cloning this one got a
/// working build and a render that died on a path they had no way to populate.
/// The whole library is under 1.5 MB, which is a cheaper thing to carry than an
/// onboarding step nobody outside one account can complete.
///
/// `CARGO_MANIFEST_DIR` for a checkout, the executable's own directory for an
/// installed release, and the historical sibling path last so a machine still
/// laid out the old way keeps working.
pub fn components_root() -> PathBuf {
    let vendored = Path::new(env!("CARGO_MANIFEST_DIR")).join("components");
    if vendored
        .join("compositions/chapter-title-card.html")
        .is_file()
    {
        return vendored;
    }
    if let Some(beside_exe) = std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(|dir| dir.join("components")))
    {
        if beside_exe
            .join("compositions/chapter-title-card.html")
            .is_file()
        {
            return beside_exe;
        }
    }
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../screencast/components")
}

/// Everything on: the plan a render made before there were boxes to untick.
/// Kept for the tests, which are about the layout rather than the boxes.
#[cfg(test)]
pub fn prepare(
    edit_root: &Path,
    compose_root: &Path,
    library: &Path,
    titles: &[(u32, String)],
    project_title: &str,
) -> Result<Plan> {
    prepare_targets(
        edit_root,
        compose_root,
        library,
        titles,
        project_title,
        crate::config::RenderTargets::default(),
    )
}

/// The plan for what `targets` asks for.
///
/// An output that is off contributes nothing to the plan — no cards, no
/// chapter compositions — so nothing is drawn for it. What it does not do is
/// remove an earlier render's files: a box is "do not spend time on this", not
/// "throw it away", and the pane lists what is on disk either way.
pub fn prepare_targets(
    edit_root: &Path,
    compose_root: &Path,
    library: &Path,
    titles: &[(u32, String)],
    project_title: &str,
    targets: crate::config::RenderTargets,
) -> Result<Plan> {
    if !library
        .join("compositions/chapter-title-card.html")
        .is_file()
    {
        bail!("HyperFrames library missing at {}", library.display());
    }
    let horizontal = compose_root.join("horizontal");
    let vertical = compose_root.join("vertical");
    write_workspace(&horizontal, 1920, 1080)?;
    write_workspace(&vertical, 1080, 1920)?;
    copy_library(
        library,
        &horizontal,
        &[
            "compositions/chapter-title-card.html",
            "assets/pattern-rings.svg",
            // The mark in the middle of the card's separator. Missing, the card
            // renders a broken image and says nothing about it.
            "assets/badge.svg",
            "assets/silence.mp3",
            "assets/fonts/Booton-Regular.woff2",
            "assets/fonts/Booton-Semibold.woff2",
            "assets/fonts/Booton-Bold.woff2",
        ],
    )?;
    copy_library(
        library,
        &vertical,
        &[
            "compositions/talking-head-vertical.html",
            "assets/fonts/Booton-Regular.woff2",
            "assets/fonts/Booton-Semibold.woff2",
            "assets/fonts/Booton-Bold.woff2",
        ],
    )?;

    let mut h_segments: Vec<Segment> = Vec::new();
    let mut v_jobs = Vec::new();
    for (n, title) in titles {
        let chapter = edit_root.join(format!("chapter-{n:02}"));
        let audio = chapter.join("audio.mp3");
        let h_src = chapter.join(format!("chapter-{n:02}-horizontal.mp4"));
        let v_src = chapter.join(format!("chapter-{n:02}-vertical.mp4"));
        if targets.horizontal && h_src.exists() {
            // The opening title card stands in for chapter one's, so chapter one
            // plays straight out of the title with nothing between them — one
            // card at the front rather than six seconds of two.
            //
            // Every card carries the chapter's own number. The first card a
            // viewer meets therefore reads "Chapter 02", and that is right:
            // chapter one opened under the title, the way a book's first chapter
            // opens under its own. The cards used to count what had been shown
            // instead — "01" in front of chapter two — and that number agreed
            // with nothing else: the vertical cut of the same chapter said
            // "Chapter 02", the notes and the blog said chapter 2, and a chapter
            // with no title fell back to its own number, so one card read
            // "02 / Chapter 3".
            if !h_segments.is_empty() {
                h_segments.push(Segment::Render(write_card(&horizontal, *n, title)?));
            }
            // The cut itself, straight into the longform. Nothing is copied into the
            // horizontal workspace for it either — the cards do not reference chapter
            // media, and this take's footage is hundreds of megabytes.
            h_segments.push(Segment::Passthrough(h_src));
        }
        if targets.vertical_parts() && v_src.exists() {
            let seconds = cut::probe_duration_seconds(&v_src)?;
            copy_media(&vertical, *n, &v_src, audio.exists().then_some(&audio))?;
            v_jobs.push(write_v_chapter(&vertical, *n, title, seconds)?);
        }
    }
    // The longform's own title, in front of everything. Added last so it is not
    // mistaken for a chapter above, and whenever there is any body to open —
    // asking for a *rendered* segment here meant a single-chapter video got no
    // title card at all, because chapter one no longer has a card of its own.
    if !h_segments.is_empty() {
        h_segments.insert(
            0,
            Segment::Render(write_opener(&horizontal, project_title)?),
        );
    }

    let first_render = h_segments
        .iter()
        .find_map(Segment::job)
        .or_else(|| v_jobs.first());
    if let Some(first) = first_render {
        let preview_ws = if h_segments.iter().any(|s| s.job().is_some()) {
            &horizontal
        } else {
            &vertical
        };
        write_index(preview_ws, first)?;
    }
    Ok(Plan {
        targets,
        horizontal,
        vertical,
        h_segments,
        v_jobs,
    })
}

fn write_workspace(root: &Path, width: u32, height: u32) -> Result<()> {
    std::fs::create_dir_all(root.join("compositions"))
        .with_context(|| format!("creating {}", root.display()))?;
    std::fs::create_dir_all(root.join("assets/videos"))?;
    std::fs::write(root.join("hyperframes.json"), HYPERFRAMES_JSON)
        .with_context(|| format!("writing {}", root.join("hyperframes.json").display()))?;
    let package = format!(
        "{{\n  \"name\": \"stream-recorder-compose\",\n  \"private\": true,\n  \"type\": \"module\",\n  \"scripts\": {{\n    \"render\": \"npx --yes hyperframes@{HF_VERSION} render\"\n  }}\n}}\n"
    );
    std::fs::write(root.join("package.json"), package)?;
    std::fs::write(root.join("index.html"), blank_index(width, height))?;
    // Only when it differs, like every other render input: rewriting it each
    // run would make every composition look stale every run.
    write_if_changed(&root.join(QUALITY_FILE), super::render::QUALITY)?;
    Ok(())
}

fn copy_library(library: &Path, dest: &Path, rels: &[&str]) -> Result<()> {
    for rel in rels {
        let src = library.join(rel);
        if !src.is_file() {
            continue;
        }
        let out = dest.join(rel);
        if let Some(parent) = out.parent() {
            std::fs::create_dir_all(parent)?;
        }
        // Unconditional copies here would bump the mtime of every library block on
        // every run, which cascades into the baked compositions and defeats the
        // renderer's freshness check.
        copy_if_changed(&src, &out)?;
    }
    Ok(())
}

/// Copies a chapter's media into the workspace, returning the workspace-side paths
/// so the job can name them as render inputs.
fn copy_media(
    workspace: &Path,
    n: u32,
    video: &Path,
    audio: Option<&Path>,
) -> Result<Vec<PathBuf>> {
    let dir = workspace.join(format!("assets/videos/chapter-{n:02}"));
    std::fs::create_dir_all(&dir)?;
    let video_name = video.file_name().context("video name")?;
    let mut copied = Vec::new();
    let video_dest = dir.join(video_name);
    copy_if_changed(video, &video_dest)?;
    copied.push(video_dest);
    if let Some(audio) = audio {
        let audio_dest = dir.join("audio.mp3");
        copy_if_changed(audio, &audio_dest)?;
        copied.push(audio_dest);
    }
    Ok(copied)
}

/// Writes only when the bytes differ, so an unchanged composition keeps its mtime
/// and the renderer can trust "output is newer than input" to mean "still current".
/// Rewriting identical content every run would make every freshness check fail.
///
/// Shared with the cut, which writes `edits.json` under the same constraint: it is an
/// input to the cut beside it, so rewriting it unchanged would invalidate a cut that is
/// perfectly current.
pub(crate) fn write_if_changed(path: &Path, content: &str) -> Result<()> {
    if let Ok(existing) = std::fs::read_to_string(path) {
        if existing == content {
            return Ok(());
        }
    }
    std::fs::write(path, content).with_context(|| format!("writing {}", path.display()))
}

/// The same idea for binaries, compared on size and mtime rather than contents —
/// a chapter render is hundreds of megabytes and not worth hashing.
fn copy_if_changed(src: &Path, dest: &Path) -> Result<()> {
    if let (Ok(from), Ok(to)) = (src.metadata(), dest.metadata()) {
        let same_size = from.len() == to.len();
        let current = match (from.modified(), to.modified()) {
            (Ok(from), Ok(to)) => to >= from,
            _ => false,
        };
        if same_size && current {
            return Ok(());
        }
    }
    std::fs::copy(src, dest)
        .with_context(|| format!("copying {} → {}", src.display(), dest.display()))?;
    Ok(())
}

/// The card in front of chapter `n`, numbered `n` — the same number the
/// vertical cut, the notes and the blog give the chapter. See [`prepare`] for
/// why the first one a viewer meets is "02".
///
/// `title` may be empty: a chapter nobody has titled shows "Chapter 03" from
/// the label and number alone, and the topic slot collapses. A topic reading
/// "Chapter 3" under a number reading "03" said the same thing twice.
fn write_card(workspace: &Path, n: u32, title: &str) -> Result<Job> {
    title_card(
        workspace,
        &format!("seg-{n:02}-card"),
        serde_json::json!({
            "chapterLabel": "Chapter",
            "chapterNumber": format!("{n:02}"),
            "chapterTopic": title,
            "durationSeconds": CARD_SECONDS,
            // Revealed on a beat, like it always was: a chapter card appears
            // mid-video where the animation is the point.
            "holdFromStart": 0,
        }),
    )
}

/// The longform's own opening title, in front of chapter one's card.
///
/// The same component as a chapter card with the two chapter slots left empty:
/// the label is a 48px kicker and the number is 420px, and both collapse in the
/// centred flex column when they carry no text, leaving the project's title on
/// the branded background.
///
/// It exists because the longform had no opening of its own, so the first thing
/// a viewer saw was a chapter card rather than the video's title — see
/// [`prepare`].
fn write_opener(workspace: &Path, project_title: &str) -> Result<Job> {
    title_card(
        workspace,
        "seg-00-opener",
        serde_json::json!({
            "chapterLabel": "",
            "chapterNumber": "",
            "chapterTopic": project_title,
            "durationSeconds": CARD_SECONDS,
            // Composed from frame one. Every element on this card is hidden
            // until its beat, so frame 0 was the background and nothing else —
            // and frame 0 is the frame a social platform grabs for its preview.
            // The title of the video has to be legible in it.
            "holdFromStart": 1,
        }),
    )
}

/// One `chapter-title-card` render, whatever it says.
fn title_card(workspace: &Path, id: &str, values: serde_json::Value) -> Result<Job> {
    let block = "chapter-title-card";
    let library = workspace.join(format!("compositions/{block}.html"));
    let baked = library.parent().unwrap().join(format!("{id}.{block}.html"));
    copy_if_changed(&library, &baked)?;
    let wrapper = write_wrapper(
        workspace,
        id,
        block,
        &format!("{id}.{block}.html"),
        &values,
        CARD_SECONDS,
        1920,
        1080,
    )?;
    Ok(Job {
        id: id.to_string(),
        kind: Kind::Card,
        composition: format!("compositions/{id}.html"),
        // The badge is declared a source as well as the markup: it is drawn on
        // every card, so a new one has to re-render them rather than leaving the
        // old logo on disk looking current.
        sources: vec![
            wrapper,
            baked,
            workspace.join("assets/badge.svg"),
            workspace.join(QUALITY_FILE),
        ],
    })
}

fn write_v_chapter(workspace: &Path, n: u32, title: &str, seconds: f64) -> Result<Job> {
    let id = format!("chapter-{n:02}");
    let block = "talking-head-vertical";
    let camera = format!("assets/videos/chapter-{n:02}/chapter-{n:02}-vertical.mp4");
    let audio = format!("assets/videos/chapter-{n:02}/audio.mp3");
    let values = serde_json::json!({
        "chapterLabel": "Chapter",
        "chapterNumber": format!("{n:02}"),
        "chapterTopic": title,
        "cameraSrc": camera,
        "audioSrc": audio,
        "durationSeconds": (seconds * 1000.0).round() / 1000.0,
        "openerSeconds": OPENER_SECONDS,
        "cameraPosition": "center bottom",
    });
    let library = workspace.join(format!("compositions/{block}.html"));
    let baked_name = format!("{id}.{block}.html");
    let baked = workspace.join("compositions").join(&baked_name);
    let mut body = std::fs::read_to_string(&library)
        .with_context(|| format!("reading {}", library.display()))?;
    body = body.replace("assets/placeholder-camera.mp4", &camera);
    body = body.replace("assets/placeholder-audio.mp3", &audio);
    body = bake_duration(&body, seconds, block)?;
    write_if_changed(&baked, &body)?;
    let wrapper = write_wrapper(
        workspace,
        &id,
        block,
        &baked_name,
        &values,
        seconds,
        1080,
        1920,
    )?;
    Ok(Job {
        id,
        kind: Kind::Vertical,
        composition: format!("compositions/chapter-{n:02}.html"),
        sources: vec![
            wrapper,
            baked,
            workspace.join(&camera),
            workspace.join(&audio),
            workspace.join(QUALITY_FILE),
        ],
    })
}

/// The library block declares its own length, so a longer chapter is left blank.
///
/// Every timed element in `talking-head-vertical` ships as `data-duration="10"`,
/// and the block fixes them at runtime from the `durationSeconds` variable. That
/// fix does not reach whatever the renderer times clips by: all four verticals
/// went flat at 10.0s — a 105-second chapter with 95 seconds of background —
/// while the horizontal ran full length, because the horizontal body wrote the real
/// duration straight into the HTML. (It no longer renders at all — see [`Segment`] —
/// but the lesson is the same and this block still needs it.)
///
/// So do the same here. A block that no longer carries the declared default is
/// an error rather than another silent ten seconds: the render is expensive, the
/// failure is invisible in the log, and the next person to notice is whoever
/// watches the published video.
fn bake_duration(body: &str, seconds: f64, block: &str) -> Result<String> {
    const DECLARED: &str = r#"data-duration="10""#;
    if !body.contains(DECLARED) {
        bail!(
            "{block} no longer declares {DECLARED} — find what it times its clips \
             by before trusting a render, or every chapter past that length goes blank"
        );
    }
    Ok(body.replace(
        DECLARED,
        &format!(r#"data-duration="{}""#, fmt_seconds(seconds)),
    ))
}

#[allow(clippy::too_many_arguments)]
fn write_wrapper(
    workspace: &Path,
    id: &str,
    block: &str,
    block_file: &str,
    values: &serde_json::Value,
    seconds: f64,
    width: u32,
    height: u32,
) -> Result<PathBuf> {
    let duration = fmt_seconds(seconds);
    let encoded = serde_json::to_string(values)?.replace('\'', "&#39;");
    let html = format!(
        r##"<!doctype html>
<html lang="en">
  <head>
    <meta charset="UTF-8" />
    <meta name="viewport" content="width={width}, height={height}" />
    <script src="https://cdn.jsdelivr.net/npm/gsap@3.14.2/dist/gsap.min.js"></script>
    <style>
      * {{ margin: 0; padding: 0; box-sizing: border-box; }}
      html, body {{
        margin: 0; width: {width}px; height: {height}px;
        overflow: hidden; background: #101010;
      }}
    </style>
  </head>
  <body>
    <div id="root" data-composition-id="{id}" data-start="0" data-duration="{duration}" data-width="{width}" data-height="{height}">
      <div
        id="{id}-slot"
        data-composition-id="{block}"
        data-composition-src="compositions/{block_file}"
        data-variable-values='{encoded}'
        data-start="0"
        data-duration="{duration}"
        data-track-index="0"
        data-width="{width}"
        data-height="{height}"
        style="position: absolute; left: 0; top: 0; width: {width}px; height: {height}px"
      ></div>
    </div>
    <script>
      window.__timelines = window.__timelines || {{}};
      window.__timelines["{id}"] = gsap.timeline({{ paused: true }});
    </script>
  </body>
</html>
"##
    );
    let wrapper = workspace.join(format!("compositions/{id}.html"));
    write_if_changed(&wrapper, &html)?;
    Ok(wrapper)
}

fn write_index(workspace: &Path, job: &Job) -> Result<()> {
    let src = workspace.join(&job.composition);
    let body =
        std::fs::read_to_string(&src).with_context(|| format!("reading {}", src.display()))?;
    std::fs::write(workspace.join("index.html"), body)?;
    Ok(())
}

fn blank_index(width: u32, height: u32) -> String {
    format!(
        r##"<!doctype html>
<html lang="en">
  <head>
    <meta charset="UTF-8" />
    <meta name="viewport" content="width={width}, height={height}" />
  </head>
  <body>
    <div id="root" data-composition-id="blank" data-start="0" data-duration="1" data-width="{width}" data-height="{height}"></div>
  </body>
</html>
"##
    )
}

fn fmt_seconds(seconds: f64) -> String {
    let text = format!("{seconds:.3}");
    text.trim_end_matches('0').trim_end_matches('.').to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fmt_seconds_trims_zeros() {
        assert_eq!(fmt_seconds(3.0), "3");
        assert_eq!(fmt_seconds(2.6), "2.6");
        assert_eq!(fmt_seconds(12.345), "12.345");
    }

    /// The block ships `data-duration="10"` on its root, its camera and its
    /// audio. Every one has to become the chapter's real length, or the clip that
    /// keeps the default ends at ten seconds and the frame goes to background.
    #[test]
    fn every_declared_duration_becomes_the_chapter_length() {
        let block = r#"<div id="root" data-duration="10">
             <video id="th-camera-video" class="clip" data-start="0" data-duration="10"></video>
             <audio id="th-audio" data-start="0" data-duration="10"></audio>
           </div>"#;
        let baked = bake_duration(block, 105.0876, "talking-head-vertical").unwrap();
        assert_eq!(baked.matches(r#"data-duration="105.088""#).count(), 3);
        assert!(!baked.contains(r#"data-duration="10""#));
    }

    /// The variable declaration carries `"default":10` as JSON, not as an
    /// attribute, so rewriting durations must leave it alone.
    #[test]
    fn the_variable_default_is_not_an_attribute_and_survives() {
        let block = r#"{&quot;id&quot;:&quot;durationSeconds&quot;,&quot;default&quot;:10}
           <video data-duration="10"></video>"#;
        let baked = bake_duration(block, 14.16, "talking-head-vertical").unwrap();
        assert!(baked.contains("&quot;default&quot;:10"));
        assert!(baked.contains(r#"data-duration="14.16""#));
    }

    /// A block that stopped declaring the default has to stop the render, not
    /// quietly produce another ten-second clip inside a two-minute chapter.
    #[test]
    fn a_block_without_the_declared_default_is_an_error() {
        let err = bake_duration(
            "<div data-duration=\"12\"></div>",
            30.0,
            "talking-head-vertical",
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains("talking-head-vertical"), "{err}");
        assert!(err.contains("goes blank"), "{err}");
    }

    #[test]
    fn wrapper_points_at_the_baked_block() {
        let dir = std::env::temp_dir().join(format!(
            "stream-recorder-compose-{}-wrap",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("compositions")).unwrap();
        write_wrapper(
            &dir,
            "seg-01-card",
            "chapter-title-card",
            "seg-01-card.chapter-title-card.html",
            &serde_json::json!({"chapterTopic": "Hello"}),
            3.0,
            1920,
            1080,
        )
        .unwrap();
        let html = std::fs::read_to_string(dir.join("compositions/seg-01-card.html")).unwrap();
        assert!(html
            .contains("data-composition-src=\"compositions/seg-01-card.chapter-title-card.html\""));
        assert!(html.contains("Hello"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A library and two cut chapters, the least `prepare` needs to lay out a
    /// longform.
    fn fixture(tag: &str) -> (std::path::PathBuf, std::path::PathBuf, std::path::PathBuf) {
        let root = std::env::temp_dir().join(format!(
            "stream-recorder-compose-{}-{tag}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        let (library, edit, compose) = (root.join("lib"), root.join("edit"), root.join("compose"));
        std::fs::create_dir_all(library.join("compositions")).unwrap();
        // `bake_duration` refuses a block that no longer declares the default,
        // and the card is copied rather than baked — so a stub is enough here.
        std::fs::write(
            library.join("compositions/chapter-title-card.html"),
            r#"<div data-duration="10"></div>"#,
        )
        .unwrap();
        std::fs::create_dir_all(library.join("assets")).unwrap();
        std::fs::write(library.join("assets/badge.svg"), b"<svg/>").unwrap();
        for n in 1..=2 {
            let chapter = edit.join(format!("chapter-{n:02}"));
            std::fs::create_dir_all(&chapter).unwrap();
            std::fs::write(chapter.join(format!("chapter-{n:02}-horizontal.mp4")), b"v").unwrap();
        }
        (library, edit, compose)
    }

    /// The bug this pins, reported as "we see chapter 2 first": chapter one used
    /// to be the only chapter with no card, so the first thing a viewer met was
    /// a chapter card rather than the video's title. The longform now opens on
    /// its own title and every chapter is labelled — by its own number.
    #[test]
    fn the_longform_opens_on_its_own_title_then_labels_every_chapter() {
        let (library, edit, compose) = fixture("order");
        let titles = vec![(1u32, "First".to_string()), (2u32, "Second".to_string())];
        let plan = prepare(&edit, &compose, &library, &titles, "Why watermarking fails").unwrap();

        let order: Vec<String> = plan
            .h_segments
            .iter()
            .map(|segment| match segment {
                Segment::Render(job) => job.id.clone(),
                Segment::Passthrough(path) => path
                    .file_stem()
                    .map(|s| s.to_string_lossy().into_owned())
                    .unwrap_or_default(),
            })
            .collect();
        assert_eq!(
            order,
            vec![
                "seg-00-opener",
                "chapter-01-horizontal",
                "seg-02-card",
                "chapter-02-horizontal",
            ]
            .into_iter()
            .map(String::from)
            .collect::<Vec<_>>(),
            "the title card stands in for chapter one's, so chapter one follows it"
        );

        // The card in front of chapter two reads "02": the chapter's own number,
        // which is what the vertical cut, the notes and the blog call it. It
        // used to count cards shown instead, and a chapter with no title then
        // read "02 / Chapter 3".
        let card =
            std::fs::read_to_string(compose.join("horizontal/compositions/seg-02-card.html"))
                .unwrap();
        assert!(card.contains(r#""chapterNumber":"02""#), "{card}");
        assert!(card.contains("Second"));
        // A chapter card still animates in — it arrives mid-video.
        assert!(card.contains(r#""holdFromStart":0"#), "{card}");
        let _ = std::fs::remove_dir_all(edit.parent().unwrap());
    }

    /// A render is made at one quality, and the quality is one of its inputs:
    /// every job names the workspace's quality file, and the file carries the
    /// current setting — so raising it re-renders once, and not again.
    #[test]
    fn the_render_quality_is_a_source_of_every_job() {
        let (library, edit, compose) = fixture("quality");
        let titles = vec![(1u32, "First".to_string()), (2u32, "Second".to_string())];
        let plan = prepare(&edit, &compose, &library, &titles, "A video").unwrap();
        let quality = compose.join("horizontal").join(QUALITY_FILE);
        assert_eq!(
            std::fs::read_to_string(&quality).unwrap(),
            super::super::render::QUALITY
        );
        for job in plan.h_segments.iter().filter_map(Segment::job) {
            assert!(
                job.sources.contains(&quality),
                "{}: {:?}",
                job.id,
                job.sources
            );
        }
        let _ = std::fs::remove_dir_all(edit.parent().unwrap());
    }

    /// The boxes decide what is planned, and an output that is off plans
    /// nothing — no cards, no chapter compositions — rather than planning
    /// everything and skipping at the render.
    #[test]
    fn switched_off_outputs_plan_nothing() {
        use crate::config::RenderTargets;
        let (library, edit, compose) = fixture("targets");
        // Give the fixture verticals too, so both halves of the plan are in play.
        std::fs::write(
            library.join("compositions/talking-head-vertical.html"),
            r#"<div data-duration="10"></div>"#,
        )
        .unwrap();
        for n in 1..=2 {
            std::fs::write(
                edit.join(format!("chapter-{n:02}/chapter-{n:02}-vertical.mp4")),
                b"v",
            )
            .unwrap();
        }
        let titles = vec![(1u32, "First".to_string()), (2u32, "Second".to_string())];
        let plan = |targets: RenderTargets| {
            prepare_targets(&edit, &compose, &library, &titles, "A video", targets)
        };

        // Only the horizontal: cards and bodies, no chapter compositions.
        let horizontal_only = plan(RenderTargets {
            horizontal: true,
            vertical: false,
            shorts: false,
        });
        match horizontal_only {
            Ok(plan) => {
                assert!(!plan.h_segments.is_empty());
                assert!(plan.v_jobs.is_empty(), "{:?}", plan.v_jobs);
                assert!(!plan.targets.vertical);
            }
            // The fixture's vertical stub has no real duration to probe; only
            // the horizontal half is exercised on a machine without ffprobe.
            Err(err) => panic!("{err:#}"),
        }

        // Nothing horizontal: no opener, no cards, no bodies.
        let no_horizontal = RenderTargets {
            horizontal: false,
            vertical: false,
            shorts: false,
        };
        let plan = plan(no_horizontal).unwrap();
        assert!(plan.h_segments.is_empty());
        assert!(plan.v_jobs.is_empty());
        assert!(plan.is_empty());
        let _ = std::fs::remove_dir_all(edit.parent().unwrap());
    }

    /// A chapter nobody has titled gets its number and nothing under it, rather
    /// than a topic that restates — or, as it did, contradicts — the number.
    #[test]
    fn an_untitled_chapter_is_numbered_and_its_topic_left_empty() {
        let (library, edit, compose) = fixture("untitled");
        let titles = vec![(1u32, "First".to_string()), (2u32, String::new())];
        prepare(&edit, &compose, &library, &titles, "A video").unwrap();
        let card =
            std::fs::read_to_string(compose.join("horizontal/compositions/seg-02-card.html"))
                .unwrap();
        assert!(card.contains(r#""chapterNumber":"02""#), "{card}");
        assert!(card.contains(r#""chapterTopic":"""#), "{card}");
        let _ = std::fs::remove_dir_all(edit.parent().unwrap());
    }

    /// The card's separator mark is an `<img src="assets/badge.svg">`, so the
    /// asset has to travel with it. Left behind, the card still renders — with a
    /// broken image where the logo should be, and nothing in the log to say so.
    #[test]
    fn the_badge_the_card_draws_travels_into_the_workspace() {
        let (library, edit, compose) = fixture("badge");
        prepare(
            &edit,
            &compose,
            &library,
            &[(1u32, "Only".into())],
            "A video",
        )
        .unwrap();
        assert!(
            compose.join("horizontal/assets/badge.svg").is_file(),
            "the separator mark is missing from the workspace"
        );
        let _ = std::fs::remove_dir_all(edit.parent().unwrap());
    }

    /// A one-chapter video still opens on its title. The opener used to be added
    /// only when something else was being rendered, and chapter one has no card
    /// of its own — so the only video that needed the title most got none.
    #[test]
    fn a_single_chapter_video_still_gets_its_title_card() {
        let (library, edit, compose) = fixture("single");
        let plan = prepare(
            &edit,
            &compose,
            &library,
            &[(1u32, "Only".to_string())],
            "A short one",
        )
        .unwrap();
        assert_eq!(plan.h_segments.len(), 2, "the title and the one body");
        assert_eq!(
            plan.h_segments[0].job().map(|j| j.id.as_str()),
            Some("seg-00-opener")
        );
        let _ = std::fs::remove_dir_all(edit.parent().unwrap());
    }

    /// The opener carries the video's title, not a chapter's, and leaves the two
    /// chapter slots empty so they collapse.
    #[test]
    fn the_opener_says_the_videos_name_and_claims_no_chapter_number() {
        let (library, edit, compose) = fixture("opener");
        let titles = vec![(1u32, "First".to_string())];
        prepare(&edit, &compose, &library, &titles, "Why watermarking fails").unwrap();
        let html =
            std::fs::read_to_string(compose.join("horizontal/compositions/seg-00-opener.html"))
                .unwrap();
        assert!(html.contains("Why watermarking fails"), "{html}");
        // `write_wrapper` escapes single quotes only, so the JSON keeps its
        // double quotes inside the single-quoted attribute.
        assert!(html.contains(r#""chapterNumber":"""#), "{html}");
        assert!(html.contains(r#""chapterLabel":"""#), "{html}");
        // Composed from frame one: a social preview grabs frame 0, and a blank
        // plate there tells a scroller nothing about the video.
        assert!(html.contains(r#""holdFromStart":1"#), "{html}");
        let _ = std::fs::remove_dir_all(edit.parent().unwrap());
    }

    /// Cards only is not a longform, so there is nothing to open.
    #[test]
    fn a_project_with_no_cuts_gets_no_opener() {
        let (library, edit, compose) = fixture("empty");
        for n in 1..=2 {
            let _ = std::fs::remove_dir_all(edit.join(format!("chapter-{n:02}")));
        }
        std::fs::create_dir_all(&edit).unwrap();
        let plan = prepare(&edit, &compose, &library, &[], "A video").unwrap();
        assert!(plan.h_segments.is_empty());
        let _ = std::fs::remove_dir_all(edit.parent().unwrap());
    }
}

#[cfg(test)]
mod library_tests {
    use super::*;

    /// Every file `prepare` copies has to exist in the vendored library.
    ///
    /// This is the check that was missing while `components_root` pointed at
    /// `../screencast/components`: that library lives in a *different, private*
    /// repo, so it was never present on a fresh clone and every test over it
    /// skipped. The absence surfaced instead as a render dying at runtime, on a
    /// recording someone had already made.
    ///
    /// The list is duplicated from `prepare` on purpose — sharing a helper with
    /// the code under test would let both sides be wrong together.
    #[test]
    fn the_vendored_library_has_everything_prepare_copies() {
        let library = components_root();
        for rel in [
            "compositions/chapter-title-card.html",
            "compositions/talking-head-vertical.html",
            "assets/pattern-rings.svg",
            "assets/badge.svg",
            "assets/silence.mp3",
            "assets/fonts/Booton-Regular.woff2",
            "assets/fonts/Booton-Semibold.woff2",
            "assets/fonts/Booton-Bold.woff2",
        ] {
            assert!(
                library.join(rel).is_file(),
                "{rel} missing from {} — a render fails on this",
                library.display()
            );
        }
    }

    /// `prepare` bails on exactly this file, so its absence is what becomes the
    /// "HyperFrames library missing" message.
    #[test]
    fn the_library_resolves_inside_this_repo() {
        let library = components_root();
        assert!(
            library
                .join("compositions/chapter-title-card.html")
                .is_file(),
            "library did not resolve to the vendored copy: {}",
            library.display()
        );
        assert!(
            !library.to_string_lossy().contains("screencast"),
            "still resolving to the private sibling checkout: {}",
            library.display()
        );
    }

    /// Whatever a composition asks for by relative path has to be carried too.
    /// `talking-head-vertical` names two placeholders `prepare` never copies,
    /// because it rewrites those paths to the real media first — so the check
    /// has to know about that substitution rather than flag it.
    #[test]
    fn every_asset_a_composition_references_is_vendored() {
        let library = components_root();
        let substituted = [
            "assets/placeholder-camera.mp4",
            "assets/placeholder-audio.mp3",
            "assets/placeholder-screen.mp4",
        ];
        let Ok(entries) = std::fs::read_dir(library.join("compositions")) else {
            return;
        };
        for entry in entries.flatten() {
            let Ok(html) = std::fs::read_to_string(entry.path()) else {
                continue;
            };
            for rel in referenced_assets(&html) {
                if substituted.contains(&rel.as_str()) {
                    continue;
                }
                assert!(
                    library.join(&rel).is_file(),
                    "{} references {rel}, which is not vendored",
                    entry.file_name().to_string_lossy()
                );
            }
        }
    }

    /// `prepare` against the **real** vendored library, not a stub.
    ///
    /// Every other test here builds a synthetic library in a temp dir, which is
    /// right for exercising the planning logic and useless for the failure that
    /// actually happened: the library was absent, and no test that supplies its
    /// own could notice. This is the one that runs the real thing.
    #[test]
    fn prepare_succeeds_against_the_vendored_library() {
        let root =
            std::env::temp_dir().join(format!("stream-recorder-vendored-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let (edit, compose) = (root.join("edit"), root.join("compose"));
        for n in 1..=2 {
            let chapter = edit.join(format!("chapter-{n:02}"));
            std::fs::create_dir_all(&chapter).unwrap();
            std::fs::write(chapter.join(format!("chapter-{n:02}-horizontal.mp4")), b"v").unwrap();
        }

        let plan = prepare(
            &edit,
            &compose,
            &components_root(),
            &[(1, "First".into()), (2, "Second".into())],
            "A real video",
        )
        .expect("prepare against the vendored library");

        // The exact files a render reads out of the workspace. `copy_library`
        // skips a source that is not there rather than failing, so checking the
        // destination is what proves the library actually carried them.
        for rel in [
            "horizontal/compositions/chapter-title-card.html",
            "horizontal/assets/badge.svg",
            "horizontal/assets/pattern-rings.svg",
            "horizontal/assets/silence.mp3",
            "horizontal/assets/fonts/Booton-Regular.woff2",
            "vertical/compositions/talking-head-vertical.html",
            "vertical/assets/fonts/Booton-Bold.woff2",
        ] {
            assert!(
                compose.join(rel).is_file(),
                "{rel} never reached the workspace — the library is incomplete"
            );
        }
        assert!(plan.render_count() > 0, "nothing to render");
        let _ = std::fs::remove_dir_all(&root);
    }

    /// The `assets/...` paths a composition names.
    ///
    /// Hand-rolled rather than a regex dependency, and it stops at the first
    /// character that cannot appear in a path: these files carry JSON inside
    /// HTML attributes, so a reference is as likely to be followed by `&quot;`
    /// as by a plain quote.
    fn referenced_assets(html: &str) -> Vec<String> {
        let mut found: Vec<String> = Vec::new();
        for (idx, _) in html.match_indices("assets/") {
            let rel: String = html[idx..]
                .chars()
                .take_while(|c| c.is_ascii_alphanumeric() || matches!(c, '/' | '.' | '-' | '_'))
                .collect();
            if rel.contains('.') && !found.iter().any(|f| *f == rel) {
                found.push(rel);
            }
        }
        found
    }
}
