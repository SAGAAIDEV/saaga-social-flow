//! Building, finishing and discarding one chapter's outputs.
//!
//! Split from the state machine in [`super`] because it is a different job:
//! that file decides *when* a chapter begins and ends, this one owns
//! everything that happens at the boundary — the writers, the shared anchor
//! both files start from, the per-chapter [`Graph`] lifecycle, the file paths,
//! and the transcode and `.discarded/` moves on the way out.
//!
//! The invariant that holds these together: **build before swap, close after
//! move**. A chapter's replacements are constructed before the live ones are
//! taken out, so no buffer falls into a writerless gap; and a graph's sidecar
//! is written beside wherever its media ended up, including inside
//! `.discarded/`, so a recovered take is never recovered without its data.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{bail, Context, Result};
use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2_core_media::{CMClock, CMTime};
use objc2_foundation::{NSDictionary, NSString};

use crate::capture::av::create_chapter_writer;
use crate::capture::av_delegate::AvState;
use crate::capture::composed::create_composed_writer;
use crate::capture::screen_delegate::{ScreenDelegate, ScreenState};
use crate::capture::screen_writer::create_screen_writer;
use crate::markers::MarkerLog;
use crate::ops::{graphs, Graph, StreamCtx, StreamId};
use crate::region::PixelSize;
use crate::router::Router;
use crate::timesync;
use crate::transcode;

/// Where a thrown-away take goes, under the session directory. A move, never a
/// delete: the folder is documented to the operator as recoverable.
pub const DISCARDED: &str = ".discarded";

/// One timestamp per discard, so every file of one discarded take — camera,
/// screen, composed outputs, sidecars — carries the same number and stays
/// recognisable as a set inside `.discarded/`.
fn discard_stamp() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
}

/// Everything the Router needs to write the screen half of each chapter.
/// Absent when no display is selected — screen capture is opt-in.
pub struct ScreenTrack {
    /// The handle screen chapter writers are swapped through.
    pub state: Arc<std::sync::Mutex<Option<ScreenState>>>,
    /// Kept so each new chapter can be seeded with the last frame the stream
    /// delivered — see [`ScreenDelegate::seed_chapter`].
    pub delegate: Retained<ScreenDelegate>,
    /// Hand-built H.264 settings; see `screen_stream::video_settings`.
    pub settings: Retained<NSDictionary<NSString, AnyObject>>,
    /// The clock the stream timestamps its frames against. Not assumed to be
    /// the capture session's — measured at 3.6 ppm apart on the machine this
    /// was built on, because a capture session with an audio input runs on the
    /// audio interface's clock.
    pub clock: Retained<CMClock>,
    /// The stream's real pixel size, as configured on the `SCStream`.
    ///
    /// Carried rather than read back out of `settings`, which already holds it
    /// under `AVVideoWidthKey`/`AVVideoHeightKey`: `ScreenConnection` knows
    /// these first-hand, and two sources of truth for one number is how a pool
    /// ends up sized differently from the buffers it has to hold. The camera
    /// side has no equivalent — see [`StreamCtx::size`].
    pub width: usize,
    pub height: usize,
}

