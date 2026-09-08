//! Application state and lifecycle.
//!
//! Tabs organize the pipeline:
//! 1. Draft: Camera/mic/screen dropdowns, record controls, live preview and notes deck.
//! 2. Edit: hand-trim each chapter's keep-list before anything is cut.
//! 3. Render: Titles, then disfluency cut + HyperFrames.
//! 4. Post: AI social media copy generation for 8 platforms.
//! 5. Substack: notes to hand-type an essay from. A post step nothing sends.
//! 6. Distribute: public S3 upload.
//! 7. Schedule: build a Buffer plan, review it, queue it.
//! 8. Analytics: sample sent posts at 7 and 30 days, then report.
//! 9. Reflect: read every step, propose and validate better prompts.

pub(crate) mod clock;
mod devices;
pub(crate) mod card;
pub(crate) mod face;
mod figures;
pub(crate) mod pointer;
pub(crate) mod framing;
pub(crate) mod resolve;
mod startup;
mod video_brief;

pub use startup::run_record_session;

use std::collections::BTreeMap;
use std::sync::mpsc::{self, Receiver};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use objc2::rc::Retained;
use winit::application::ApplicationHandler;
use winit::dpi::LogicalSize;
use winit::event::WindowEvent;
use winit::event_loop::{ActiveEventLoop, ControlFlow};
use winit::window::{Window, WindowId};

use crate::capture::device_picker::CaptureDevice;
use crate::capture::{av, screen_stream};
use crate::hotkeys::{Action, Hotkeys};
use crate::layouts::{Orientation, Pair};
use crate::overlay::RegionOverlay;
use crate::region::placement::{Placement, Resolved};
use crate::region::DisplayGeometry;
use crate::router::Router;
use crate::session::Session;
use crate::ui::{self, UiEvent};

/// ~60Hz, matching region-marker: fast enough that hotkeys feel instant, slow
/// enough to be free.
const TICK: Duration = Duration::from_millis(16);

/// How often to recompute the work queue. Samples come due by the day, so this is
/// about noticing at all rather than noticing quickly.
const DUE_SCAN_EVERY: Duration = Duration::from_secs(60);

/// Everything that only exists once there is a window.
struct Live {
    window: Window,
    hotkeys: Hotkeys,
    /// Events from the window's controls; same pipeline as hotkeys.
    ui_rx: Receiver<UiEvent>,
    /// A clone of the controls' sender, so the region overlay can post into
    /// the same queue the buttons and hotkeys use.
    ui_tx: std::sync::mpsc::Sender<UiEvent>,
    /// NSControl.target is weak — dropping this silently kills the controls.
    control_target: Retained<ui::ControlTarget>,
    /// The region overlay for the selected display, built lazily on the first
    /// Show Regions press and rebuilt when the display changes. Dropping it
    /// orders it off screen.
    overlay: Option<RegionOverlay>,
    /// The figure snip overlay, built on the first ⌃⇧S and rebuilt when the
    /// display changes. Like `overlay`, dropping it orders it off screen —
    /// which matters more here, because this one eats every click on the
    /// display it covers.
    snip: Option<crate::figure::snip::Snip>,
    preview: ui::PreviewHost,
    notes: crate::notes::NotesPane,
    posts_form: crate::posts::PostsForm,
    schedule_form: crate::schedule::ScheduleForm,
    reflect_pane: ui::WebPane,
    thumbnail_pane: ui::WebPane,
    review_pane: ui::WebPane,
    edit_pane: Option<ui::WebPane>,
    substack_pane: Option<ui::WebPane>,
    blog_pane: ui::WebPane,
    publish_pane: ui::WebPane,
    video_brief_pane: ui::WebPane,
    settings_pane: ui::WebPane,
    layout: ui::Layout,
}

pub struct App {
    session: Session,
    notes_tx: mpsc::Sender<crate::notes::NotesEvent>,
    notes_rx: Receiver<crate::notes::NotesEvent>,
    render_tx: mpsc::Sender<crate::edit::RenderEvent>,
    render_rx: Receiver<crate::edit::RenderEvent>,
    /// True while the cut-and-render job is in flight. It rewrites `edit/vN` and
    /// `render/vN` in place, so a second press would have two threads composing
    /// into the same folders.
    render_busy: bool,
    posts_tx: mpsc::Sender<crate::posts::PostsEvent>,
    posts_rx: Receiver<crate::posts::PostsEvent>,
    substack_tx: mpsc::Sender<crate::substack::SubstackEvent>,
    substack_rx: Receiver<crate::substack::SubstackEvent>,
    /// True while the notes are being written. It rewrites `substack/vN` in
    /// place, so a second press would have two threads over the same two files.
    substack_busy: bool,
    blog_tx: mpsc::Sender<crate::blog::BlogEvent>,
    blog_rx: Receiver<crate::blog::BlogEvent>,
    /// True while the article is being written and posted. The whole job is one
    /// network write of a public page, so a second press must not start a second.
    blog_busy: bool,
    titles_tx: mpsc::Sender<crate::titles::TitlesEvent>,
    titles_rx: Receiver<crate::titles::TitlesEvent>,
    reflect_tx: mpsc::Sender<crate::reflect::ReflectEvent>,
    reflect_rx: Receiver<crate::reflect::ReflectEvent>,
    reflect_busy: bool,
    card_tx: mpsc::Sender<crate::card::raster::RasterEvent>,
    card_rx: Receiver<crate::card::raster::RasterEvent>,
    video_copy_job: Option<video_brief::CopyJob>,
    /// The offscreen web view while a card is being photographed. Its presence
    /// *is* the busy flag — there is exactly one at a time — and holding it is
    /// what keeps the navigation delegate alive, which `WKWebView` does not.
    card_raster: Option<crate::card::raster::Raster>,
    /// The card and still hash the in-flight snapshot was started for, so the
    /// picture is filed under the words it was actually drawn from rather than
    /// whatever is in the boxes when it comes back.
    card_pending: Option<crate::card::assets::Job>,
    figure_tx: mpsc::Sender<crate::figure::FigureEvent>,
    figure_rx: Receiver<crate::figure::FigureEvent>,
    /// True while a blurb pass is in flight. Not shared with the shutter: a
    /// figure can be snipped while blurbs are being written for earlier ones,
    /// and the two touch different rows of the same append-only ledger.
    figure_busy: bool,
    /// The break a figure is being explained on, while one is — see
    /// `figures::Break`. Any open chapter is paused for as long as this is
    /// `Some`, and every path that finishes a chapter ends it first.
    aside: Option<figures::Break>,
    thumbnail_tx: mpsc::Sender<crate::thumbnail::ThumbnailEvent>,
    thumbnail_rx: Receiver<crate::thumbnail::ThumbnailEvent>,
    thumbnail_busy: bool,
    analytics_tx: mpsc::Sender<crate::analytics::AnalyticsEvent>,
    analytics_rx: Receiver<crate::analytics::AnalyticsEvent>,
    distribute_tx: mpsc::Sender<crate::distribute::DistributeEvent>,
    distribute_rx: Receiver<crate::distribute::DistributeEvent>,
    /// Same guard for the upload: it pushes to public S3 and rewrites
    /// `links.json`, so a double press would re-upload every asset.
    distribute_busy: bool,
    schedule_tx: mpsc::Sender<crate::schedule::ScheduleEvent>,
    schedule_rx: Receiver<crate::schedule::ScheduleEvent>,
    /// True while a plan or queue job is in flight. Both touch the same files and
    /// Queue posts publicly, so a second press (or a double click) must not start
    /// a second thread — two concurrent queues cannot see each other's ledger rows.
    schedule_busy: bool,
    /// Same guard for the analytics pull: it appends to `analytics.jsonl`, and two
    /// concurrent pulls could not see each other's rows.
    analytics_busy: bool,
    publish_tx: mpsc::Sender<crate::publish::PublishEvent>,
    publish_rx: Receiver<crate::publish::PublishEvent>,
    /// True while the longform is going up. Its own flag, not the schedule's:
    /// uploading a video and queueing social posts are different systems that
    /// happen to be reachable from the same window.
    publish_busy: bool,
    /// Which chapter the Edit tab has open. View state, not project state — nothing on
    /// disk records it, and reopening the app lands on the first chapter.
    open_edit_chapter: Option<u32>,
    /// The most recent report markdown, kept so the work queue can be repainted
    /// above it without re-reading the file.
    last_report: Option<String>,
    /// How long this session has been recording, and how much of it was speech.
    /// Display only — nothing downstream reads it, and the cut is the authority
    /// on what the video ends up being. See [`clock`].
    clock: clock::RecordClock,
    /// Throttles the work-queue rescan. The scan is local-only, but it walks every
    /// project folder, so once a minute is plenty for files that change by the day.
    due_checked: Option<Instant>,
    live: Option<Live>,
    /// `None` until the first New Chapter press, and again after a device
    /// switch: capture is running but no writer is attached, so buffers are
    /// counted and dropped. See the module docs for why this is the default.
    router: Option<Router>,
    connection: Option<av::Connection>,
    /// The chapter number the next New Chapter press opens while `router` is
    /// `None`. Survives device switches so numbering never restarts mid-session.
    next_chapter: u32,
    cameras: Vec<CaptureDevice>,
    mics: Vec<CaptureDevice>,
    displays: Vec<CaptureDevice>,
    camera_uid: String,
    audio_uid: String,
    /// The display the screen dropdown is pointing at, `None` for "No screen".
    screen_uid: Option<String>,
    /// The running screen capture. Present when a display is selected *and*
    /// the current layout has a screen slot — see [`App::screen_wanted`].
    /// Like `connection`, it runs writerless until a chapter opens.
    screen: Option<screen_stream::ScreenConnection>,
    /// The 2x2 this session is currently on. Together they name the layout,
    /// which decides the screen region's aspect and whether there is a screen
    /// capture at all — see [`App::layout`].
    pair: Pair,
    /// A layout picked mid-take, waiting for the next chapter to start.
    ///
    /// The output size is locked into an `AVAssetWriterInput` when the chapter's
    /// writer is created, so a layout cannot change inside one. Rather than cut a
    /// chapter nobody asked for, the choice is held here and applied by the next
    /// New Chapter — a layout is something you set for a take, not during it.
    pending_pair: Option<Pair>,
    orientation: Orientation,
    /// The selected display's geometry, `None` when no display is selected.
    /// Every region is expressed in this display's point space, so both are
    /// rebuilt together by [`App::rebuild_regions`].
    geometry: Option<DisplayGeometry>,
    /// What the operator set for each screen-bearing layout: offset and zoom.
    /// The saved half of a region, keyed by hyperframes block id.
    placements: BTreeMap<&'static str, Placement>,
    /// Those placements resolved against the display and the parent link — the
    /// rects actually captured and drawn.
    ///
    /// One entry per layout that has a slot, i.e. the two Split cells, so a
    /// lookup that misses is exactly the "this layout has no screen" case and
    /// no separate flag can disagree with it.
    regions: BTreeMap<&'static str, Resolved>,
    renderer: Option<crate::ops::Renderer>,
    /// Who the next YouTube upload goes up to. Mirrors `config.youtube_privacy`
    /// so the picker and the upload cannot disagree within a session.
    youtube_privacy: crate::publish::youtube::Privacy,
    /// Live face tracking's saved settings, including whether it is wanted.
    face_config: crate::config::FaceTracking,
    /// The loaded detector. `None` when tracking is off *or* still loading —
    /// the composites cannot tell those apart and should not have to. See
    /// [`face`] for the state machine.
    face_tracker: Option<std::sync::Arc<crate::face::FaceTracker>>,
    /// A background build is in flight. Guards against a double-click starting
    /// two 34 MB downloads.
    face_loading: bool,
    /// Mouse tracking's saved settings, including whether it is wanted.
    pointer_config: crate::config::MouseTracking,
    /// The pointer tracker, when tracking is on. `None` is off — there is no
    /// third state here, unlike `face_tracker`, because nothing has to load.
    pointer_tracker: Option<std::sync::Arc<crate::pointer::PointerTracker>>,
    /// The Notes tab's provider→model choice. One value, because the provider
    /// decides which models exist — see [`crate::notes::Picker`].
    notes_pick: crate::notes::Picker,
    /// Every provider OpenRouter offers, shared by both popups: it is the same
    /// list, and only the selection differs.
    notes_providers: Vec<String>,
    notes_prompt: String,

    /// The Post tab's own choice. Separate from `notes_pick` and not derived
    /// from it — the two tabs used to share a model menu, so changing one
    /// provider silently re-pointed the other tab's list.
    posts_pick: crate::notes::Picker,
    posts_prompt: String,
    posts_manifest: Option<crate::posts::PostsManifest>,
}


impl App {
    fn start(&mut self, event_loop: &ActiveEventLoop) -> Result<Live> {
        let attrs = Window::default_attributes()
            .with_title("stream-recorder")
            // LogicalSize, NOT PhysicalSize: AppKit frames are in points, so
            // the controls are laid out in points too.
            .with_inner_size(LogicalSize::new(ui::WINDOW_WIDTH, ui::WINDOW_HEIGHT))
            .with_min_inner_size(LogicalSize::new(
                ui::WINDOW_MIN_WIDTH,
                ui::WINDOW_MIN_HEIGHT,
            ))
            .with_resizable(true);
        let window = event_loop
            .create_window(attrs)
            .context("creating the record window")?;
        let hotkeys = Hotkeys::register()?;

        let camera_idx = self
            .cameras
            .iter()
            .position(|d| d.uid == self.camera_uid)
            .unwrap_or(0);
        let mic_idx = self
            .mics
            .iter()
            .position(|d| d.uid == self.audio_uid)
            .unwrap_or(0);
        // No fallback here, unlike camera/mic: an unrecognised display means
        // "No screen", never some other monitor picked on the user's behalf.
        let screen_idx = self
            .screen_uid
            .as_ref()
            .and_then(|uid| self.displays.iter().position(|d| &d.uid == uid));
        let attached = ui::attach_controls(
            &window,
            &self.cameras,
            camera_idx,
            &self.mics,
            mic_idx,
            &self.displays,
            screen_idx,
            Pair::ALL.iter().position(|p| *p == self.pair).unwrap_or(0),
            self.face_tracking_wanted(),
            self.mouse_tracking_wanted(),
            self.youtube_privacy,
            self.notes_pick.menu(),
            self.notes_pick.menu_index(),
            &self.notes_providers,
            self.notes_pick.provider_index(&self.notes_providers),
            &self.notes_prompt,
            &self.posts_prompt,
            &self.session.list_versions(),
            self.session.version,
            &crate::sessions::list_including(&self.session.root),
            &self
                .session
                .root
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_default(),
            &self.session.name().unwrap_or_default(),
        )?;

        println!("stream-recorder: ⌃⌥C start recording / new chapter, ⌃⌥T retake, ⌃⌥Q quit");
        Ok(Live {
            window,
            hotkeys,
            ui_rx: attached.rx,
            ui_tx: attached.tx,
            control_target: attached.target,
            overlay: None,
            snip: None,
            preview: attached.preview,
            notes: attached.notes,
            posts_form: attached.posts_form,
            schedule_form: attached.schedule_form,
            reflect_pane: attached.reflect_pane,
            thumbnail_pane: attached.thumbnail_pane,
            review_pane: attached.review_pane,
            edit_pane: attached.edit_pane,
            substack_pane: attached.substack_pane,
            blog_pane: attached.blog_pane,
            publish_pane: attached.publish_pane,
            video_brief_pane: attached.video_brief_pane,
            settings_pane: attached.settings_pane,
            layout: attached.layout,
        })
    }