impl Router {
    /// Build both of a chapter's writers, anchored to one shared instant.
    ///
    /// Building is separate from installing so a cut can have the replacements
    /// ready before it takes the old ones out — the delegates drop every
    /// buffer that arrives while no writer is installed, so that gap is
    /// literally lost recording. It also means a failure to build the screen
    /// writer aborts the cut with the current chapter still intact, rather
    /// than leaving the camera swapped and the screen not.
    pub(super) fn build_chapter(&self, chapter: u32) -> Result<(AvState, Option<ScreenState>)> {
        let (av_anchor, screen_anchor) = self.anchors();

        // Graphs are opened here, on the main thread, before either writer
        // exists — so an op allocates at a chapter boundary rather than on a
        // capture queue, and a graph that refuses to open aborts the cut with
        // the current chapter still intact, exactly like a writer that refuses
        // to build.
        let camera_graph = self.open_graph(StreamId::Camera, chapter)?;

        // Which layout this chapter is framed for, beside where its media will
        // land. The render reads it to tell an outline chapter — whose body it
        // has to draw — from one it passes through. A warning rather than an
        // abort: a session directory that cannot take a hundred-byte file is
        // about to fail on the writer anyway, and this must never be the thing
        // that stops a take.
        if let Err(err) =
            crate::layouts::record_chapter_layout(&self.session_dir, chapter, self.pair)
        {
            eprintln!("stream-recorder: could not record chapter {chapter:02}'s layout: {err:#}");
        }

        let chapter_writer = create_chapter_writer(
            &self.video_settings,
            &self.audio_settings,
            &self.chapter_path(chapter),
        )
        .with_context(|| format!("creating writer for chapter {chapter}"))?;

        let mut composed = Vec::new();
        for spec in graphs::preview_sinks(self.pair) {
            let path = spec.path_in(&self.session_dir, chapter);
            let writer = create_composed_writer(spec, Some(&self.audio_settings), &path)
                .with_context(|| format!("creating composed writer for chapter {chapter}"))?;
            composed.push(writer);
        }

        let screen = match (&self.screen, screen_anchor) {
            (Some(track), Some(anchor)) => {
                let screen_graph = self.open_graph(StreamId::Screen, chapter)?;
                let writer = create_screen_writer(
                    &track.settings,
                    &self.screen_chapter_path(chapter),
                    PixelSize {
                        w: track.width,
                        h: track.height,
                    },
                )
                .with_context(|| format!("creating screen writer for chapter {chapter}"))?;
                Some(ScreenState::new_for_chapter(writer, anchor, screen_graph))
            }
            _ => None,
        };

        Ok((
            AvState::new_for_chapter(chapter_writer, composed, av_anchor, camera_graph),
            screen,
        ))
    }

    /// Build and open one stream's graph for `chapter`.
    ///
    /// `size`/`fps` are `Some` for the screen and `None` for the camera, and
    /// that asymmetry is honest rather than lazy — see [`StreamCtx::size`].
    pub(super) fn open_graph(&self, stream: StreamId, chapter: u32) -> Result<Graph> {
        let (clock, size, fps) = match stream {
            StreamId::Camera => (self.av_clock.clone(), None, None),
            StreamId::Screen => {
                let track = self
                    .screen
                    .as_ref()
                    .context("asked for a screen graph with no screen track")?;
                (
                    track.clock.clone(),
                    Some((track.width, track.height)),
                    Some(f64::from(crate::capture::screen_stream::FPS)),
                )
            }
        };
        let mut graph = graphs::default_graph(stream);
        graph
            .open(&StreamCtx {
                stream,
                chapter,
                clock,
                size,
                fps,
                renderer: None,
                pool: None,
            })
            .with_context(|| {
                format!(
                    "opening the {} graph for chapter {chapter}",
                    stream.as_str()
                )
            })?;
        Ok(graph)
    }

    /// One instant in real time, expressed on each stream's own clock.
    ///
    /// This is the whole sync mechanism. Both files put *this* moment at t=0,
    /// so a marker at 4.2s means the same thing in each, even though the two
    /// streams' buffers arrive at different times against different clocks.
    ///
    /// The clocks still tick at slightly different rates (3.6 ppm apart when
    /// measured against an Elgato XLR Dock — roughly 13 ms per hour), so the
    /// two files drift within a chapter. Bounded by chapter length, that is
    /// ~2 ms over ten minutes, which is why per-frame retiming is not worth
    /// its cost here. If chapters ever run for hours, revisit that.
    pub(super) fn anchors(&self) -> (CMTime, Option<CMTime>) {
        let host = unsafe { CMClock::host_time_clock() };
        let now = timesync::TimeSync::now_host_time();
        (
            timesync::convert(now, &host, &self.av_clock),
            self.screen
                .as_ref()
                .map(|track| timesync::convert(now, &host, &track.clock)),
        )
    }