    fn install_preview(&mut self) {
        let Some(conn) = self.connection.as_ref() else {
            return;
        };
        let Some(renderer) = self.renderer.as_ref() else {
            return;
        };
        let screen = self.screen.as_ref().map(|s| s.tap());
        let crops = self.screen_crops();
        let tracking = self.tracking();
        let pointing = self.pointing();
        match conn.install_preview(
            self.pair,
            renderer,
            screen,
            crops,
            tracking.as_ref(),
            pointing.as_ref(),
        ) {
            Ok(ports) => {
                if let Some(live) = self.live.as_ref() {
                    live.preview.bind(&ports);
                }
            }
            Err(e) => eprintln!("stream-recorder: preview graph failed to open: {e:#}"),
        }
    }

    /// Push the current recording state into the window's controls.
    fn sync_controls(&self) {
        let waiting = self
            .pending_pair
            .map(|pair| crate::layouts::Layout::get(pair, self.orientation).label());
        if let Some(live) = self.live.as_ref() {
            live.control_target.set_recording(
                self.router.as_ref().map(|r| r.current_chapter_number()),
                self.session.version,
                waiting.as_deref(),
            );
            live.control_target.set_stage_gates(&self.stages());
        }
    }

    /// Advance the recording clock and repaint it if it has anything new to say.
    ///
    /// Runs on every tick because the meter is live before recording starts —
    /// that is what lets the mic be checked and the gain set from this tab —
    /// but the labels themselves only repaint when their text changes.
    fn tick_timer(&mut self) {
        let snapshot = self
            .connection
            .as_ref()
            .map(|conn| conn.delegate.meter().snapshot());
        self.clock.tick(snapshot);
        // Before asking for a readout: `take_paint` records what it hands back as
        // painted, so calling it with no window to paint into would swallow the
        // first real one.
        if self.live.is_none() {
            return;
        }
        let Some(readout) = self.clock.take_paint() else {
            return;
        };
        if let Some(live) = self.live.as_ref() {
            live.control_target.set_timer(
                &readout.headline,
                &readout.detail,
                // The bar draws peak, not RMS — see `capture::level::Levels`.
                readout.peak_dbfs,
                readout.clipped,
                readout.recording,
            );
        }
    }

    /// What each pipeline button will accept right now. A handful of `stat`
    /// calls, so it is re-read on every control sync rather than cached — the
    /// files change under us whenever a worker thread finishes.
    fn stages(&self) -> crate::stage::Stages {
        crate::stage::Stages::read(
            &self.session,
            crate::stage::Busy {
                render: self.render_busy,
                distribute: self.distribute_busy,
                schedule: self.schedule_busy,
                publish: self.publish_busy,
                substack: self.substack_busy,
                blog: self.blog_busy,
            },
        )
    }

    fn apply(&mut self, action: Action, event_loop: &ActiveEventLoop) {
        match action {
            Action::Quit => {
                // The aside's writer hangs off the capture session; it has to
                // be finished before that session stops delivering.
                self.end_break(false);
                // Stop capture first so no more sample buffers race the
                // writers while the router finishes the final chapter.
                if let Some(ref conn) = self.connection {
                    unsafe { conn.session.stopRunning() };
                }
                if let Some(screen) = self.screen.take() {
                    println!(
                        "stream-recorder: screen capture wrote {} frames ({} skipped as idle)",
                        screen.delegate.frames_appended(),
                        screen.delegate.frames_skipped(),
                    );
                    if let Err(e) = screen.stop() {
                        eprintln!("stream-recorder: error stopping screen capture: {e:#}");
                    }
                }
                if let Some(ref mut router) = self.router {
                    if let Err(e) = router.stop() {
                        eprintln!("stream-recorder: error stopping recording: {:#}", e);
                    }
                }
                event_loop.exit();
            }
            Action::Stop => self.stop_recording(),
            Action::NewChapter => self.start_or_cut(),
            Action::Notes => self.build_notes(),
            Action::CopyTranscript => self.copy_transcript(),
            Action::Render => self.run_render(),
            Action::GenerateTitles => self.run_generate_titles(),
            Action::GeneratePosts => self.run_generate_posts(),
            Action::SavePosts => self.save_edited_posts(),
            Action::GenerateSubstack => self.run_generate_substack(),
            Action::EditSubstackPrompt => self.edit_substack_prompt(),
            Action::PublishBlog => self.run_publish_blog(),
            Action::WriteBlog => self.run_write_blog(),
            Action::PreviewBlog => self.run_preview_blog(),
            Action::EditBlogPrompt => self.edit_blog_prompt(),
            Action::RefreshBlogLibrary => self.refresh_blog_library(),
            Action::Distribute => self.run_distribute(),
            Action::SchedulePlan => self.run_schedule_plan(),
            Action::ScheduleApproveAll => self.approve_all_schedule(),
            Action::ScheduleQueue => self.run_schedule_queue(),
            Action::ScheduleClear => self.run_schedule_clear(),
            Action::YoutubeUpload => self.run_youtube_upload(),
            Action::ConnectYoutube => self.run_youtube_connect(),
            Action::PullAnalytics => self.run_analytics_pull(),
            Action::Reflect => self.run_reflect(),
            Action::CaptureFrame => self.capture_frame(),
            Action::CaptureFigure => self.capture_figure(),
            Action::WriteBlurbs => self.write_blurbs(),
            Action::GenerateThumbnails => self.run_thumbnails(),
            Action::DrawCard => self.draw_card(),
            Action::ApplyRewrites => self.run_apply_rewrites(),
            Action::CollectAllAnalytics => self.run_analytics_collect_all(),
            Action::NewVersion => self.new_version(),
            Action::NewProject => self.new_project(),
            Action::Retake => {
                if self.aside.is_some() {
                    // A retake throws the chapter away; a break is the one
                    // moment its figure is still being made. Finish that first.
                    self.set_figure_status(
                        "On a break — press ⌃⇧S to pick the take back up before retaking.",
                    );
                    return;
                }
                let Some(router) = self.router.as_mut() else {
                    eprintln!("stream-recorder: not recording yet — nothing to retake");
                    return;
                };
                match router.retake_chapter() {
                    Ok(()) => {
                        println!(
                            "stream-recorder: retaking chapter {}",
                            router.current_chapter_number()
                        );
                        // The take is gone, so its minutes are too — the clock
                        // must not carry a discarded chapter into the estimate.
                        self.clock.discard();
                    }
                    Err(e) => eprintln!("stream-recorder: error retaking: {:#}", e),
                }
            }
        }
    }

    /// The New Chapter action. While `router` is `None` this is what *starts*
    /// recording — it builds the first writer against the format the devices
    /// have settled on by the time a human reaches the button. Once recording,
    /// it's an ordinary chapter cut.
    /// Takes a layout choice, now or at the next chapter.
    ///
    /// Idle, it applies at once, and the take that follows records in it. Mid-take
    /// it is only remembered: the chapter's writer has already locked its output
    /// size, so applying it here would have to cut a chapter to do it, and a
    /// chapter boundary is not something a dropdown should invent.
    fn select_pair(&mut self, pair: Pair) {
        if self.router.is_none() {
            self.pending_pair = None;
            if pair != self.pair {
                self.pair = pair;
                self.apply_layout_change();
            }
            return;
        }
        // Back to what is recording leaves nothing to apply.
        self.pending_pair = (pair != self.pair).then_some(pair);
        self.sync_controls();
    }

    fn start_or_cut(&mut self) {
        // New Chapter on a break ends it and then cuts: the operator asked for
        // the next chapter, not for the take to stay paused.
        self.end_break(true);
        if self.router.is_some() {
            // A layout waiting on this cut turns it into a reopen: same one new
            // chapter either way, and the new one is the first in the new layout.
            if let Some(pair) = self.pending_pair.take() {
                self.pair = pair;
                self.apply_layout_change();
                if let Some(live) = self.live.as_ref() {
                    live.notes.next_slide();
                }
                return;
            }
            let router = self.router.as_mut().expect("checked above");
            match router.cut_chapter() {
                Ok(()) => {
                    println!(
                        "stream-recorder: chapter {} open",
                        router.current_chapter_number()
                    );
                    let opened = router.current_chapter_number();
                    report_chapter(self.clock.open(opened));
                    if let Some(live) = self.live.as_ref() {
                        live.notes.next_slide();
                    }
                }
                Err(e) => eprintln!("stream-recorder: error cutting chapter: {:#}", e),
            }
            self.sync_controls();
            return;
        }

        let Some(conn) = self.connection.as_ref() else {
            eprintln!("stream-recorder: no capture session to record from");
            return;
        };
        let delegate = conn.delegate.clone();
        let video_settings = conn.video_settings.clone();
        let audio_settings = conn.audio_settings.clone();
        let av_clock = match conn.sync_clock() {
            Ok(clock) => clock,
            Err(e) => {
                eprintln!("stream-recorder: error starting recording: {e:#}");
                return;
            }
        };
        let screen_track = match self.screen_track() {
            Ok(track) => track,
            Err(e) => {
                eprintln!("stream-recorder: screen capture unavailable: {e:#}");
                return;
            }
        };
        match Router::start_at(
            &self.session.dir,
            &delegate,
            video_settings,
            audio_settings,
            av_clock,
            screen_track,
            self.pair,
            self.next_chapter,
        ) {
            Ok(router) => {
                println!(
                    "stream-recorder: recording — chapter {} open in {}",
                    self.next_chapter,
                    self.session.dir.display()
                );
                self.router = Some(router);
                self.clock.open(self.next_chapter);
            }
            Err(e) => eprintln!("stream-recorder: error starting recording: {:#}", e),
        }
        self.sync_controls();
    }

    fn handle_ui_event(&mut self, event: UiEvent, event_loop: &ActiveEventLoop) {
        match event {
            UiEvent::Action(action) => self.apply(action, event_loop),
            UiEvent::CameraSelected(idx) => {
                if let Some(device) = self.cameras.get(idx) {
                    let camera = device.uid.clone();
                    let audio = self.audio_uid.clone();
                    if let Err(e) = self.switch_devices(camera, audio) {
                        eprintln!("stream-recorder: device switch failed: {:#}", e);
                    }
                }
            }
            UiEvent::MicSelected(idx) => {
                if let Some(device) = self.mics.get(idx) {
                    let camera = self.camera_uid.clone();
                    let audio = device.uid.clone();
                    if let Err(e) = self.switch_devices(camera, audio) {
                        eprintln!("stream-recorder: device switch failed: {:#}", e);
                    }
                }
            }
            UiEvent::ScreenSelected(idx) => {
                let uid = idx.and_then(|i| self.displays.get(i)).map(|d| d.uid.clone());
                if let Err(e) = self.select_screen(uid) {
                    eprintln!("stream-recorder: saving the screen choice failed: {:#}", e);
                }
            }
            UiEvent::PairSelected(idx) => {
                if let Some(pair) = Pair::ALL.get(idx).copied() {
                    self.select_pair(pair);
                }
            }
            UiEvent::FaceTrackToggled(on) => {
                self.set_face_tracking(on);
                self.sync_face_control();
            }
            UiEvent::MouseTrackToggled(on) => self.set_mouse_tracking(on),
            UiEvent::FaceTrackReady(built) => self.face_tracker_ready(built),
            UiEvent::YoutubePrivacySelected(idx) => self.select_youtube_privacy(idx),
            UiEvent::ModelSelected(idx) => self.select_model(idx),
            UiEvent::ProviderSelected(idx) => self.select_provider(idx),
            UiEvent::PromptChanged(text) => {
                if text != self.notes_prompt {
                    self.notes_prompt = text;
                    self.save_notes_choice();
                }
            }
            UiEvent::PostsModelSelected(idx) => self.select_posts_model(idx),
            UiEvent::PostsProviderSelected(idx) => self.select_posts_provider(idx),
            UiEvent::PostsPromptChanged(text) => {
                if text != self.posts_prompt {
                    self.posts_prompt = text;
                    self.save_posts_prompt();
                }
            }
            UiEvent::VersionSelected(n) => self.switch_version(n),
            UiEvent::ProjectSelected(index) => self.switch_project(index),
            UiEvent::ValidateRewrite(index) => self.run_validate_rewrite(index),
            UiEvent::WebApprove { index, value } => self.set_rewrite_approval(index, value),
            UiEvent::SaveBrief(fields) => self.save_brief(fields),
            UiEvent::SaveSettings(fields) => self.save_settings(fields),
            UiEvent::TestSettings(service) => self.test_settings(&service),
            UiEvent::SettingsTested(outcome) => self.settings_tested(&outcome),
            UiEvent::SaveCard(fields) => { self.save_card(&fields); },
            UiEvent::GenerateArtwork(fields) => { if self.save_card(&fields) { self.draw_card(); } },
            UiEvent::ImportPortrait(data) => {
                self.set_thumbnail_status("Importing your photo…");
                crate::thumbnail::spawn_portrait(self.session.root.clone(), data, self.thumbnail_tx.clone());
            },
            UiEvent::SaveVideoBrief { fields, apply } => self.save_video_brief(&fields, apply),
            UiEvent::GenerateVideoCopy(fields) => self.generate_video_copy(&fields),
            UiEvent::SaveYoutube(fields) => {
                let metadata = crate::publish::metadata::Metadata {
                    title: fields.get("title").cloned().unwrap_or_default(),
                    description: fields.get("description").cloned().unwrap_or_default(),
                };
                match crate::publish::metadata::save(&self.session, &metadata) {
                    Ok(()) => {
                        self.update_publish_summary();
                        self.sync_controls();
                        if let Some(live) = &self.live { live.control_target.set_publish_status("Video details saved."); }
                    }
                    Err(err) => if let Some(live) = &self.live { live.control_target.set_publish_status(&format!("{err:#}")); },
                }
            }
            UiEvent::SelectThumbnail(id) => self.select_thumbnail(&id),
            UiEvent::BlogAuthorSelected(id) => self.select_blog_author(&id),
            UiEvent::BlogCategorySelected(id) => self.select_blog_category(&id),
            UiEvent::ThumbnailModelSelected(id) => self.select_thumbnail_model(&id),
            UiEvent::ToggleReference { name, value } => self.toggle_reference(&name, value),
            UiEvent::AddReference { name, data } => self.add_reference(&name, &data),
            UiEvent::RemoveReference(name) => self.remove_reference(&name),
            UiEvent::ProjectNameChanged(name) => self.rename_project(&name),
            UiEvent::CopyText(text) => self.copy_text(&text),
            UiEvent::OpenChapter(chapter) => self.open_edit_chapter(chapter),
            UiEvent::SaveEdit { chapter, spans } => self.save_edit_spans(chapter, &spans),
            UiEvent::ApplyEdit(chapter) => self.apply_edit(chapter),
            UiEvent::ResetEdit(chapter) => self.reset_edit(chapter),
            UiEvent::ToggleRegions => self.toggle_regions(),
            UiEvent::RegionPlaced { orientation, rect } => {
                self.region_placed(orientation, rect)
            }
            UiEvent::FigureSnipped { rect } => self.figure_snipped(rect),
            UiEvent::FigureSnipCancelled => self.figure_snip_cancelled(),
        }
    }

    /// Opens the project at `index` in the picker's list.
    fn switch_project(&mut self, index: usize) {
        let projects = crate::sessions::list_including(&self.session.root);
        let Some(chosen) = projects.get(index) else {
            return;
        };
        if chosen.root == self.session.root {
            return;
        }
        // A take in flight belongs to the project it started in.
        self.finish_open_chapter();
        match crate::session::Session::open_root(chosen.root.clone()) {
            Ok(next) => self.adopt_session(next),
            Err(err) => eprintln!(
                "stream-recorder: could not open project {}: {err:#}",
                chosen.title()
            ),
        }
    }

    /// Starts a fresh project folder and switches to it.
    fn new_project(&mut self) {
        self.finish_open_chapter();
        match crate::session::Session::create() {
            Ok(next) => self.adopt_session(next),
            Err(err) => eprintln!("stream-recorder: could not start a new project: {err:#}"),
        }
    }

    /// Names the open project. The folder keeps its timestamp — only the label moves.
    fn rename_project(&mut self, name: &str) {
        if let Err(err) = self.session.set_name(name) {
            eprintln!("stream-recorder: could not rename project: {err:#}");
            return;
        }
        self.refresh_project_controls();
    }

    /// Repaints the whole "which project, which version" strip: the picker, the
    /// name field, the version popup and the window title.
    ///
    /// All four together, because they answer one question between them. Leaving
    /// the version popup out is how a resumed project came up reading as though
    /// no version were open while every tab was reading `v1`.
    fn refresh_project_controls(&self) {
        let Some(live) = self.live.as_ref() else { return };
        let folder = self
            .session
            .root
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default();
        live.control_target
            .set_projects(&crate::sessions::list_including(&self.session.root), &folder);
        live.control_target
            .set_project_name(&self.session.name().unwrap_or_default());
        live.control_target
            .set_versions(&self.session.list_versions(), self.session.version);
        live.window.set_title(&self.window_title());
    }

    /// Both halves of where you are, always. The title used to say either the
    /// project or the version depending on which button you last pressed, so it
    /// read as changing on its own.
    fn window_title(&self) -> String {
        match self.session.version {
            Some(v) => format!("stream-recorder · {} · v{v}", self.session.title()),
            None => format!("stream-recorder · {}", self.session.title()),
        }
    }

    /// The microphone the capture session is on, by the name the dropdown shows.
    fn mic_name(&self) -> String {
        self.mics
            .iter()
            .find(|d| d.uid == self.audio_uid)
            .map_or_else(|| self.audio_uid.clone(), |d| d.name.clone())
    }

    /// Points every version-scoped view at whatever `self.session` is now.
    ///
    /// Notes, both forms, all three summaries, the version popup and the title —
    /// everything that means something different in v2 than it did in v1. The
    /// project-scoped views are deliberately absent: the picker, analytics,
    /// reflect and thumbnails do not move when the version does.
    ///
    /// One function because the three callers kept drifting apart. New Version
    /// was missing the titles and posts reloads, so a fresh version opened
    /// showing the previous one's copy — and Render, which saves the titles form
    /// before it runs, then wrote that copy into the new version as though it had
    /// been generated there.
    fn refresh_version_views(&mut self) {
        self.update_video_brief("Render video writes the title and description from the transcript and these notes.");
        if let Some(live) = self.live.as_ref() {
            if let Some(html) = crate::notes::existing_html(&self.session) {
                live.notes.load(&html);
                live.notes.reset_slide();
            } else {
                live.notes.show_placeholder();
            }
            live.window.set_title(&self.window_title());
            live.control_target
                .set_versions(&self.session.list_versions(), self.session.version);
            if let Ok(manifest) = crate::posts::load_manifest(&self.session.posts_dir()) {
                live.posts_form.show(&manifest);
                live.control_target.set_posts_status("Loaded saved posts.");
                self.posts_manifest = Some(manifest);
            } else {
                live.posts_form.show_empty();
                live.control_target
                    .set_posts_status("Ready to generate social copy.");
                self.posts_manifest = None;
            }
        }
        self.update_render_summary();
        self.update_review_view();
        self.update_substack_view();
        self.update_blog_view();
        self.update_distribute_summary();
        self.update_publish_summary();
        self.update_schedule_summary();
        self.sync_controls();
    }

    /// The chapter a recording would open next: one past the highest already
    /// closed in this take's folder.
    fn resume_chapter_number(&self) -> u32 {
        crate::notes::closed_chapter_numbers(&self.session.dir)
            .into_iter()
            .max()
            .unwrap_or(0)
            + 1
    }

    /// Points every tab at `next`. Anything missed here would keep showing the
    /// previous project's work.
    fn adopt_session(&mut self, next: crate::session::Session) {
        self.session = next;
        self.next_chapter = self.resume_chapter_number();
        self.refresh_version_views();
        self.refresh_project_controls();
        self.last_report = None;
        self.refresh_analytics_view();
        self.update_reflect_view();
        self.update_thumbnail_view();
    }

    fn switch_version(&mut self, n: u32) {
        if self.session.version == Some(n) {
            return;
        }
        self.finish_open_chapter();
        match self.session.open_version(n) {
            Ok(next) => {
                self.session = next;
                self.next_chapter = self.resume_chapter_number();
                self.refresh_version_views();
            }
            Err(e) => eprintln!("stream-recorder: could not open v{n}: {e:#}"),
        }
    }

    fn stop_recording(&mut self) {
        if self.router.is_none() {
            eprintln!("stream-recorder: not recording");
            return;
        }
        self.finish_open_chapter();
        println!("stream-recorder: recording stopped");
    }

    fn finish_open_chapter(&mut self) {
        // A break ends with the chapter and does not resume it: the file closes
        // at the pause point, with the explanation kept beside the figure.
        self.end_break(false);
        if let Some(mut router) = self.router.take() {
            self.next_chapter = router.current_chapter_number() + 1;
            println!(
                "stream-recorder: finishing chapter {} so the job can include it",
                router.current_chapter_number()
            );
            if let Err(e) = router.stop() {
                eprintln!("stream-recorder: error finishing take: {e:#}");
            }
            report_chapter(self.clock.close());
            self.sync_controls();
        }
        self.adopt_pending_layout();
    }

    /// Nothing is recording, so a layout that was waiting for a chapter can stop
    /// waiting: there is no writer left to hold its size, and the preview should
    /// show what the next take will record in.
    fn adopt_pending_layout(&mut self) {
        if self.router.is_some() {
            return;
        }
        if let Some(pair) = self.pending_pair.take() {
            if pair != self.pair {
                self.pair = pair;
                self.apply_layout_change();
            }
        }
    }

    fn build_notes(&mut self) {
        self.finish_open_chapter();
        let title = self
            .session
            .name()
            .unwrap_or_else(|| "Speaking notes".into());
        if let Some(live) = self.live.as_ref() {
            self.notes_prompt = live.control_target.prompt_text();
        }
        println!("stream-recorder: building notes…");
        crate::notes::spawn_notes(
            self.session.clone(),
            title,
            self.notes_pick.model().to_string(),
            self.notes_pick.provider().map(str::to_string),
            (!self.notes_prompt.trim().is_empty()).then(|| self.notes_prompt.clone()),
            self.notes_tx.clone(),
        );
    }

    fn run_render(&mut self) {
        // The button is switched off in both these cases, but a hotkey and a
        // queued click both reach here without passing the button.
        if self.render_busy {
            self.set_render_status("A render is already running…");
            return;
        }
        let stages = self.stages();
        if let Some(reason) = stages.render.missing() {
            self.set_render_status(reason);
            return;
        }
        self.finish_open_chapter();
        println!("stream-recorder: cutting disfluencies and rendering…");
        self.render_busy = true;
        self.set_render_status("Cutting disfluencies…");
        crate::edit::spawn_render(self.session.clone(), self.render_tx.clone());
        self.sync_controls();
    }

    fn set_render_status(&self, text: &str) {
        if let Some(live) = self.live.as_ref() {
            live.control_target.set_render_status(text);
        }
    }

    fn run_generate_titles(&mut self) {
        let stages = self.stages();
        if let Some(reason) = stages.titles.missing() {
            self.set_render_status(reason);
            return;
        }
        self.finish_open_chapter();
        self.set_render_status("Generating titles…");
        crate::titles::spawn_generate_titles(
            self.session.clone(),
            self.notes_pick.model().to_string(),
            self.notes_pick.provider().map(str::to_string),
            self.titles_tx.clone(),
        );
    }

    fn run_generate_posts(&mut self) {
        let stages = self.stages();
        if let Some(reason) = stages.posts.missing() {
            if let Some(live) = self.live.as_ref() {
                live.control_target.set_posts_status(reason);
            }
            return;
        }
        self.finish_open_chapter();
        println!("stream-recorder: generating social media posts…");
        if let Some(live) = self.live.as_ref() {
            // Pressing Generate does not move focus out of the prompt, so the
            // text area's own end-of-editing callback has not fired yet. Take
            // what is on screen and keep it: the run about to happen is exactly
            // the guidance worth having back at the next launch.
            let typed = live.control_target.posts_prompt_text();
            let changed = typed != self.posts_prompt;
            self.posts_prompt = typed;
            if changed {
                self.save_posts_prompt();
            }
            live.control_target.set_posts_status("Generating posts for 8 platforms…");
        }
        crate::posts::spawn_generate_posts(
            self.session.clone(),
            self.posts_pick.model().to_string(),
            self.posts_pick.provider().map(str::to_string),
            (!self.posts_prompt.trim().is_empty()).then(|| self.posts_prompt.clone()),
            self.posts_tx.clone(),
        );
    }

    fn save_edited_posts(&mut self) {
        let Some(live) = self.live.as_ref() else { return };
        // Keep the prompt attribution the generated manifest carried; an edit
        // changes the words, not which prompt wrote the first draft.
        let attribution = self
            .posts_manifest
            .as_ref()
            .map(|m| (m.prompt_version, m.prompt_hash.clone()))
            .unwrap_or((None, String::new()));
        let manifest = live.posts_form.collect(self.session.version, attribution);
        let posts_dir = self.session.posts_dir();
        match crate::posts::save_manifest(&posts_dir, &manifest) {
            Ok(path) => {
                self.posts_manifest = Some(manifest);
                live.control_target.set_posts_status(&format!("Saved posts → {}", path.display()));
            }
            Err(e) => {
                live.control_target.set_posts_status(&format!("Failed to save posts: {e:#}"));
            }
        }
    }

    /// Writes the Substack notes. A post step whose output nobody sends.
    fn run_generate_substack(&mut self) {
        if self.substack_busy {
            self.set_substack_status("Notes are already being written…");
            return;
        }
        let stages = self.stages();
        if let Some(reason) = stages.substack.missing() {
            self.set_substack_status(reason);
            return;
        }
        // The open chapter has no transcript until it is closed, and an essay
        // written without the last thing said is missing the ending.
        self.finish_open_chapter();
        self.substack_busy = true;
        self.set_substack_status("Reading the longform…");
        crate::substack::spawn_generate(
            self.session.clone(),
            self.posts_pick.model().to_string(),
            self.posts_pick.provider().map(str::to_string),
            self.substack_tx.clone(),
        );
        self.sync_controls();
    }

    /// Seeds the standing prompt from the builtin if it is not there, then opens
    /// it. The one place the house voice gets changed.
    fn edit_substack_prompt(&mut self) {
        match crate::substack::edit_prompt() {
            Ok(path) => self.set_substack_status(&format!("Editing {}", path.display())),
            Err(err) => self.set_substack_status(&format!("Could not open the prompt: {err:#}")),
        }
        // The pane names the file and labels the version, and both can have just
        // changed — seeding turns "v0 (builtin)" into an overlay.
        self.update_substack_view();
    }

    /// Puts a pane's words on the system pasteboard.
    ///
    /// Deliberately silent on success: the button is beside the text it copied,
    /// so a status line saying so is noise. A *failure* is worth a line, because
    /// the alternative is pasting whatever was on the clipboard before.
    fn copy_text(&self, text: &str) {
        if !crate::ui::copy_to_pasteboard(text) {
            self.set_substack_status("Could not reach the pasteboard.");
        }
    }