    pub(super) fn chapter_path(&self, chapter: u32) -> PathBuf {
        self.session_dir.join(format!("chapter-{chapter:02}.mp4"))
    }

    pub(super) fn screen_chapter_path(&self, chapter: u32) -> PathBuf {
        self.session_dir
            .join(format!("chapter-{chapter:02}-screen.mp4"))
    }

    /// Finish a chapter: mark inputs finished, wait for writer completion,
    /// extract audio tracks, transcode to mp3, kick off a background
    /// transcript, and log the event.
    ///
    /// The screen file gets the metadata repair but no audio derivatives —
    /// it has no audio track; the mic lives in the camera file.
    pub(super) fn finish_chapter(
        &self,
        av: AvState,
        screen: Option<ScreenState>,
        chapter_num: u32,
    ) -> Result<()> {
        let mut transcribing = false;
        let result = self.finish_chapter_files(av, screen, chapter_num, &mut transcribing);
        // A failure before the transcript job started used to leave a closed
        // chapter (its mp4 is on disk) with no job and no transcript file, and
        // the render waited on it for ten minutes. Written down instead, with
        // the error, so the wait ends and says why; the next Render remakes the
        // mp3 and tries again.
        if let Err(err) = &result {
            if !transcribing {
                crate::notes::record_transcript_failure(
                    &self.chapter_path(chapter_num),
                    &format!("closing the chapter failed before its audio was sent: {err:#}"),
                );
            }
        }
        result
    }

    fn finish_chapter_files(
        &self,
        mut av: AvState,
        mut screen: Option<ScreenState>,
        chapter_num: u32,
        transcribing: &mut bool,
    ) -> Result<()> {
        let chapter_path = self.chapter_path(chapter_num);

        // Both writers are finalized first, before any derived work — and both
        // are attempted even if the first fails. Everything below this point
        // (metadata repair, op sidecars, audio extraction) can return an error,
        // and none of it may be allowed to leave a writer unfinalized: an mp4
        // that never got finishWriting has no moov atom and does not play at
        // all. A missing mp3 is an annoyance; an unplayable take is the shoot.
        let av_finished = av.finish();
        let screen_finished = screen
            .as_ref()
            .map(|screen| screen.finish().context("finishing screen chapter"))
            .transpose();
        av_finished?;
        screen_finished?;

        // Fix broken duration metadata that AVAssetWriter sometimes sets when
        // multiple writers feed from the same continuous capture stream.
        transcode::fix_mp4_metadata(&chapter_path).context("fixing mp4 metadata")?;

        // Same place, same reason: this is where a finished chapter's derived
        // artifacts are produced.
        close_graph(&mut av.graph, &chapter_path);

        for sink in &av.composed {
            let path = sink.spec.path_in(&self.session_dir, chapter_num);
            transcode::fix_mp4_metadata(&path)
                .with_context(|| format!("fixing {} mp4 metadata", sink.spec.name()))?;
        }

        // Extract audio and transcode.
        let _m4a_path = transcode::extract_audio_copy(&chapter_path)
            .context("extracting audio from chapter")?;
        let mp3_path =
            transcode::to_mp3(&chapter_path).context("transcoding chapter audio to mp3")?;
        crate::notes::spawn_chapter_transcript(mp3_path);
        *transcribing = true;

        if let Some(screen) = &mut screen {
            let screen_path = self.screen_chapter_path(chapter_num);
            transcode::fix_mp4_metadata(&screen_path).context("fixing screen mp4 metadata")?;
            close_graph(&mut screen.graph, &screen_path);
        }

        // Log the chapter close event.
        self.marker_log
            .log_chapter_closed(chapter_num, &chapter_path)?;

        Ok(())
    }