    /// Copy Transcript: every transcribed chapter, chapter-headed, as one
    /// document.
    ///
    /// Read off disk on the press rather than held anywhere, like every pane in
    /// this app — a copy cached at launch would hand over a transcript that
    /// predates the last three chapters, which is worse than no button because
    /// it looks like it worked.
    ///
    /// Reports to the console rather than a status line because the Draft tab
    /// has none, and because the interesting case is not the success but the
    /// two silences: nothing recorded, and recorded-but-not-transcribed. Those
    /// look identical from the button.
    fn copy_transcript(&self) {
        let Some(text) = crate::notes::full_transcript(&self.session.dir) else {
            let closed = crate::notes::closed_chapter_numbers(&self.session.dir).len();
            if closed == 0 {
                println!(
                    "stream-recorder: nothing to copy — no chapters recorded yet in {}.",
                    self.session.dir.display(),
                );
            } else {
                println!(
                    "stream-recorder: nothing to copy — {closed} chapter(s) recorded, but \
                     none have transcribed yet. Transcripts land a moment after a chapter \
                     closes; a silent take never gets one.",
                );
            }
            return;
        };
        let chapters = crate::notes::collect_completed(&self.session.dir).len();
        self.copy_text(&text);
        println!(
            "stream-recorder: copied {chapters} chapter(s), {} words, to the pasteboard.",
            text.split_whitespace().count(),
        );
    }

    fn set_substack_status(&self, text: &str) {
        if let Some(live) = self.live.as_ref() {
            live.control_target.set_substack_status(text);
        }
    }

    fn drain_substack(&mut self) {
        // No early return on a missing window: the terminal events are what
        // clear `substack_busy`, and dropping one would wedge the button.
        let events: Vec<_> = self.substack_rx.try_iter().collect();
        for event in events {
            match event {
                crate::substack::SubstackEvent::Status(msg) => self.set_substack_status(&msg),
                crate::substack::SubstackEvent::Ready(path, notes) => {
                    self.substack_busy = false;
                    self.update_substack_view();
                    self.set_substack_status(&format!(
                        "{} section(s), {} beat(s) → {}",
                        notes.sections.len(),
                        notes.beat_count(),
                        path.display()
                    ));
                    self.sync_controls();
                }
                crate::substack::SubstackEvent::Failed(msg) => {
                    self.substack_busy = false;
                    self.set_substack_status(&msg);
                    self.sync_controls();
                }
            }
        }
    }

    /// Writes the article to disk and stops there.
    ///
    /// Shares `blog_busy` with the publish rather than having its own flag: both
    /// write `article.json`, so two of them running would race over the same
    /// file and the loser's draft would be the one that published.
    fn run_write_blog(&mut self) {
        if self.blog_busy {
            self.set_blog_status("Already working on the article…");
            return;
        }
        // Deliberately *not* gated on the YouTube upload or the thumbnail, which
        // the publish is. Neither is needed to write prose, and having to
        // finish the whole pipeline before you can read a draft is what made
        // reading the draft something nobody did.
        self.finish_open_chapter();
        self.blog_busy = true;
        crate::blog::spawn_write(
            self.session.clone(),
            self.posts_pick.model().to_string(),
            self.posts_pick.provider().map(str::to_string),
            self.blog_tx.clone(),
        );
        self.sync_controls();
    }

    /// Opens the draft on disk as a local page, and writes the create body out
    /// beside it. Both paths are reported: the JSON is the half you cannot see
    /// by looking at the page.
    fn run_preview_blog(&mut self) {
        match crate::blog::preview(&self.session) {
            Ok(preview) => {
                let body = preview
                    .body
                    .map(|path| format!(" · {}", path.display()))
                    .unwrap_or_default();
                self.set_blog_status(&format!("Preview: {}{body}", preview.page.display()));
            }
            Err(err) => self.set_blog_status(&format!("{err:#}")),
        }
    }

    /// Writes the article and posts it to the video blog at `/blog`.
    ///
    /// The only stage that creates a public page from generated prose, so the
    /// gate is checked here as well as on the button: the reasons it can refuse —
    /// nothing on YouTube, no thumbnail, no credentials — are all things that
    /// would otherwise fail after an LLM call and a thumbnail upload.
    fn run_publish_blog(&mut self) {
        if self.blog_busy {
            self.set_blog_status("The blog post is already going up…");
            return;
        }
        let stages = self.stages();
        if let Some(reason) = stages.blog.missing() {
            self.set_blog_status(reason);
            return;
        }
        // The open chapter has no transcript until it is closed, and an article
        // written without the last thing said is missing its ending.
        self.finish_open_chapter();
        self.blog_busy = true;
        self.set_blog_status("Reading the longform…");
        crate::blog::spawn_publish(
            self.session.clone(),
            self.posts_pick.model().to_string(),
            self.posts_pick.provider().map(str::to_string),
            self.blog_tx.clone(),
        );
        self.sync_controls();
    }

    /// Seeds the standing article prompt from the builtin if it is not there,
    /// then opens it.
    fn edit_blog_prompt(&mut self) {
        match crate::blog::edit_prompt() {
            Ok(path) => self.set_blog_status(&format!("Editing {}", path.display())),
            Err(err) => self.set_blog_status(&format!("Could not open the prompt: {err:#}")),
        }
        // Seeding turns "v0 (builtin)" into an overlay, and the pane says which.
        self.update_blog_view();
    }

    /// Re-reads the CMS's author and category lists into the cache.
    ///
    /// Shares `blog_busy` with the publish rather than having a flag of its own:
    /// both talk to the same CMS, and picking a byline halfway through a post
    /// that is already being written would not take effect anyway.
    fn refresh_blog_library(&mut self) {
        if self.blog_busy {
            self.set_blog_status("Already talking to Strapi…");
            return;
        }
        self.blog_busy = true;
        crate::blog::spawn_refresh_library(self.blog_tx.clone());
        self.sync_controls();
    }

    /// Records the byline. Repaints, unlike [`Self::select_thumbnail`]: the
    /// choice gates the Publish button, so the bar above the dropdown has to
    /// agree with it immediately.
    fn select_blog_author(&mut self, id: &str) {
        match crate::blog::select_author(id) {
            Ok(message) => self.set_blog_status(&message),
            Err(err) => self.set_blog_status(&format!("Could not save the author: {err:#}")),
        }
        self.update_blog_view();
        self.sync_controls();
    }

    fn select_blog_category(&mut self, id: &str) {
        match crate::blog::select_category(id) {
            Ok(message) => self.set_blog_status(&message),
            Err(err) => self.set_blog_status(&format!("Could not save the category: {err:#}")),
        }
        self.update_blog_view();
    }

    fn set_blog_status(&self, text: &str) {
        if let Some(live) = self.live.as_ref() {
            live.control_target.set_blog_status(text);
        }
    }

    fn drain_blog(&mut self) {
        // No early return on a missing window: the terminal events are what clear
        // `blog_busy`, and dropping one would wedge the button.
        let events: Vec<_> = self.blog_rx.try_iter().collect();
        for event in events {
            match event {
                crate::blog::BlogEvent::Status(msg) => self.set_blog_status(&msg),
                crate::blog::BlogEvent::Ready(post) => {
                    self.blog_busy = false;
                    self.update_blog_view();
                    // The warning is surfaced rather than logged: a post that
                    // went up with no byline is live and wrong, and the status
                    // line is the only place anyone would notice.
                    // A draft has no page yet: /blog/{slug} 404s until
                    // someone presses publish in the CMS, so point at the one
                    // URL that opens.
                    let where_it_is = match post.published {
                        true => post.url.clone(),
                        false => format!("Draft — review and publish at {}", post.admin_url),
                    };
                    match post.warning.as_deref() {
                        Some(warning) => {
                            self.set_blog_status(&format!("{where_it_is} — {warning}"))
                        }
                        None => self.set_blog_status(&where_it_is),
                    }
                    self.sync_controls();
                }
                crate::blog::BlogEvent::Written(path) => {
                    self.blog_busy = false;
                    // Repaint first: the blocks and the figure count are what
                    // was just written, and they are what tells you whether to
                    // preview it or rewrite it.
                    self.update_blog_view();
                    self.set_blog_status(&format!(
                        "Draft written to {} — press Preview to read it. Nothing uploaded.",
                        path.display()
                    ));
                    self.sync_controls();
                }
                crate::blog::BlogEvent::LibraryReady { authors, categories } => {
                    self.blog_busy = false;
                    // Repaint before the status line: the dropdowns are the
                    // point of the refresh, and the count is only a receipt.
                    self.update_blog_view();
                    self.set_blog_status(&format!(
                        "Strapi: {authors} author(s), {categories} category(ies)."
                    ));
                    self.sync_controls();
                }
                crate::blog::BlogEvent::Failed(msg) => {
                    self.blog_busy = false;
                    self.set_blog_status(&msg);
                    self.sync_controls();
                }
            }
        }
    }

    /// Runs the YouTube OAuth flow in a worker, because it opens a browser and
    /// waits for a human to come back from it.
    ///
    /// On the publish channel, so its progress lands on the same status line the
    /// upload narrates to and a press during it is refused by the same flag.
    fn run_youtube_connect(&mut self) {
        if self.publish_busy {
            self.set_publish_status("A YouTube job is already running…");
            return;
        }
        self.publish_busy = true;
        self.set_publish_status("Opening a browser to connect YouTube…");
        let tx = self.publish_tx.clone();
        if let Err(err) = std::thread::Builder::new()
            .name("youtube-connect".into())
            .spawn(move || match crate::publish::youtube::connect() {
                Ok(()) => {
                    let _ = tx.send(crate::publish::PublishEvent::Connected);
                }
                Err(err) => {
                    // Printed as well as sent, matching the upload path in
                    // `publish::run`. The event only reaches the YouTube tab's
                    // status line, so a connect that refused the wrong channel
                    // or timed out left the terminal silent and the operator
                    // pressing Upload against a store that never got a token.
                    eprintln!("stream-recorder: youtube connect failed: {err:#}");
                    let _ = tx.send(crate::publish::PublishEvent::Failed(format!(
                        "Connect failed: {err:#}"
                    )));
                }
            })
        {
            self.publish_busy = false;
            self.set_publish_status(&format!("Could not start the connect job: {err}"));
        }
        self.sync_controls();
    }

    fn run_distribute(&mut self) {
        if self.distribute_busy {
            self.set_distribute_status("An upload is already running…");
            return;
        }
        let stages = self.stages();
        if let Some(reason) = stages.distribute.missing() {
            self.set_distribute_status(reason);
            return;
        }
        self.distribute_busy = true;
        self.set_distribute_status("Uploading renders to S3…");
        crate::distribute::spawn_distribute(self.session.clone(), self.distribute_tx.clone());
        self.sync_controls();
    }

    fn set_distribute_status(&self, text: &str) {
        if let Some(live) = self.live.as_ref() {
            live.control_target.set_distribute_status(text);
        }
    }

    /// Step 1: read posts + links + channels and write schedule.json. No posting.
    fn run_schedule_plan(&mut self) {
        if self.schedule_busy {
            self.set_schedule_status("A schedule job is already running…");
            return;
        }
        let stages = self.stages();
        if let Some(reason) = stages.plan.missing() {
            self.set_schedule_status(reason);
            return;
        }
        self.schedule_busy = true;
        self.set_schedule_status("Building the Buffer plan…");
        crate::schedule::spawn_plan(self.session.clone(), self.schedule_tx.clone());
        self.sync_controls();
    }

    /// Step 2: queue exactly what the saved plan says, never a fresh plan.
    /// Ticks every sendable row and writes it to the saved plan, so the approval
    /// survives a restart the same way a hand-ticked one does.
    fn run_analytics_collect_all(&mut self) {
        if self.analytics_busy {
            self.set_analytics_status("An analytics pull is already running…");
            return;
        }
        self.analytics_busy = true;
        self.set_analytics_status("Sweeping every project that owes samples…");
        crate::analytics::spawn_collect_all(self.analytics_tx.clone());
    }

    /// Recomputes the work queue and repaints the tab.
    ///
    /// Every count is read from local files, so this is cheap enough to call on a
    /// timer — it never asks Buffer anything.
    fn refresh_analytics_view(&mut self) {
        let queue = crate::analytics::queue::scan(
            &crate::sessions::list(),
            crate::analytics::due::now_unix(),
        );
        let Some(live) = self.live.as_ref() else { return };
        live.control_target
            .set_analytics_badge(&queue.badge("Analytics"));
        let mut body = queue.as_markdown();
        match self.last_report.as_deref() {
            Some(report) => body.push_str(report),
            None => body.push_str(
                "Press Pull & Report for this project, or Collect All Due to sweep \
                 every project that owes samples.",
            ),
        }
        live.control_target.set_analytics_info(&body);
        self.due_checked = Some(Instant::now());
    }

    /// Grabs the current camera frame as the thumbnail's subject.
    /// Grabs the camera and, when a layout has one, the screen alongside it.
    ///
    /// Both, because they answer different halves of the same question: the
    /// camera is who the thumbnail is of, and the screen is what the video is
    /// about. Taken at the same instant so the pair actually belongs together —
    /// capturing them separately would let the screen drift onto a different
    /// slide from the expression that was caught with it.
    fn capture_frame(&mut self) {
        let Some(live) = self.live.as_ref() else { return };
        let Some(frame) = live.preview.latest_camera_frame() else {
            self.set_thumbnail_status("No camera frame yet — is the camera running?");
            return;
        };
        let camera = crate::thumbnail::still::write(&self.session.root, frame.pixels.get());
        // The tap the compositor reads, so this is the screen exactly as the
        // layout sees it. `None` is the ordinary case for a talking-head layout,
        // not a failure worth reporting.
        let screen = self
            .screen
            .as_ref()
            .and_then(|screen| screen.tap().latest())
            .map(|frame| {
                crate::thumbnail::still::write_screen(&self.session.root, frame.pixels.get())
            });

        let message = match (camera, screen) {
            (Err(err), _) => format!("Capture failed: {err:#}"),
            (Ok(path), None) => format!("Captured {}", file_name(&path)),
            (Ok(path), Some(Ok(shot))) => format!(
                "Captured {} and the screen ({})",
                file_name(&path),
                file_name(&shot)
            ),
            (Ok(path), Some(Err(err))) => {
                format!("Captured {} — the screen grab failed: {err:#}", file_name(&path))
            }
        };
        self.set_thumbnail_status(&message);
        self.update_thumbnail_view();
    }

    fn run_thumbnails(&mut self) {
        if self.thumbnail_busy {
            self.set_thumbnail_status("A thumbnail job is already running…");
            return;
        }
        let config = crate::config::load();
        if !config.thumbnail.enabled {
            self.set_thumbnail_status("Thumbnails are switched off in config.");
            return;
        }
        self.thumbnail_busy = true;
        self.set_thumbnail_status("Generating thumbnail candidates…");
        // Only the chosen one. The list is the menu, not the batch.
        let chosen: Vec<_> = config
            .thumbnail
            .models
            .iter()
            .filter(|model| model.id == config.thumbnail.model)
            .cloned()
            .collect();
        let chosen = match chosen.is_empty() {
            // A model id that is no longer in the menu still draws: the config is
            // the authority on what to run, the menu only on what to offer.
            true => vec![crate::thumbnail::image::ModelSpec {
                id: config.thumbnail.model.clone(),
                label: config.thumbnail.model.clone(),
            }],
            false => chosen,
        };
        crate::thumbnail::spawn(
            self.session.clone(),
            chosen,
            config.thumbnail.candidates_per_model,
            self.thumbnail_tx.clone(),
        );
    }

    fn save_brief(&mut self, fields: std::collections::BTreeMap<String, String>) {
        // Absent is the ordinary starting state now, not an error: nothing drafts
        // a brief, so the first save is what creates one.
        let previous = crate::thumbnail::load_brief(&self.session);
        let mut brief = previous.clone().unwrap_or_default().brief;
        let take = |key: &str, current: &str| -> String {
            fields.get(key).cloned().unwrap_or_else(|| current.to_string())
        };
        brief.title = take("title", &brief.title);
        brief.description = take("description", &brief.description);

        let saved =
            crate::thumbnail::pane::edited(brief, crate::schedule::ledger::now_rfc3339());
        // A human typed this, so it becomes the starting point for the next
        // project. Only here: the stage's own save must not rewrite global config.
        crate::thumbnail::remember_brief(&saved.brief);
        // Whether the edit reached anything the images are keyed on. Compared on
        // the finished prompt, so a change to the instructions counts exactly as a
        // change to a field does. Coming back identical is worth saying out loud:
        // Regenerate would then skip every candidate, and the pair of steps would
        // look broken together.
        let unchanged = previous
            .as_ref()
            .is_some_and(|before| before.prompt() == saved.prompt());
        match crate::thumbnail::save_brief(&self.session, &saved) {
            Ok(()) => self.set_thumbnail_status(match unchanged {
                true => "Brief saved, but nothing changed — Regenerate would draw the same picture.",
                false => "Brief saved — press Regenerate Images to draw it.",
            }),
            Err(err) => self.set_thumbnail_status(&format!("Could not save the brief: {err:#}")),
        }
        self.update_thumbnail_view();
    }

    /// Records the choice without repainting.
    ///
    /// The pane has already moved the highlight, and a repaint would reload the
    /// document and scroll the reader back to the top. Only a failure repaints —
    /// to correct a pane now showing something the disk does not agree with.
    fn select_thumbnail(&mut self, id: &str) {
        if let Err(err) = crate::thumbnail::activate(&self.session, id) {
            self.set_thumbnail_status(&format!("Could not select: {err:#}"));
            self.update_thumbnail_view();
            return;
        }
        self.set_thumbnail_status(&format!("{id} is now the thumbnail."));
        // Not a repaint — `sync_controls` touches the control bar and the gates,
        // never a pane document, so the scroll position this function protects
        // is safe. The Blog stage waits on a *chosen* thumbnail, and choosing
        // one is exactly what just happened.
        self.sync_controls();
    }

    fn toggle_reference(&mut self, name: &str, value: bool) {
        let Ok(root) = crate::thumbnail::references::library_root() else { return };
        // As with selection: the tile already moved, so only a refusal — hitting
        // the active cap — needs the pane corrected from disk.
        if let Err(err) = crate::thumbnail::references::set_active(&root, name, value) {
            self.set_thumbnail_status(&format!("{err:#}"));
            self.update_thumbnail_view();
        }
    }

    /// Remembers which image model draws. Config, not session state: the choice
    /// is about how you want thumbnails made, not about this one project.
    fn select_thumbnail_model(&mut self, id: &str) {
        let mut config = crate::config::load();
        if config.thumbnail.model == id {
            return;
        }
        let label = config
            .thumbnail
            .models
            .iter()
            .find(|model| model.id == id)
            .map(|model| model.label.clone())
            .unwrap_or_else(|| id.to_string());
        config.thumbnail.model = id.to_string();
        match crate::config::save(&config) {
            Ok(()) => self.set_thumbnail_status(&format!("Drawing with {label}.")),
            Err(err) => self.set_thumbnail_status(&format!("Could not save the model: {err:#}")),
        }
        self.update_thumbnail_view();
    }

    fn add_reference(&mut self, name: &str, data: &str) {
        self.set_thumbnail_status(&format!("Reading {name}…"));
        crate::thumbnail::spawn_prepare_reference(
            name.to_string(),
            data.to_string(),
            self.thumbnail_tx.clone(),
        );
    }

    /// Files the prepared image and switches it on.
    ///
    /// Dropping an image into a box labelled "add them to the style library" is
    /// asking for it to be used, so it goes on rather than waiting to be clicked.
    /// Failing the cap is worth saying out loud and is not a failure to add: the
    /// reference is in the library either way.
    fn store_reference(&mut self, name: &str, bytes: &[u8]) {
        let Ok(root) = crate::thumbnail::references::library_root() else { return };
        let path = match crate::thumbnail::references::store(&root, name, bytes) {
            Ok(path) => path,
            Err(err) => {
                self.set_thumbnail_status(&format!("{err:#}"));
                return;
            }
        };
        // The stored name, not the dropped one: a collision renames it.
        let stored = path.file_name().unwrap_or_default().to_string_lossy().into_owned();
        let message = match crate::thumbnail::references::set_active(&root, &stored, true) {
            Ok(()) => format!("Added {stored} — on, and it will shape the next generation"),
            Err(err) => format!("Added {stored}, but not switched on: {err:#}"),
        };
        self.set_thumbnail_status(&message);
    }

    fn remove_reference(&mut self, name: &str) {
        let Ok(root) = crate::thumbnail::references::library_root() else { return };
        match crate::thumbnail::references::remove(&root, name) {
            Ok(()) => self.set_thumbnail_status(&format!("Removed {name}")),
            Err(err) => {
                self.set_thumbnail_status(&format!("{err:#}"));
                // The tile is already gone from the pane; put it back.
                self.update_thumbnail_view();
            }
        }
    }

    // ----------------------------------------------------------- settings

    /// Write the Settings form and redraw the pane from what the file now says.
    ///
    /// Redrawing from `settings::sections()` rather than from `fields` on
    /// purpose: the pane should show what the *app* will read, so a key that
    /// did not stick — because the file was not writable, or because the shell
    /// exports its own value — says so instead of echoing back what was typed.
    fn save_settings(&mut self, fields: std::collections::BTreeMap<String, String>) {
        let count = fields.len();
        let note = match crate::settings::write(&fields) {
            Ok(path) => {
                if count == 0 {
                    "Nothing to save — the boxes were all empty.".to_string()
                } else {
                    format!("Saved to {}.", path.display())
                }
            }
            Err(err) => format!("Could not save: {err:#}"),
        };
        self.redraw_settings(Some(&note));
        // A key arriving unblocks the stages gated on it — `stage::Stages::read`
        // checks `S3_BUCKET`, `BUFFER_API_KEY` and the Strapi pair through
        // `env_set`, and those buttons are drawn from a snapshot taken before
        // this save. Without this the key is live but its button stays greyed
        // out until something else happens to redraw.
        if let Some(live) = self.live.as_ref() {
            live.control_target.set_stage_gates(&self.stages());
        }
    }

    /// Run one group's credential check off the event loop.
    ///
    /// On a worker thread because every check is a network round trip, and the
    /// pane is drawn by the same thread that would be blocked waiting for it.
    fn test_settings(&mut self, service: &str) {
        let Some(group) = crate::settings::Group::from_slug(service) else {
            eprintln!("stream-recorder: settings pane asked to test unknown group {service:?}");
            return;
        };
        let Some(tx) = self.live.as_ref().map(|live| live.ui_tx.clone()) else { return };
        std::thread::spawn(move || {
            let _ = tx.send(UiEvent::SettingsTested(crate::settings::check::run(group)));
        });
    }

    /// Put one test's verdict next to its section without redrawing the pane —
    /// a redraw here would throw away anything typed into the other boxes.
    fn settings_tested(&mut self, outcome: &crate::settings::check::Outcome) {
        let Some(live) = self.live.as_ref() else { return };
        live.settings_pane.eval(&outcome.script());
    }

    fn redraw_settings(&self, note: Option<&str>) {
        if let Some(live) = self.live.as_ref() {
            live.settings_pane.show(&ui::settings_page(note));
        }
    }

    fn set_thumbnail_status(&self, msg: &str) {
        if let Some(live) = self.live.as_ref() {
            live.control_target.set_thumbnail_status(msg);
        }
    }

    fn drain_thumbnail(&mut self) {
        let events: Vec<_> = self.thumbnail_rx.try_iter().collect();
        if events.is_empty() {
            return;
        }
        let mut repaint = false;
        for event in events {
            match event {
                crate::thumbnail::ThumbnailEvent::Status(msg) => self.set_thumbnail_status(&msg),
                crate::thumbnail::ThumbnailEvent::PortraitSaved { root } => {
                    if root == self.session.root { self.set_thumbnail_status("Photo ready. Enter a title and generate artwork."); repaint = true; }
                }
                crate::thumbnail::ThumbnailEvent::Prepared { name, bytes } => {
                    self.store_reference(&name, &bytes);
                    repaint = true;
                }
                crate::thumbnail::ThumbnailEvent::Generated { made, total } => {
                    self.thumbnail_busy = false;
                    // Skipping every candidate is the *designed* outcome when
                    // nothing changed, so it has to read as an answer rather than
                    // as the same message a successful run would print.
                    self.set_thumbnail_status(&match made {
                        0 => format!(
                            "Nothing new to draw — same brief, still and references ({total} on file). \
                             Edit the brief or capture a frame first."
                        ),
                        made => format!("{made} new candidate(s) — {total} on file."),
                    });
                    repaint = true;
                }
                crate::thumbnail::ThumbnailEvent::Failed(msg) => {
                    self.thumbnail_busy = false;
                    self.set_thumbnail_status(&msg);
                }
            }
        }
        if repaint {
            self.update_thumbnail_view();
        }
    }

    /// Repaints the Review tab from whatever is in the render folder.
    ///
    /// Cheap — a handful of `stat` calls and a template — so it runs wherever the
    /// render summary does rather than needing a button of its own.
    fn update_review_view(&self) {
        let Some(live) = self.live.as_ref() else { return };
        let pane = crate::review::build(&self.session.render_dir());
        live.review_pane.show_local(
            &ui::render::page("review.html", &pane),
            &self.session.root,
            &self.session.root,
            ".review.html",
        );
    }

    /// Repaints the Substack tab from the notes on disk.
    ///
    /// A file read and a template, so it runs wherever the other version-scoped
    /// views do. It carries the gate's reason rather than deciding one itself:
    /// the pane and the button have to agree about why nothing can be generated.
    fn update_substack_view(&self) {
        let Some(live) = self.live.as_ref() else { return };
        let Some(view) = live.substack_pane.as_ref() else { return };
        let gate = self.stages().substack;
        let pane = crate::substack::pane::build(
            &self.session.root,
            &self.session.substack_dir(),
            gate.is_ready(),
            gate.missing().map(str::to_string),
        );
        view.show_local(
            &ui::render::page("substack.html", &pane),
            &self.session.root,
            &self.session.root,
            ".substack.html",
        );
    }

    /// Repaints the Blog tab: the draft on disk, the ledger row if the post is
    /// already live, and the gate's reason when it is not ready.
    fn update_blog_view(&self) {
        let Some(live) = self.live.as_ref() else { return };
        let gate = self.stages().blog;
        // The newest upload is what the post would be about, so it is also the
        // one whose ledger row decides whether this has already been posted.
        let posted = crate::publish::load(&self.session)
            .into_iter()
            .next_back()
            .and_then(|upload| crate::blog::posted(&self.session, &upload.video_id));
        let pane = crate::blog::pane::build(
            &self.session.root,
            &self.session.blog_dir(),
            gate.is_ready(),
            gate.missing().map(str::to_string),
            posted,
            &crate::config::load(),
            &crate::blog::library::load(),
            // The selected display's backing scale, so figure sizes read in
            // pixels. `None` on a talking-head layout, where there is no
            // display selected and a point size is the honest answer.
            crate::figure::pane::build(
                &self.session.root,
                self.geometry.map(|geometry| geometry.scale()),
            ),
        );
        live.blog_pane.show_local(
            &ui::render::page("blog.html", &pane),
            &self.session.root,
            &self.session.root,
            ".blog.html",
        );
    }

    /// Repaints the Edit tab from the take on disk.
    ///
    /// Called sparingly, and that is deliberate: the pane holds the zoom, the scroll and
    /// the playhead, and `show_local` reloads the page. Opening another chapter, a Reset
    /// and a finished render are the three moments where losing that position is what
    /// the user asked for; a keep-list save is not one of them.
    fn update_edit_view(&self) {
        let Some(live) = self.live.as_ref() else { return };
        let Some(view) = live.edit_pane.as_ref() else { return };
        let pane = crate::edit::pane::build(
            &self.session.dir,
            &self.session.edit_dir(),
            self.open_edit_chapter,
        );
        view.show_local(
            &ui::render::edit_page(&pane),
            &self.session.root,
            &self.session.root,
            ".edit.html",
        );
    }

    fn set_edit_status(&self, text: &str) {
        if let Some(live) = self.live.as_ref() {
            live.control_target.set_edit_status(text);
        }
    }