    /// Discard a chapter (retake): finish the writer, move the file to
    /// .discarded/, and log the event.
    ///
    /// Deliberately `finish()`, NOT `cancelWriting()`: cancelWriting deletes
    /// the output file outright (per its documentation — discovered the hard
    /// way when the .discarded/ move found nothing to move). Finishing keeps
    /// the discarded take playable, which is the point of .discarded/ being
    /// a recoverable location rather than a delete.
    pub(super) fn discard_chapter(
        &self,
        mut av: AvState,
        mut screen: Option<ScreenState>,
        chapter_num: u32,
    ) -> Result<()> {
        av.finish().context("finishing discarded take")?;
        if let Some(screen) = &screen {
            screen.finish().context("finishing discarded screen take")?;
        }

        let discarded_dir = self.session_dir.join(DISCARDED);
        fs::create_dir_all(&discarded_dir).context("creating .discarded directory")?;
        let now = discard_stamp();

        let discarded_path = discarded_dir.join(format!("chapter-{chapter_num:02}-{now}.mp4"));
        let chapter_path = self.chapter_path(chapter_num);
        fs::rename(&chapter_path, &discarded_path).with_context(|| {
            format!(
                "moving {} to {}",
                chapter_path.display(),
                discarded_path.display()
            )
        })?;
        // Beside the *discarded* path, not the chapter path it no longer
        // occupies: .discarded/ is documented above as a recoverable location,
        // and a take recovered without its analysis data is not recovered.
        close_graph(&mut av.graph, &discarded_path);

        for sink in &av.composed {
            let path = sink.spec.path_in(&self.session_dir, chapter_num);
            let discarded = discarded_dir.join(format!(
                "chapter-{chapter_num:02}-{now}-{}.mp4",
                sink.spec.name()
            ));
            if path.exists() {
                fs::rename(&path, &discarded).with_context(|| {
                    format!("moving {} to {}", path.display(), discarded.display())
                })?;
            }
        }

        // The layout record goes with the take it describes, under the same
        // stamp, so a recovered take still says what it was framed for.
        let layout = crate::layouts::layout_path(&self.session_dir, chapter_num);
        if layout.exists() {
            let discarded_layout =
                discarded_dir.join(format!("chapter-{chapter_num:02}-{now}.layout.json"));
            fs::rename(&layout, &discarded_layout).with_context(|| {
                format!(
                    "moving {} to {}",
                    layout.display(),
                    discarded_layout.display()
                )
            })?;
        }

        if let Some(screen) = &mut screen {
            let screen_path = self.screen_chapter_path(chapter_num);
            let discarded_screen =
                discarded_dir.join(format!("chapter-{chapter_num:02}-{now}-screen.mp4"));
            fs::rename(&screen_path, &discarded_screen).with_context(|| {
                format!(
                    "moving {} to {}",
                    screen_path.display(),
                    discarded_screen.display()
                )
            })?;
            close_graph(&mut screen.graph, &discarded_screen);
        }

        // Log the take discard event.
        self.marker_log
            .log_take_discarded(chapter_num, &discarded_path)?;

        Ok(())
    }