    /// Writes a hand-edited keep-list, and nothing else.
    ///
    /// No cut and no repaint. The pane already shows this list — it is where it came
    /// from — so the only job here is making it survive a crash, a version switch, or
    /// the next press of Render.
    fn save_edit_spans(&mut self, chapter: u32, spans: &[[i64; 2]]) {
        match crate::edit::pane::save_spans(
            &self.session.edit_dir(),
            &self.session.dir,
            chapter,
            spans,
        ) {
            Ok(_) => self.set_edit_status(&format!(
                "Chapter {chapter:02}: {} segment(s) kept. Press Cut & render when ready.",
                spans.len()
            )),
            Err(err) => {
                eprintln!("stream-recorder: could not save the edit: {err:#}");
                self.set_edit_status(&format!("Could not save the edit: {err:#}"));
            }
        }
    }

    /// Cut this chapter to its keep-list and re-render what depends on it.
    ///
    /// Guarded by `render_busy` rather than a flag of its own: this writes the same
    /// `edit/vN` and `render/vN` folders Render does, and two threads composing into
    /// them at once is the one failure that would be hard to even diagnose.
    fn apply_edit(&mut self, chapter: u32) {
        if self.render_busy {
            self.set_edit_status("A render is already running — wait for it to finish.");
            return;
        }
        self.render_busy = true;
        self.set_edit_status(&format!("Cutting chapter {chapter:02}…"));
        crate::edit::spawn_recut(self.session.clone(), vec![chapter], self.render_tx.clone());
        self.sync_controls();
    }

    /// Back to the automatic cut, and repaint so the timeline shows it.
    fn reset_edit(&mut self, chapter: u32) {
        let dir = crate::edit::pane::chapter_dir(&self.session.edit_dir(), chapter);
        match crate::edit::keep::clear(&dir) {
            Ok(()) => {
                self.set_edit_status(&format!(
                    "Chapter {chapter:02} is back on the automatic cut."
                ));
                self.open_edit_chapter = Some(chapter);
                self.update_edit_view();
            }
            Err(err) => {
                eprintln!("stream-recorder: could not reset the edit: {err:#}");
                self.set_edit_status(&format!("Could not reset the edit: {err:#}"));
            }
        }
    }

    fn open_edit_chapter(&mut self, chapter: u32) {
        self.open_edit_chapter = Some(chapter);
        self.update_edit_view();
    }

    /// Renders the thumbnail pane from disk, like every other web pane.
    ///
    /// [`file_name`] is beside it because a status line naming a path wants the
    /// leaf, never the whole thing.
    fn update_thumbnail_view(&self) {
        let Some(live) = self.live.as_ref() else { return };
        let Ok(library) = crate::thumbnail::references::library_root() else { return };
        let pane = crate::thumbnail::pane::build(
            &self.session.root,
            &library,
            crate::config::load().thumbnail.brief,
        );
        // Both trees have to be readable, and the API takes one directory — so
        // scope it to the shallowest folder holding both, which for the default
        // library is `~/.stream-recorder` and never the whole disk.
        let access = crate::ui::common_ancestor(&self.session.root, &library);
        live.thumbnail_pane.show_local(
            &ui::render::page("thumbnail.html", &pane),
            &self.session.root,
            &access,
            ".thumbnail.html",
        );
    }

    fn run_reflect(&mut self) {
        if self.reflect_busy {
            self.set_reflect_status("A reflection is already running…");
            return;
        }
        self.reflect_busy = true;
        self.set_reflect_status("Reading every generative step…");
        crate::reflect::spawn_reflect(
            self.session.clone(),
            self.posts_pick.model().to_string(),
            self.posts_pick.provider().map(str::to_string),
            self.reflect_tx.clone(),
        );
    }

    fn run_validate_rewrite(&mut self, index: usize) {
        if self.reflect_busy {
            self.set_reflect_status("A reflection job is already running…");
            return;
        }
        self.reflect_busy = true;
        self.set_reflect_status("Generating with the proposed prompt…");
        crate::reflect::spawn_validate(
            self.session.clone(),
            index,
            self.posts_pick.model().to_string(),
            self.posts_pick.provider().map(str::to_string),
            self.reflect_tx.clone(),
        );
    }

    fn run_apply_rewrites(&mut self) {
        if self.reflect_busy {
            self.set_reflect_status("A reflection job is already running…");
            return;
        }
        self.reflect_busy = true;
        self.set_reflect_status("Writing the approved prompt overlays…");
        crate::reflect::spawn_apply(self.session.clone(), self.reflect_tx.clone());
    }

    /// Records one tick immediately.
    ///
    /// The pane posts the new value on every click rather than holding it, so
    /// there is never on-screen state to lose — and no need to save before
    /// validating or applying.
    fn set_rewrite_approval(&mut self, index: usize, value: bool) {
        let dir = self.session.reflect_dir();
        let Ok(mut report) = crate::reflect::schema::load(&dir) else {
            return;
        };
        let Some(rewrite) = report.reflection.rewrite.get_mut(index) else {
            return;
        };
        rewrite.approved = value;
        if let Err(err) = crate::reflect::schema::save(&dir, &report) {
            eprintln!("stream-recorder: could not save rewrite approval: {err:#}");
            return;
        }
        // Repaint so Apply Selected enables and the pill colour follows.
        self.update_reflect_view();
    }

    fn set_reflect_status(&self, msg: &str) {
        if let Some(live) = self.live.as_ref() {
            live.control_target.set_reflect_status(msg);
        }
    }

    fn drain_reflect(&mut self) {
        let events: Vec<_> = self.reflect_rx.try_iter().collect();
        if events.is_empty() {
            return;
        }
        let mut repaint = false;
        for event in events {
            match event {
                crate::reflect::ReflectEvent::Status(msg) => self.set_reflect_status(&msg),
                crate::reflect::ReflectEvent::Ready(path, report) => {
                    self.reflect_busy = false;
                    self.set_reflect_status(&format!(
                        "{} rewrite(s) proposed from {} → {}",
                        report.reflection.rewrite.len(),
                        report.inputs.summary(),
                        path.display()
                    ));
                    repaint = true;
                }
                crate::reflect::ReflectEvent::Validated(detail) => {
                    self.reflect_busy = false;
                    self.set_reflect_status(&format!("Validation: {detail}"));
                    repaint = true;
                }
                crate::reflect::ReflectEvent::Applied(applied) => {
                    self.reflect_busy = false;
                    self.set_reflect_status(&if applied.is_empty() {
                        "Nothing applied — tick a rewrite and validate it first.".to_string()
                    } else {
                        format!("Applied {}", applied.join(", "))
                    });
                    repaint = true;
                }
                crate::reflect::ReflectEvent::Failed(msg) => {
                    self.reflect_busy = false;
                    self.set_reflect_status(&msg);
                }
            }
        }
        if repaint {
            self.update_reflect_view();
        }
    }

    /// Renders the whole Reflect pane from disk.
    ///
    /// Whole-pane, every time: the HTML is a function of `reflect.json`, so the
    /// pane cannot show something the file does not say.
    fn update_reflect_view(&self) {
        let Some(live) = self.live.as_ref() else { return };
        let saved = crate::reflect::schema::load(&self.session.reflect_dir()).ok();
        let pane = crate::reflect::pane::build(saved.as_ref(), &self.session.root);
        live.reflect_pane
            .show(&ui::render::page("reflect.html", &pane));
    }

    fn run_analytics_pull(&mut self) {
        if self.analytics_busy {
            self.set_analytics_status("An analytics pull is already running…");
            return;
        }
        self.analytics_busy = true;
        self.set_analytics_status("Asking Buffer how the sent posts did…");
        crate::analytics::spawn_pull(self.session.clone(), self.analytics_tx.clone());
    }

    fn set_analytics_status(&self, msg: &str) {
        if let Some(live) = self.live.as_ref() {
            live.control_target.set_analytics_status(msg);
        }
    }

    fn drain_analytics(&mut self) {
        let events: Vec<_> = self.analytics_rx.try_iter().collect();
        if events.is_empty() {
            return;
        }
        // The queue repaint needs `&mut self`, so nothing here may hold `live`
        // across the loop — collect the intent and act on it afterwards.
        let mut repaint = false;
        for event in events {
            match event {
                crate::analytics::AnalyticsEvent::Status(msg) => {
                    self.set_analytics_status(&msg);
                }
                crate::analytics::AnalyticsEvent::Ready(outcome, markdown) => {
                    self.analytics_busy = false;
                    self.set_analytics_status(&format!(
                        "{} → {}",
                        outcome.summary(),
                        outcome.report.display()
                    ));
                    self.last_report = Some(markdown);
                    repaint = true;
                }
                crate::analytics::AnalyticsEvent::Collected(total, projects) => {
                    self.analytics_busy = false;
                    self.set_analytics_status(&format!(
                        "Swept {projects} project(s): {}",
                        total.summary()
                    ));
                    repaint = true;
                }
                crate::analytics::AnalyticsEvent::Failed(msg) => {
                    self.analytics_busy = false;
                    self.set_analytics_status(&msg);
                }
            }
        }
        if repaint {
            self.refresh_analytics_view();
        }
    }

    fn approve_all_schedule(&mut self) {
        let Some(live) = self.live.as_ref() else { return };
        if live.schedule_form.approve_all() == 0 {
            self.set_schedule_status("Nothing to approve — press Build Plan first.");
            return;
        }
        match self.save_schedule_approvals() {
            Ok(approved) => self.set_schedule_status(&format!(
                "Approved {approved} post(s) — press Queue to Buffer to send."
            )),
            Err(err) => self.set_schedule_status(&format!("Could not save approvals: {err:#}")),
        }
        self.update_schedule_summary();
    }

    fn run_schedule_queue(&mut self) {
        if self.schedule_busy {
            self.set_schedule_status("A schedule job is already running…");
            return;
        }
        let stages = self.stages();
        if let Some(reason) = stages.queue.missing() {
            self.set_schedule_status(reason);
            return;
        }
        // Commit the on-screen ticks to disk before the worker reads the plan, so
        // what gets sent is exactly what the rows said when Queue was pressed.
        let approved = match self.save_schedule_approvals() {
            Ok(count) => count,
            Err(err) => {
                self.set_schedule_status(&format!("Could not save approvals: {err:#}"));
                return;
            }
        };
        if approved == 0 {
            self.set_schedule_status("Nothing approved — tick the posts to send first.");
            return;
        }
        self.schedule_busy = true;
        self.set_schedule_status(&format!("Queueing {approved} approved post(s) to Buffer…"));
        crate::schedule::spawn_queue(self.session.clone(), self.schedule_tx.clone());
        self.sync_controls();
    }

    /// Empties the Buffer queue, after asking which one and getting a straight answer.
    fn run_schedule_clear(&mut self) {
        if self.schedule_busy {
            self.set_schedule_status("A schedule job is already running…");
            return;
        }
        let Some(live) = self.live.as_ref() else { return };
        // Counted from the ledger before the prompt, so the dialog can say how
        // many posts are about to go rather than asking for a blind yes.
        let queued = match crate::schedule::clear::count(
            &self.session,
            crate::schedule::clear::Scope::Project,
        ) {
            Ok(count) => count,
            Err(err) => {
                self.set_schedule_status(&format!("Could not read the ledger: {err:#}"));
                return;
            }
        };
        let choice = live
            .control_target
            .confirm_clear(queued, &self.session.title());
        let scope = match choice {
            Some(ui::ClearChoice::Project) => crate::schedule::clear::Scope::Project,
            Some(ui::ClearChoice::Everything) => crate::schedule::clear::Scope::Everything,
            None => {
                self.set_schedule_status("Clear cancelled — nothing deleted.");
                return;
            }
        };
        self.schedule_busy = true;
        self.set_schedule_status(&format!("Clearing {}…", scope.label()));
        crate::schedule::spawn_clear(self.session.clone(), scope, self.schedule_tx.clone());
        self.sync_controls();
    }

    /// Publishes the longform straight to YouTube.
    /// Remember who the next upload will be visible to.
    ///
    /// Saved immediately rather than read at upload time from the popup,
    /// because the choice is a standing preference like the category beside it:
    /// a channel that publishes unlisted drafts does so every time, and
    /// re-picking it per video is how it ends up wrong on the one that
    /// mattered.
    fn select_youtube_privacy(&mut self, idx: usize) {
        let Some(privacy) = crate::publish::youtube::Privacy::ALL.get(idx).copied() else {
            return;
        };
        if privacy == self.youtube_privacy {
            return;
        }
        self.youtube_privacy = privacy;
        let mut cfg = crate::config::load();
        cfg.youtube_privacy = privacy;
        if let Err(e) = crate::config::save(&cfg) {
            eprintln!("stream-recorder: could not save the YouTube visibility: {e:#}");
        }
        // The summary names the visibility the next upload will carry, so it has
        // to repaint with the picker rather than at the next natural refresh.
        self.update_publish_summary();
    }

    fn run_youtube_upload(&mut self) {
        if self.publish_busy {
            self.set_publish_status("An upload is already running…");
            return;
        }
        self.publish_busy = true;
        self.set_publish_status("Uploading the longform to YouTube…");
        crate::publish::spawn_upload(self.session.clone(), self.publish_tx.clone());
        self.sync_controls();
    }

    fn set_publish_status(&self, text: &str) {
        if let Some(live) = self.live.as_ref() {
            live.control_target.set_publish_status(text);
        }
    }

    fn drain_publish(&mut self) {
        // No early return on a missing window: the terminal event clears the busy
        // flag, and losing it would wedge the button for good.
        let events: Vec<_> = self.publish_rx.try_iter().collect();
        for event in events {
            match event {
                crate::publish::PublishEvent::Status(msg) => self.set_publish_status(&msg),
                crate::publish::PublishEvent::Uploaded(upload) => {
                    self.publish_busy = false;
                    let thumb = match upload.thumbnail_set {
                        true => " with its thumbnail",
                        false => " — no thumbnail set",
                    };
                    // Summary first: it repaints the status line from the gate,
                    // and the link is the more useful thing to leave on screen.
                    self.update_publish_summary();
                    self.set_publish_status(&format!("Live at {}{thumb}", upload.url));
                    self.sync_controls();
                }
                crate::publish::PublishEvent::Connected => {
                    self.publish_busy = false;
                    self.sync_controls();
                    // Connecting satisfies nothing the publish gate asks for —
                    // it wants a rendered longform and a posts.json, and an
                    // OAuth grant is neither. This used to say "Press Upload."
                    // unconditionally, over a button `sync_controls` had just
                    // re-disabled and with the real reason wiped off the line,
                    // which reads exactly like the connect having failed. So
                    // the gate gets the last word, the way it does after an
                    // upload.
                    let status = match self.stages().publish {
                        crate::stage::Gate::Ready => {
                            "YouTube connected. Press Upload.".to_string()
                        }
                        crate::stage::Gate::Missing(reason) => {
                            format!("YouTube connected. {reason}")
                        }
                        crate::stage::Gate::Busy => "YouTube connected.".to_string(),
                    };
                    self.set_publish_status(&status);
                }
                crate::publish::PublishEvent::Failed(msg) => {
                    self.publish_busy = false;
                    self.set_publish_status(&msg);
                    self.sync_controls();
                }
            }
        }
    }

    /// Writes the form's ticks onto the saved plan. Returns how many items are now
    /// approved *and* unblocked — the exact set Queue will send.
    fn save_schedule_approvals(&self) -> anyhow::Result<usize> {
        let Some(live) = self.live.as_ref() else {
            return Ok(0);
        };
        let dir = self.session.schedule_dir();
        let mut plan = crate::schedule::load_plan(&dir)
            .context("no saved plan — press Build Plan first")?;
        live.schedule_form.apply(&mut plan);
        let approved = plan.sendable().count();
        crate::schedule::save_plan(&dir, &plan)?;
        Ok(approved)
    }

    fn set_schedule_status(&self, msg: &str) {
        if let Some(live) = self.live.as_ref() {
            live.control_target.set_schedule_status(msg);
        }
    }

    fn select_model(&mut self, idx: usize) {
        if self.notes_pick.choose_model(idx) {
            self.save_notes_choice();
        }
    }

    fn select_posts_model(&mut self, idx: usize) {
        if self.posts_pick.choose_model(idx) {
            self.save_posts_choice();
        }
    }

    /// Write the Post tab's model and provider down.
    ///
    /// The twin of [`Self::save_notes_choice`], and absent until now — which is
    /// why the Post tab forgot its model on every launch.
    fn save_posts_choice(&self) {
        let mut cfg = crate::config::load();
        cfg.posts_model = Some(self.posts_pick.model().to_string());
        cfg.posts_provider = self.posts_pick.provider().map(str::to_string);
        if let Err(e) = crate::config::save(&cfg) {
            eprintln!("stream-recorder: could not save the post model choice: {e:#}");
        }
    }

    /// Picks the Notes tab's provider, then re-fills its model popup.
    ///
    /// Only the Notes popup. The Post tab keeps its own provider and its own
    /// list: pushing this catalog to both is what used to make the Post tab
    /// display a model it was not using.
    fn select_provider(&mut self, idx: usize) {
        if !self.notes_pick.choose_provider(&self.notes_providers, idx) {
            return;
        }
        if let Some(live) = self.live.as_ref() {
            live.control_target
                .set_models(self.notes_pick.menu(), self.notes_pick.menu_index());
        }
        self.save_notes_choice();
    }

    /// The same for the Post tab, and the half that was missing: choosing a
    /// provider here used to set a routing string and leave the model list
    /// showing whatever the Notes tab last loaded.
    fn select_posts_provider(&mut self, idx: usize) {
        if !self.posts_pick.choose_provider(&self.notes_providers, idx) {
            return;
        }
        if let Some(live) = self.live.as_ref() {
            live.control_target
                .set_posts_models(self.posts_pick.menu(), self.posts_pick.menu_index());
        }
        // Changing the provider also repairs the model against the new catalog,
        // so both halves have to be written, not just the provider.
        self.save_posts_choice();
    }

    /// Keeps the audience/tone instructions between runs.
    ///
    /// It used to live only in the field: typed once, used for that session, and
    /// gone on the next launch. Who you are writing for is a standing preference,
    /// and one that silently reverts to nothing is worse than one you have to set,
    /// because the posts still generate — just in a default voice.
    fn save_posts_prompt(&self) {
        let mut cfg = crate::config::load();
        cfg.posts_prompt =
            (!self.posts_prompt.trim().is_empty()).then(|| self.posts_prompt.clone());
        if let Err(e) = crate::config::save(&cfg) {
            eprintln!("stream-recorder: could not save the posts prompt: {e:#}");
        }
    }

    fn save_notes_choice(&self) {
        let mut cfg = crate::config::load();
        cfg.notes_model = Some(self.notes_pick.model().to_string());
        cfg.notes_provider = self.notes_pick.provider().map(str::to_string);
        cfg.notes_prompt = (!self.notes_prompt.trim().is_empty()).then(|| self.notes_prompt.clone());
        if let Err(e) = crate::config::save(&cfg) {
            eprintln!("stream-recorder: could not save model choice: {e:#}");
        }
    }

    fn new_version(&mut self) {
        if let Some(mut router) = self.router.take() {
            if let Err(e) = router.stop() {
                eprintln!("stream-recorder: error finishing take: {e:#}");
            }
        }
        // A version starts its own chapter numbering: the folder is empty, so
        // chapter 1 is free and the take reads as take one.
        self.next_chapter = 1;
        match self.session.next_version() {
            Ok(next) => {
                if let Err(e) = crate::notes::copy_notes_into(&self.session, &next) {
                    eprintln!("stream-recorder: could not copy notes into the new version: {e:#}");
                }
                // Figures travel too, and for a stronger reason than notes do:
                // a figure is a moment on a screen that has since moved on, so
                // a new version that started with none could not get them back.
                if let Err(e) =
                    crate::figure::copy_into(&self.session.root, &next.root)
                {
                    eprintln!(
                        "stream-recorder: could not copy figures into the new version: {e:#}"
                    );
                }
                self.session = next;
                // The same refresh a version switch does, because it is one. The
                // summaries it computes are all empty for a fresh version, which
                // is the honest thing to show; the status line carries the
                // guidance that used to be faked into the summary itself.
                self.refresh_version_views();
                if let Some(live) = self.live.as_ref() {
                    live.control_target.set_render_status(
                        "New version — record, then press Render video.",
                    );
                }
            }
            Err(e) => eprintln!("stream-recorder: new version failed: {e:#}"),
        }
    }

    fn update_render_summary(&self) {
        let Some(live) = self.live.as_ref() else { return };
        let edit_dir = self.session.edit_dir();
        let render_dir = self.session.render_dir();
        let mut summary = String::new();
        // Above the detail, because it is the reason the button below is grey.
        if let Some(reason) = self.stages().render.missing() {
            summary.push_str(&format!("**Render is waiting on: {reason}**\n\n"));
        }
        summary.push_str(&format!("# Cut ({})\n\n", edit_dir.display()));
        let mut cut_any = false;
        for n in 1..=99 {
            let ch_dir = edit_dir.join(format!("chapter-{n:02}"));
            let edits_file = ch_dir.join("edits.json");
            if !edits_file.exists() {
                continue;
            }
            cut_any = true;
            summary.push_str(&format!("## Chapter {n:02}\n"));
            if let Ok(text) = std::fs::read_to_string(&edits_file) {
                if let Ok(edits) = serde_json::from_str::<Vec<crate::edit::compute::Edit>>(&text) {
                    summary.push_str(&format!("- Kept segments: {}\n", edits.len()));
                    let kept_words: usize = edits
                        .iter()
                        .map(|e| e.text.split_whitespace().count())
                        .sum();
                    summary.push_str(&format!("- Words kept: {kept_words}\n"));
                }
            }
            if ch_dir.join(format!("chapter-{n:02}-horizontal.mp4")).exists() {
                summary.push_str("- Horizontal: cut\n");
            }
            if ch_dir.join(format!("chapter-{n:02}-vertical.mp4")).exists() {
                summary.push_str("- Vertical: cut\n");
            }
            summary.push('\n');
        }
        if !cut_any {
            summary.push_str("No cut yet.\n\n");
        }
        summary.push_str(&format!("# Render ({})\n\n", render_dir.display()));
        let h_longform = render_dir.join("horizontal/longform.mp4");
        if h_longform.exists() {
            let size_mb = h_longform
                .metadata()
                .map(|m| m.len() as f64 / (1024.0 * 1024.0))
                .unwrap_or(0.0);
            summary.push_str(&format!(
                "## Horizontal longform\n- {}\n- {:.1} MB\n\n",
                h_longform.display(),
                size_mb
            ));
        }
        summary.push_str("## Vertical clips\n");
        let mut count = 0;
        for n in 1..=99 {
            let v_file = render_dir.join(format!("vertical/chapter-{n:02}.mp4"));
            if v_file.exists() {
                count += 1;
                let size_mb = v_file
                    .metadata()
                    .map(|m| m.len() as f64 / (1024.0 * 1024.0))
                    .unwrap_or(0.0);
                summary.push_str(&format!(
                    "- Chapter {n:02}: {:.1} MB\n",
                    size_mb
                ));
            }
        }
        if count == 0 {
            summary.push_str("None yet.\n");
        }
        live.control_target.set_render_summary(&summary);
    }

    fn update_distribute_summary(&self) {
        let Some(live) = self.live.as_ref() else { return };
        // A running upload is left to narrate itself: it reports per-file byte
        // counts, which is strictly more than this could say.
        match self.stages().distribute {
            crate::stage::Gate::Missing(reason) => {
                live.control_target.set_distribute_status(&reason)
            }
            crate::stage::Gate::Ready => live
                .control_target
                .set_distribute_status("Ready to upload to S3."),
            crate::stage::Gate::Busy => {}
        }
        let mut info = format!("# Distribute ({})\n\n", self.session.distribute_dir().display());
        if let Ok(links) = crate::distribute::load(&self.session.distribute_dir()) {
            info.push_str("## Public URLs\n");
            for item in &links.items {
                info.push_str(&format!("- {}: {}\n", item.id, item.url));
            }
        } else {
            info.push_str("No upload yet. Render, then Upload to S3.\n");
        }
        live.control_target.set_distribute_info(&info);
    }

    /// What the YouTube tab shows: whether this render is already up, and the
    /// title and description the upload would carry.
    ///
    /// Reads the ledger rather than the channel. Asking YouTube would need a
    /// token, a round trip and a spinner to answer a question the append-only
    /// row on disk already answers exactly.
    fn update_publish_summary(&self) {
        let Some(live) = self.live.as_ref() else { return };
        // A running upload narrates itself; leave its line alone.
        match self.stages().publish {
            crate::stage::Gate::Missing(reason) => live.control_target.set_publish_status(&reason),
            crate::stage::Gate::Ready => live
                .control_target
                .set_publish_status("Ready to upload the longform."),
            crate::stage::Gate::Busy => {}
        }

        // Ahead of the explanation, not buried under it. Everything below is
        // background that does not change; this is the part that says whether
        // pressing Upload will do anything, and it is the reason a disabled
        // button and an unconnected account used to be indistinguishable.
        let mut info = String::new();
        match self.stages().publish.missing() {
            Some(reason) => info.push_str(&format!(
                "## Not ready\n- Upload is disabled: {reason}\n"
            )),
            None => info.push_str("## Ready\n- Inputs are on disk.\n"),
        }
        match crate::publish::connected_channel() {
            Some(channel) => info.push_str(&format!("- Connected to {channel}\n\n")),
            None => info.push_str(
                "- NOT CONNECTED — press Connect… and finish Google's flow within \
                 five minutes. To land on the SAAGA channel, switch to it on \
                 youtube.com *first*: the grant binds to whichever channel is active \
                 there.\n\n",
            ),
        }
        info.push_str("Upload sends the rendered video and your selected thumbnail. After upload, continue to Blog (Strapi), then generate social posts.\n\n");

        info.push_str(&format!(
            "## Next upload\n- Visibility: {}\n\n",
            self.youtube_privacy.label()
        ));

        let uploads = crate::publish::load(&self.session);
        match uploads.last() {
            None => info.push_str("## Not uploaded yet\n"),
            Some(latest) => {
                info.push_str("## Uploaded\n");
                info.push_str(&format!("- {}\n", latest.url));
                info.push_str(&format!("- Title: {}\n", latest.title));
                info.push_str(&format!("- At: {}\n", latest.uploaded_at));
                info.push_str(&format!("- Visibility: {}\n", latest.privacy.label()));
                let thumb = match latest.thumbnail_set {
                    true => "set",
                    false => "not set — press Upload again to retry the thumbnail",
                };
                info.push_str(&format!("- Thumbnail: {thumb}\n"));
            }
        }
        live.publish_pane.show_local(
            &ui::render::page("youtube.html", minijinja::context! {
                metadata => crate::publish::metadata::load(&self.session), info => info,
            }),
            &self.session.root, &self.session.root, ".youtube.html",
        );
    }

    /// Renders the *saved* plan, so the tab always shows what Queue would send.
    fn update_schedule_summary(&self) {
        let Some(live) = self.live.as_ref() else { return };
        let schedule_dir = self.session.schedule_dir();
        let plan = crate::schedule::load_plan(&schedule_dir).ok();
        let hosted = crate::distribute::load(&self.session.distribute_dir()).ok();
        match &plan {
            Some(saved) => live.schedule_form.show(saved),
            None => live.schedule_form.show_empty(),
        }
        let status = match &plan {
            Some(saved) => {
                let ready = saved.queueable().count();
                format!(
                    "Plan: {ready} ready / {} skipped — {} approved",
                    saved.items.len() - ready,
                    saved.sendable().count()
                )
            }
            // With no plan yet, what Build Plan is waiting for is the whole
            // story — and the gate already knows which of the three it is.
            None => match self.stages().plan.missing() {
                Some(reason) => reason.to_string(),
                None => "S3 links ready — press Build Plan".to_string(),
            },
        };
        live.control_target.set_schedule_status(&status);

        let mut info = format!("# Schedule ({})\n\n", schedule_dir.display());
        match plan {
            Some(plan) => {
                let ready = plan.queueable().count();
                let skipped = plan.items.len() - ready;
                info.push_str(&format!("**{ready} ready / {skipped} skipped**\n\n"));
                for item in &plan.items {
                    let channel = if item.channel_name.is_empty() {
                        "—"
                    } else {
                        item.channel_name.as_str()
                    };
                    let state = match (&item.skip, item.approved) {
                        (Some(reason), _) => format!("skipped: {reason}"),
                        (None, true) => format!("APPROVED — {}", item.reason),
                        (None, false) => format!("awaiting approval — {}", item.reason),
                    };
                    info.push_str(&format!(
                        "- {} · {} · {} — {}\n",
                        item.video_id, item.platform, channel, state
                    ));
                    // The words themselves. Approval is bound to the copy hash, so
                    // the caption has to be readable next to the switch that approves
                    // it — ticking a row you cannot read is not a review.
                    if let Some(title) = &item.title {
                        info.push_str(&format!("    title: {title}\n"));
                    }
                    for line in item.text.lines() {
                        info.push_str(&format!("    | {line}\n"));
                    }
                    // The payload a human is actually approving: public vs private,
                    // who gets notified. Hiding it defeats the point of a review step.
                    if let Some(meta) = &item.metadata {
                        info.push_str(&format!("    payload: {meta}\n"));
                    }
                    info.push('\n');
                }
            }
            None => {
                let count = hosted.map(|l| l.items.len()).unwrap_or(0);
                info.push_str(&format!("No plan yet. {count} public URL(s) from Distribute.\n"));
                info.push_str("Press Build Plan to see what would be queued.\n");
            }
        }
        live.control_target.set_schedule_info(&info);
    }