    /// Move a *closed* chapter's files into `.discarded/` so the same number
    /// can be recorded again.
    ///
    /// [`Router::discard_chapter`] retakes the chapter that is rolling; this is
    /// its counterpart for a chapter finished some time ago — picked from the
    /// Record tab's chapter menu once the whole take was in the can and one
    /// chapter of it turned out wrong. No Router is alive at that point, so
    /// this works over the session directory alone, and the caller opens the
    /// chapter afresh with [`Router::start_at`] once it returns.
    ///
    /// Everything named for the chapter goes: the camera and screen masters,
    /// the composed outputs, the audio extracted from them, the transcript, the
    /// op sidecars, and the chapter's edit directory when there is one. Not
    /// because the recorder minds most of them — `finish_chapter` overwrites
    /// the audio, and a cut is mtime-checked against its source — but because
    /// two of them would quietly outlive the take they describe. The
    /// transcriber skips a chapter whose `.transcript.json` reads as completed,
    /// so the new take would never get words; and a hand keep-list is a list of
    /// milliseconds into a file that no longer exists. Moving the lot under one
    /// stamp is also what keeps the old take recoverable as a set, rather than
    /// leaving pieces of it behind to be mistaken for the new one.
    ///
    /// Returns where the camera master went — or, for a chapter left with only
    /// its derivatives, where the first of those went — for the status line
    /// and the marker log.
    pub fn retire_chapter(
        session_dir: &Path,
        edit_chapter_dir: &Path,
        chapter: u32,
    ) -> Result<PathBuf> {
        let stem = format!("chapter-{chapter:02}");
        let master = format!("{stem}.mp4");

        // Collected before anything moves: renaming entries out from under a
        // `read_dir` in progress is unspecified, and this is not the place to
        // find out what the filesystem does about it.
        let mut named_for_chapter = Vec::new();
        for entry in fs::read_dir(session_dir)
            .with_context(|| format!("listing {}", session_dir.display()))?
        {
            let entry = entry.context("reading a session directory entry")?;
            let name = entry.file_name().to_string_lossy().into_owned();
            // `chapter-03.mp4`, `chapter-03-screen.mp4`, `chapter-03.stats.json`
            // — and not `chapter-030.mp4`, which is somebody else's chapter.
            let Some(rest) = name.strip_prefix(&stem) else {
                continue;
            };
            if rest.starts_with('.') || rest.starts_with('-') {
                let rest = rest.to_string();
                named_for_chapter.push((entry.path(), name, rest));
            }
        }
        // Nothing to retire is an error, not a no-op: the caller is about to
        // announce a retake, and must not announce one of a chapter that was
        // never recorded. A chapter down to its mp3 and transcript still
        // counts — those are exactly the leftovers a fresh take must not
        // inherit.
        if named_for_chapter.is_empty() {
            bail!(
                "chapter {chapter:02} has no take to retire in {}",
                session_dir.display()
            );
        }
        // `read_dir` order is whatever the filesystem feels like; the marker
        // log's landmark below should not depend on it.
        named_for_chapter.sort_by(|a, b| a.1.cmp(&b.1));

        let discarded_dir = session_dir.join(DISCARDED);
        fs::create_dir_all(&discarded_dir).context("creating .discarded directory")?;
        let now = discard_stamp();

        let mut moved_master = None;
        let mut first_moved = None;
        for (from, name, rest) in named_for_chapter {
            // The same shape `discard_chapter` writes: the stamp goes between the
            // stem and whatever followed it, so `chapter-03-screen.mp4` becomes
            // `chapter-03-{now}-screen.mp4` and `chapter-03.mp3` becomes
            // `chapter-03-{now}.mp3`.
            let to = discarded_dir.join(format!("{stem}-{now}{rest}"));
            fs::rename(&from, &to)
                .with_context(|| format!("moving {} to {}", from.display(), to.display()))?;
            if name == master {
                moved_master = Some(to.clone());
            }
            first_moved.get_or_insert(to);
        }
        if edit_chapter_dir.is_dir() {
            let to = discarded_dir.join(format!("{stem}-{now}-edit"));
            fs::rename(edit_chapter_dir, &to).with_context(|| {
                format!("moving {} to {}", edit_chapter_dir.display(), to.display())
            })?;
        }

        let landmark = moved_master
            .or(first_moved)
            .expect("at least one file was moved");
        MarkerLog::new(session_dir)?.log_take_discarded(chapter, &landmark)?;
        Ok(landmark)
    }
}

/// Close one chapter's graph beside the file it describes, loudly but never
/// fatally.
///
/// A sidecar write failure must not fail a chapter. Losing an op's data for one
/// chapter is a bad afternoon; losing the recording because a JSON write hit a
/// full disk is a lost shoot. The house stance on this codebase's failure modes
/// is to make them loud, not fatal — so this logs and returns rather than
/// propagating, and the chapter finishes either way.
fn close_graph(graph: &mut Graph, beside: &Path) {
    if let Err(error) = graph.close(beside) {
        eprintln!(
            "stream-recorder: could not write the op sidecars for {}: {error:#} \
             (the recording itself is unaffected)",
            beside.display()
        );
    }
}