    fn drain_titles(&mut self) {
        let events: Vec<_> = self.titles_rx.try_iter().collect();
        let Some(live) = self.live.as_ref() else {
            return;
        };
        for event in events {
            match event {
                crate::titles::TitlesEvent::Status(msg) => {
                    live.control_target.set_render_status(&msg);
                }
                crate::titles::TitlesEvent::Ready(path, manifest) => {
                    live.control_target.set_render_status(&format!(
                        "{} titles saved. {}",
                        manifest.chapters.len(), path.display()
                    ));
                }
            }
        }
    }

    fn drain_notes(&mut self) {
        let events: Vec<_> = self.notes_rx.try_iter().collect();
        let Some(live) = self.live.as_ref() else {
            return;
        };
        for event in events {
            match event {
                crate::notes::NotesEvent::Status(msg) => live.notes.show_status(&msg),
                crate::notes::NotesEvent::Ready(html) => {
                    live.notes.load(&html);
                    live.notes.reset_slide();
                }
                crate::notes::NotesEvent::NoSpeech(why) => {
                    // The one thing the notes thread cannot know: which mic these
                    // chapters came from. A wrong device is the usual reason for
                    // silence, and the fix is one dropdown away.
                    live.notes.show_status(&format!(
                        "Notes failed: {why}. Recorded from microphone \"{}\" — if that is \
                         not the one you spoke into, pick it in the Microphone dropdown and \
                         record again.",
                        self.mic_name()
                    ));
                }
            }
        }
    }

    fn drain_render(&mut self) {
        // No early return on a missing window, for the same reason as
        // `drain_schedule`: the terminal event is what clears `render_busy`, and
        // dropping it would leave the Render button off for good.
        let events: Vec<_> = self.render_rx.try_iter().collect();
        for event in events {
            match event {
                crate::edit::RenderEvent::Status(msg) => self.show_render_progress(&msg),
                crate::edit::RenderEvent::Ready(dir) => {
                    self.render_busy = false;
                    self.show_render_progress(&format!("Render ready — open Review. Files: {}", dir.display()));
                    // Every chapter has transcribed by now, which is what the
                    // title and description are written from.
                    self.write_copy_after_render();
                    self.update_render_summary();
                    // The cut moved, so the Edit tab's per-chapter figures did too.
                    self.update_edit_view();
                    self.update_review_view();
        self.update_substack_view();
        self.update_blog_view();
                    self.update_distribute_summary();
                    self.update_publish_summary();
                    self.update_schedule_summary();
                    self.sync_controls();
                }
                crate::edit::RenderEvent::Failed(msg) => {
                    self.render_busy = false;
                    self.show_render_progress(&msg);
                    self.sync_controls();
                }
            }
        }
    }

    /// The render narrates everywhere it is visible: the Render tab's status line, the
    /// notes pane (which is what is on screen while a take is being cut), and the Edit
    /// tab — a re-cut is usually started from there, and it is where the wait is felt.
    fn show_render_progress(&self, msg: &str) {
        if let Some(live) = self.live.as_ref() {
            live.notes.show_status(msg);
            live.control_target.set_render_status(msg);
            live.control_target.set_edit_status(msg);
        }
    }

    fn drain_posts(&mut self) {
        let events: Vec<_> = self.posts_rx.try_iter().collect();
        let Some(live) = self.live.as_ref() else {
            return;
        };
        for event in events {
            match event {
                crate::posts::PostsEvent::Status(msg) => {
                    live.control_target.set_posts_status(&msg);
                }
                crate::posts::PostsEvent::Ready(path, manifest) => {
                    live.control_target.set_posts_status(&format!("Posts generated and saved → {}", path.display()));
                    live.posts_form.show(&manifest);
                    self.posts_manifest = Some(manifest);
                    self.update_schedule_summary();
                    // The gates are read off disk, and this job is what puts
                    // posts.json there — the two stages waiting on it (YouTube,
                    // which takes its title and description from it, and Build
                    // Plan) stay switched off until something re-reads. Without
                    // this they wait for the next unrelated event, which looks
                    // exactly like the button being broken.
                    self.sync_controls();
                }
            }
        }
    }

    fn drain_distribute(&mut self) {
        // No early return on a missing window: the terminal event is what clears
        // `distribute_busy`, and dropping it would wedge the Upload button.
        let events: Vec<_> = self.distribute_rx.try_iter().collect();
        for event in events {
            match event {
                crate::distribute::DistributeEvent::Status(msg) => {
                    self.set_distribute_status(&msg);
                }
                crate::distribute::DistributeEvent::Progress(id, update) => {
                    if let Some(live) = self.live.as_ref() {
                        live.control_target.set_distribute_progress(update.pct);
                    }
                    self.set_distribute_status(&format!(
                        "Uploading {id} — {:.1}%  {:.1}/{:.1} MB",
                        update.pct, update.seen_mb, update.total_mb
                    ));
                }
                crate::distribute::DistributeEvent::Ready(path, _links) => {
                    self.distribute_busy = false;
                    if let Some(live) = self.live.as_ref() {
                        live.control_target.hide_distribute_progress();
                    }
                    // Summary first: it repaints the status line, and the path is
                    // the more useful thing to leave on screen.
                    self.update_distribute_summary();
                    self.update_schedule_summary();
                    self.set_distribute_status(&format!("S3 upload ready → {}", path.display()));
                    self.sync_controls();
                }
                crate::distribute::DistributeEvent::Failed(msg) => {
                    self.distribute_busy = false;
                    if let Some(live) = self.live.as_ref() {
                        live.control_target.hide_distribute_progress();
                    }
                    self.set_distribute_status(&msg);
                    self.sync_controls();
                }
            }
        }
    }

    fn drain_schedule(&mut self) {
        // No early return on a missing window: the terminal event is what clears
        // `schedule_busy`, and dropping it would wedge both buttons for good.
        let events: Vec<_> = self.schedule_rx.try_iter().collect();
        for event in events {
            match event {
                crate::schedule::ScheduleEvent::Status(msg) => self.set_schedule_status(&msg),
                crate::schedule::ScheduleEvent::Planned(path, plan) => {
                    self.schedule_busy = false;
                    // Re-read from disk first, so the preview is the file Queue
                    // will use — then overwrite the status it left behind.
                    self.update_schedule_summary();
                    let ready = plan.queueable().count();
                    self.set_schedule_status(&format!(
                        "Plan saved → {} ({ready} ready / {} skipped)",
                        path.display(),
                        plan.items.len() - ready
                    ));
                    // The plan it just wrote is what unlocks Approve and Queue.
                    self.sync_controls();
                }
                crate::schedule::ScheduleEvent::Queued(outcome) => {
                    self.schedule_busy = false;
                    self.update_schedule_summary();
                    let mut msg = format!("{} → {}", outcome.summary(), outcome.ledger.display());
                    // A live post with no ledger row is the one thing the next plan
                    // cannot see, so it stays on screen instead of scrolling past.
                    if !outcome.unrecorded.is_empty() {
                        msg = format!("{msg} — UNRECORDED: {}", outcome.unrecorded.join(", "));
                    }
                    self.set_schedule_status(&msg);
                    self.sync_controls();
                }
                crate::schedule::ScheduleEvent::Cleared(outcome) => {
                    self.schedule_busy = false;
                    // Re-plan territory: every cleared item is queueable again, and
                    // the saved plan still shows it as already queued until rebuilt.
                    self.update_schedule_summary();
                    let mut msg = outcome.summary();
                    if !outcome.failed.is_empty() {
                        msg = format!("{msg} — {}", outcome.failed.join("; "));
                    }
                    if !outcome.unrecorded.is_empty() {
                        msg = format!("{msg} — UNRECORDED: {}", outcome.unrecorded.join(", "));
                    }
                    self.set_schedule_status(&format!("{msg}. Press Build Plan to re-plan."));
                    self.sync_controls();
                }
                crate::schedule::ScheduleEvent::Failed(msg) => {
                    self.schedule_busy = false;
                    self.update_schedule_summary();
                    self.set_schedule_status(&msg);
                    self.sync_controls();
                }
            }
        }
    }
}


impl ApplicationHandler for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.live.is_some() {
            return;
        }
        match self.start(event_loop) {
            Ok(live) => {
                if let Some(html) = crate::notes::existing_html(&self.session) {
                    live.notes.load(&html);
                }
                let title = match self.session.version {
                    Some(v) => format!("stream-recorder · v{v}"),
                    None => "stream-recorder".into(),
                };
                live.window.set_title(&title);
                self.next_chapter = crate::notes::closed_chapter_numbers(&self.session.dir)
                    .into_iter()
                    .max()
                    .unwrap_or(0)
                    + 1;
                if let Ok(manifest) = crate::posts::load_manifest(&self.session.posts_dir()) {
                    live.posts_form.show(&manifest);
                    live.control_target.set_posts_status("Loaded saved posts.");
                    self.posts_manifest = Some(manifest);
                }
                self.live = Some(live);
                // Before the picker is filled, so it never lists a folder that is
                // about to go. Never the open project, whatever state it is in.
                match crate::sessions::sweep_empty(&self.session.root) {
                    0 => {}
                    swept => println!("stream-recorder: removed {swept} empty project folder(s)"),
                }
                // Nothing filled the project and version pickers before this, so
                // a launch showed both empty however far along the project was.
                self.refresh_project_controls();
                self.sync_controls();
                self.sync_overlay();
                // Before `install_preview`, so a session that launches with
                // tracking already on starts loading the detector at launch
                // rather than on the first click. The preview installed below
                // is still untracked — the graph is rebuilt when the tracker
                // arrives, which is the same path a mid-session toggle takes.
                if self.face_tracking_wanted() {
                    self.start_face_tracker();
                }
                self.ensure_pointer_tracker();
                self.sync_face_control();
                self.install_preview();
                self.report_clock_drift();
                self.update_render_summary();
        self.update_review_view();
        self.update_substack_view();
        self.update_blog_view();
                self.update_distribute_summary();
                self.update_publish_summary();
                self.update_schedule_summary();
                // Both panes are painted only by whatever last wrote them, so a
                // launch left them blank — the chosen thumbnail was on disk the
                // whole time, and the tab just never asked for it. Analytics is
                // not here because its first idle tick repaints it anyway.
                self.update_reflect_view();
                self.update_video_brief("Render video writes the title and description from the transcript and these notes.");
                self.update_thumbnail_view();
                    }
            Err(e) => {
                eprintln!("stream-recorder: {e:#}");
                event_loop.exit();
            }
        }
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, _id: WindowId, event: WindowEvent) {
        match event {
            WindowEvent::CloseRequested => {
                self.apply(Action::Quit, event_loop);
            }
            WindowEvent::Resized(_) => {
                if let Some(live) = self.live.as_ref() {
                    live.layout.sync(&live.preview, &live.notes);
                    live.posts_form.relayout();
                }
            }
            _ => {}
        }
    }

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        // Before draining, so a click that lands this tick finds the overlay in
        // the state the cursor's position implies.
        if let Some(overlay) = self.live.as_mut().and_then(|l| l.overlay.as_mut()) {
            overlay.track_cursor();
        }
        if let Some(live) = self.live.as_ref() {
            live.layout.sync(&live.preview, &live.notes);
            live.preview.pump();
        }
        self.tick_timer();
        self.drain_notes();
        self.drain_render();
        self.drain_posts();
        self.drain_substack();
        self.drain_blog();
        self.drain_titles();
        self.drain_distribute();
        self.drain_schedule();
        self.drain_publish();
        self.drain_analytics();
        self.drain_reflect();
        self.drain_thumbnail();
        self.drain_figure();
        self.drain_card();
        self.drain_video_copy();
        // `None` means never scanned, so the first tick after launch paints the
        // queue immediately rather than leaving it blank for a minute.
        if self
            .due_checked
            .is_none_or(|at| at.elapsed() >= DUE_SCAN_EVERY)
        {
            self.refresh_analytics_view();
        }

        let mut events: Vec<UiEvent> = Vec::new();
        if let Some(live) = self.live.as_ref() {
            events.extend(live.hotkeys.drain().into_iter().map(UiEvent::Action));
            events.extend(live.ui_rx.try_iter());
        }
        for event in events {
            self.handle_ui_event(event, event_loop);
        }
        event_loop.set_control_flow(ControlFlow::WaitUntil(Instant::now() + TICK));
    }
}

/// Prints what a chapter came to, once it is closed.
///
/// The speech figure is an estimate and says so. It is here as well as on screen
/// because the terminal is the record of a session — a chapter that ran six
/// minutes for one minute of talking is worth seeing in the log afterwards.
fn report_chapter(closed: Option<crate::app::clock::Closed>) {
    let Some(closed) = closed else {
        return;
    };
    println!(
        "stream-recorder: chapter {:02} ran {}, ~{} of it speech",
        closed.number,
        crate::app::clock::clock(closed.recorded),
        crate::app::clock::clock(closed.voiced),
    );
}

/// The leaf of a path, for naming a file in a status line.
fn file_name(path: &std::path::Path) -> String {
    path.file_name().unwrap_or_default().to_string_lossy().into_owned()
}
