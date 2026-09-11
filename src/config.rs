//! Persisted app config — the saved default capture devices, and where each
//! layout's screen region was last dragged to.
//! Lives outside the repo at `~/.stream-recorder/config.json`.

use std::collections::BTreeMap;
use std::path::PathBuf;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

/// The standing call-to-action card, as it is configured.
///
/// `url` is the only field the CMS component requires; the rest are omitted
/// when blank rather than sent empty. `image` is a path on this machine,
/// uploaded at publish time.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct BlogCta {
    #[serde(default)]
    pub url: String,
    #[serde(default)]
    pub label: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub image: Option<std::path::PathBuf>,
}

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct Config {
    pub audio_device_uid: Option<String>,
    pub camera_device_uid: Option<String>,
    /// `CGDirectDisplayID` as a decimal string, or `None` for "don't capture
    /// the screen". Unlike the device uids above this is *not* stable across
    /// reboots or re-plugging (see `capture::screen`), so it is only ever a
    /// hint — a stale value falls back to no screen rather than erroring.
    #[serde(default)]
    pub screen_display_id: Option<String>,
    /// Region origins per display, keyed by the same unstable display id.
    #[serde(default)]
    pub screen_regions: BTreeMap<String, DisplayRegions>,
    /// OpenRouter model id for notes, e.g. `google/gemini-2.5-flash`.
    #[serde(default)]
    pub notes_model: Option<String>,
    /// OpenRouter provider name, or none for automatic routing.
    #[serde(default)]
    pub notes_provider: Option<String>,
    /// Extra instructions appended when building notes.
    #[serde(default)]
    pub notes_prompt: Option<String>,
    /// The Post tab's own model and provider.
    ///
    /// Separate fields rather than reusing `notes_*`: the two stages pick
    /// independently on purpose (see [`crate::notes::Picker`]), and social copy
    /// is short and cheap where notes are long — the right model for one is
    /// routinely the wrong one for the other.
    ///
    /// These were missing entirely until the Settings tab gained a models
    /// section: the Post tab's dropdowns changed the running choice and nothing
    /// ever wrote it down, so every launch silently reverted to the default and
    /// the dropdown you set yesterday was not the model that ran today.
    #[serde(default)]
    pub posts_model: Option<String>,
    #[serde(default)]
    pub posts_provider: Option<String>,
    /// The Post tab's audience/tone instructions, remembered between projects.
    ///
    /// Here rather than in the project because it is a standing preference — who
    /// you are writing for does not change per video, and retyping it every
    /// recording is how it ends up not being set at all.
    #[serde(default)]
    pub posts_prompt: Option<String>,
    /// The Strapi author id the blog posts under, and the name it had when it
    /// was picked.
    ///
    /// Both, because they answer different questions. The id is what a
    /// `video-post` stores and is the only unambiguous handle — the live CMS has
    /// two authors with the same name. The name is what the pane shows before
    /// the author list has been fetched, which is every first launch on a new
    /// machine and every launch with no network.
    ///
    /// Config rather than the project: who the byline is does not change per
    /// video, and re-picking it every recording is how it ends up unset.
    #[serde(default)]
    pub blog_author_id: Option<i64>,
    #[serde(default)]
    pub blog_author_name: Option<String>,
    /// The category the post files under — the one taxonomy a video post
    /// carries, shared with `blog-articles` and read by `/blog`, the category
    /// pages, search and the breadcrumbs.
    ///
    /// Was `blog_education_category_*`, pointed at a collection that has since
    /// been retired. A config written before the rename reads back as unset,
    /// which is correct: the id it held names a row that no longer exists.
    #[serde(default)]
    pub blog_category_id: Option<i64>,
    #[serde(default)]
    pub blog_category_name: Option<String>,
    /// The standing inline call to action every post carries, if any.
    ///
    /// Config rather than the project, and configured rather than generated:
    /// the URL has to be a real place, and where it points is a marketing
    /// decision that holds across a run of videos rather than a thing to decide
    /// per recording. `url` empty means no CTA at all, which is the default.
    #[serde(default)]
    pub blog_cta: BlogCta,
    /// Thumbnail generation. Absent means the defaults, which are on — the stage
    /// runs with Render unless it is switched off here.
    #[serde(default)]
    pub thumbnail: Thumbnail,
    /// What Render produces. All three on unless switched off on the Video
    /// details pane, and remembered there.
    #[serde(default)]
    pub render: RenderTargets,
    /// How the Draft tab's live meter decides you are talking.
    #[serde(default)]
    pub vad: Vad,
    /// Whether the camera follows your face while recording, and how.
    #[serde(default)]
    pub face_tracking: FaceTracking,
    /// Whether the vertical screen crop follows the mouse, and how.
    #[serde(default)]
    pub mouse_tracking: MouseTracking,
    /// YouTube category for queued posts, as YouTube's own numeric id.
    ///
    /// Buffer rejects a YouTube post that has none — "YouTube posts require a
    /// category." — and it applies to Shorts as well as long video. The ids are
    /// YouTube constants: 28 Science & Technology, 27 Education, 24
    /// Entertainment, 26 Howto & Style, 22 People & Blogs.
    #[serde(default = "default_youtube_category")]
    pub youtube_category_id: String,
    /// Who can see the longform once it is uploaded. Set on the YouTube tab.
    ///
    /// Only the longform. Shorts queued through Buffer keep
    /// [`crate::schedule::meta::YOUTUBE_PRIVACY`], which that constant's docs
    /// explain — the longform's `youtube` row is skipped from every Buffer plan,
    /// so the two never describe the same video.
    ///
    /// Defaults to public, which is what every upload did before this was a
    /// choice, so an existing config keeps behaving exactly as it did.
    #[serde(default)]
    pub youtube_privacy: crate::publish::youtube::Privacy,
}

fn default_youtube_category() -> String {
    // Science & Technology: the closest fit for a technical screencast, and a
    // wrong-but-valid category posts, where an absent one does not.
    "28".to_string()
}

/// How the live speech meter decides you are talking.
///
/// These are the numbers the Draft tab's speech estimate runs on, and they live
/// in config rather than in code so that whatever cuts the silence later can be
/// handed the same ones — an estimate measured on a different threshold than the
/// cut is an estimate of nothing. See `capture::level`.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Vad {
    /// The quietest level that can ever count as speech, in dBFS. A backstop
    /// under the adaptive threshold, for a room quiet enough that the noise
    /// floor plus a margin would gate on the air conditioning.
    #[serde(default = "default_speech_dbfs")]
    pub speech_dbfs: f32,
    /// How far above the tracked noise floor the gate opens. What lets the same
    /// config work on the XLR dock and the built-in mic without re-tuning.
    #[serde(default = "default_floor_margin_db")]
    pub floor_margin_db: f32,
    /// How far below the opening level it closes again, so a level sitting on
    /// the threshold does not chatter the gate open and shut.
    #[serde(default = "default_release_db")]
    pub release_db: f32,
    /// How long speech must hold above the threshold before it counts. A key
    /// press is shorter than this.
    #[serde(default = "default_attack_ms")]
    pub attack_ms: u32,
    /// A quiet stretch shorter than this is a gap between words, not a pause,
    /// and stays inside the phrase. Roughly what a silence cutter keeps.
    #[serde(default = "default_hangover_ms")]
    pub hangover_ms: u32,
}

fn default_speech_dbfs() -> f32 {
    -40.0
}

fn default_floor_margin_db() -> f32 {
    12.0
}

fn default_release_db() -> f32 {
    5.0
}

fn default_attack_ms() -> u32 {
    80
}

fn default_hangover_ms() -> u32 {
    400
}

impl Default for Vad {
    fn default() -> Self {
        Vad {
            speech_dbfs: default_speech_dbfs(),
            floor_margin_db: default_floor_margin_db(),
            release_db: default_release_db(),
            attack_ms: default_attack_ms(),
            hangover_ms: default_hangover_ms(),
        }
    }
}

/// What the thumbnail stage does when Render runs.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Thumbnail {
    #[serde(default = "enabled_by_default")]
    pub enabled: bool,
    /// The models the pane offers. Adding one is a config edit, not a release —
    /// which is what makes trying a new model cheap.
    #[serde(default = "default_models")]
    pub models: Vec<crate::thumbnail::image::ModelSpec>,
    /// Which of them draws. One at a time, chosen in the pane: generating with
    /// every configured model at once bought a set you did not ask for and paid
    /// for each of them.
    #[serde(default = "default_model")]
    pub model: String,
    /// Candidates per model. Two models at two each is four images a run.
    #[serde(default = "default_per_model")]
    pub candidates_per_model: usize,
    /// The brief a new project starts from.
    ///
    /// The brief itself is per-project — a thumbnail is about that video — but
    /// the *house style* in it is not, and it was being retyped from scratch
    /// every recording. So the last brief saved becomes the next project's
    /// starting point, and editing it there does not reach back.
    #[serde(default)]
    pub brief: crate::thumbnail::Brief,
}

fn enabled_by_default() -> bool {
    true
}

fn default_per_model() -> usize {
    2
}

/// Every model here must be one OpenRouter will actually draw with.
///
/// Checked against `/api/v1/models` rather than assumed: the list carried
/// `bytedance/seedream-5-0-pro` ("is not a valid model ID") and then
/// `bytedance-seed/seedream-5-0-pro` ("No endpoints found that support the
/// requested output modalities"). Nothing matching Seedream is on OpenRouter at
/// all — it is a BytePlus model, and reaching it needs another provider client,
/// not another id. Every entry below outputs `image` per that endpoint.
fn default_models() -> Vec<crate::thumbnail::image::ModelSpec> {
    vec![
        crate::thumbnail::image::ModelSpec {
            id: BEST.into(),
            label: "Gemini 3 Pro Image".into(),
        },
        crate::thumbnail::image::ModelSpec {
            id: "google/gemini-3.1-flash-image".into(),
            label: "Nano Banana 2 (fast)".into(),
        },
        crate::thumbnail::image::ModelSpec {
            id: "openai/gpt-5-image".into(),
            label: "GPT-5 Image".into(),
        },
    ]
}

const BEST: &str = "google/gemini-3-pro-image";

fn default_model() -> String {
    BEST.to_string()
}

/// Model ids that were shipped and are not real, so a saved config carrying one
/// heals itself instead of failing every run.
///
/// A serde default only fills a key that is *missing*. This list was written to
/// `~/.stream-recorder/config.json` the first time the app saved, so correcting
/// [`default_models`] reached nobody who had already run it — their menu kept the
/// dead entry, offered it, and the selection wrote it back.
const RETIRED: [&str; 2] = [
    "bytedance/seedream-5-0-pro",
    "bytedance-seed/seedream-5-0-pro",
];

impl Thumbnail {
    /// Reconciles a saved menu with the shipped one: drops what is dead, adds
    /// what is new, and makes sure something real is selected.
    fn reconcile_models(&mut self) {
        self.models
            .retain(|model| !RETIRED.contains(&model.id.as_str()));
        // Additive, not just subtractive. Pruning alone left a config written
        // before a model existed with a *shorter* menu and no way to reach the
        // new one — a saved list is a preference, not a ceiling. Appended, so a
        // relabelled or reordered entry someone edited by hand stays put.
        for shipped in default_models() {
            if !self.models.iter().any(|model| model.id == shipped.id) {
                self.models.push(shipped);
            }
        }
        if !self.models.iter().any(|model| model.id == self.model) {
            self.model = default_model();
        }
    }
}

impl Default for Thumbnail {
    fn default() -> Self {
        Thumbnail {
            enabled: enabled_by_default(),
            models: default_models(),
            model: default_model(),
            candidates_per_model: default_per_model(),
            brief: crate::thumbnail::Brief::default(),
        }
    }
}

/// Live face tracking: whether the camera follows the subject while recording,
/// and how hard it resists doing so.
///
/// Off by default. It is an option because it is a *creative* choice — a locked
/// camera and a following one are two different looks, not a good and a bad
/// one — and because it costs a one-time ~34 MB runtime download the first time
/// it is switched on. See [`crate::face`].
///
/// The damping fields are all here rather than in code for the same reason
/// [`Vad`]'s are: they are the numbers a look is tuned on, tuning them should
/// not need a rebuild, and whatever consumes the framing later deserves to be
/// able to read what it was tuned to.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FaceTracking {
    #[serde(default)]
    pub enabled: bool,
    /// Detect on one frame in this many. `1` is every frame.
    ///
    /// This is a *responsiveness* dial, not a smoothness one — the framing
    /// glides at the full capture rate whatever this says, so raising it delays
    /// how quickly the camera notices you moved and changes nothing about how
    /// the move looks. See `face::smooth`. At 30fps the default samples ten
    /// times a second for about 1.5 ms each.
    #[serde(default = "default_detect_every")]
    pub detect_every: u32,
    /// Width, in pixels, the frame is scaled to before detection.
    ///
    /// Small on purpose: the model resizes to 128×128 internally, so this
    /// buys nothing in accuracy above a couple of hundred pixels and costs a
    /// GPU→CPU copy proportional to its area. See `face::sample`.
    #[serde(default = "default_detect_width")]
    pub detect_width: u32,
    /// How sure the model has to be before the camera will move for it.
    #[serde(default = "default_min_confidence")]
    pub min_confidence: f32,
    /// How far below the eyes to aim, in multiples of the detected face's own
    /// height. Higher lifts the eyes further up the finished frame.
    #[serde(default = "default_headroom")]
    pub headroom: f64,
    /// Roughly how long the framing takes to settle on a new position.
    #[serde(default = "default_smoothing_s")]
    pub smoothing_s: f64,
    /// Target moves smaller than this fraction of the frame are detector noise
    /// and are discarded. The number that decides whether sitting still holds a
    /// still frame.
    #[serde(default = "default_deadband")]
    pub deadband: f64,
    /// Ceiling on the framing's speed, in fractions of the frame per second.
    #[serde(default = "default_max_speed")]
    pub max_speed: f64,
    /// A target move further than this needs [`confirm`](Self::confirm)
    /// consecutive detections before it is believed.
    #[serde(default = "default_jump")]
    pub jump: f64,
    #[serde(default = "default_confirm")]
    pub confirm: u32,
    /// Ease back to centre after this many seconds with no face. Absent holds
    /// the last framing indefinitely, which is the default.
    #[serde(default)]
    pub recenter_after_s: Option<f64>,
    /// Punch-in per layout, keyed by hyperframes block id. `1.0` is no punch-in.
    ///
    /// Per layout rather than global because the four layouts start with wildly
    /// different amounts of room to move. Split · Horizontal crops a 16:9
    /// camera into a 522-pixel column and has 1398 pixels of travel for free;
    /// Talking Head · Horizontal is full bleed at the camera's own aspect and
    /// has **none**, so tracking there does nothing at all until something
    /// crops it. That is what the default 1.14 on that one block buys, and why
    /// the others ship at 1.0 — they do not need it, and a punch-in costs
    /// resolution.
    #[serde(default = "default_zoom")]
    pub zoom: BTreeMap<String, f64>,
}

fn default_detect_every() -> u32 {
    3
}

fn default_detect_width() -> u32 {
    320
}

fn default_min_confidence() -> f32 {
    0.5
}

fn default_headroom() -> f64 {
    0.45
}

fn default_smoothing_s() -> f64 {
    0.6
}

fn default_deadband() -> f64 {
    0.02
}

fn default_max_speed() -> f64 {
    0.35
}

fn default_jump() -> f64 {
    0.30
}

fn default_confirm() -> u32 {
    2
}

/// 1.14 on the one layout that cannot move without it, 1.0 everywhere else.
///
/// 1.14 renders 1920 columns from about 1685, which is a visible but not
/// obvious tightening, and buys 236 pixels of travel on each axis — enough to
/// follow someone shifting in a chair, not enough to follow them standing up.
/// Anyone who wants a more mobile longform raises it and pays more resolution
/// for it; 1.0 turns the punch-in off and leaves that layout untracked in
/// practice.
fn default_zoom() -> BTreeMap<String, f64> {
    BTreeMap::from([
        ("talking-head-horizontal".to_string(), 1.14),
        ("talking-head-vertical".to_string(), 1.0),
        ("screen-camera-split".to_string(), 1.0),
        ("screen-camera-vertical".to_string(), 1.0),
    ])
}

impl Default for FaceTracking {
    fn default() -> Self {
        FaceTracking {
            enabled: false,
            detect_every: default_detect_every(),
            detect_width: default_detect_width(),
            min_confidence: default_min_confidence(),
            headroom: default_headroom(),
            smoothing_s: default_smoothing_s(),
            deadband: default_deadband(),
            max_speed: default_max_speed(),
            jump: default_jump(),
            confirm: default_confirm(),
            recenter_after_s: None,
            zoom: default_zoom(),
        }
    }
}

impl FaceTracking {
    /// The damping these settings describe.
    pub fn damping(&self) -> crate::track::smooth::Damping {
        crate::track::smooth::Damping {
            smoothing_s: self.smoothing_s,
            deadband: self.deadband,
            max_speed: self.max_speed,
            jump: self.jump,
            confirm: self.confirm,
            recenter_after_s: self.recenter_after_s,
        }
    }

    /// The punch-in for one layout. An unlisted block means none, so a config
    /// written before a layout existed frames it exactly as it frames today
    /// rather than inheriting a number chosen for a different slot.
    pub fn zoom_for(&self, block: &str) -> f64 {
        self.zoom.get(block).copied().unwrap_or(1.0)
    }
}

/// How the vertical screen crop follows the pointer.
///
/// Deliberately *not* a copy of [`FaceTracking`], even though both damp a
/// moving framing, because two of that type's fields answer questions that do
/// not exist here:
///
/// - There is no `detect_every`. A face costs ~1.5 ms to find and so has to be
///   sampled every few frames; the pointer costs **131 ns** (measured — see
///   `crate::pointer::sample`), so it is read on every frame and a knob that
///   would always be 1 is not a knob.
/// - There is no `min_confidence`. The window server does not guess where the
///   pointer is.
///
/// What replaces them is [`rest_zoom`](Self::rest_zoom), which has no analogue
/// on the camera side: the punch-in here is bounded by how much of the display
/// the operator's own region covers, so the feature needs the region framed
/// wide before it has anywhere to go.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct MouseTracking {
    #[serde(default)]
    pub enabled: bool,
    /// What the vertical region's `Placement::zoom` is raised to, once, the
    /// first time tracking is switched on with no room to punch into.
    ///
    /// At 1.0 the region resolves to exactly its 1080×1280 slot — pixel-exact,
    /// and with nowhere to slide or zoom. 1.4 covers 1512×1792 of the display
    /// instead, which buys 432×512 buffer pixels of travel and a 1.4× punch-in
    /// that lands *on* 1:1 rather than past it, so the frame sharpens as it
    /// tightens. The cost is a resting frame that is slightly downsampled,
    /// which is the trade `FaceTracking::zoom`'s 1.14 makes for the same
    /// reason on the one layout that cannot move without it.
    #[serde(default = "default_rest_zoom")]
    pub rest_zoom: f64,
    /// Roughly how long the crop takes to settle after the pointer moves.
    ///
    /// Longer than the camera's 0.6 s on purpose. A face drifts; a pointer
    /// darts, and a frame that kept up with it would be unwatchable.
    #[serde(default = "default_pointer_smoothing_s")]
    pub smoothing_s: f64,
    /// Pointer moves smaller than this fraction of the region are not moves.
    /// Bigger than the camera's deadband because the hand resting on a
    /// trackpad twitches further, relative to the frame, than a face does.
    #[serde(default = "default_pointer_deadband")]
    pub deadband: f64,
    /// Ceiling on the crop's speed, in fractions of the region per second.
    #[serde(default = "default_pointer_max_speed")]
    pub max_speed: f64,
    /// A pointer move further than this needs [`confirm`](Self::confirm)
    /// consecutive samples before the frame believes it — what stops a flick
    /// to the dock and back from yanking the whole frame across.
    #[serde(default = "default_pointer_jump")]
    pub jump: f64,
    #[serde(default = "default_pointer_confirm")]
    pub confirm: u32,
    /// How long the punch-in takes to ease in or out when the ⌃⌥⇧ combo is
    /// pressed or released.
    ///
    /// Short enough to feel like a response to the key rather than a delay,
    /// long enough that the cut does not read as a jump. This is the only
    /// timing the zoom has: there is no dwell to settle and nothing to
    /// detect, because the operator says when.
    #[serde(default = "default_punch_s")]
    pub punch_s: f64,
    /// Ease back to the middle of the region after this long with the pointer
    /// off the captured display. Absent holds the last framing, which is the
    /// default and almost always right: reaching to a second monitor should
    /// not re-centre the take.
    #[serde(default)]
    pub recenter_after_s: Option<f64>,
}

fn default_rest_zoom() -> f64 {
    2.0
}

fn default_pointer_smoothing_s() -> f64 {
    0.9
}

fn default_pointer_deadband() -> f64 {
    0.03
}

fn default_pointer_max_speed() -> f64 {
    0.6
}

fn default_pointer_jump() -> f64 {
    0.35
}

fn default_pointer_confirm() -> u32 {
    2
}

fn default_punch_s() -> f64 {
    0.45
}

impl Default for MouseTracking {
    fn default() -> Self {
        MouseTracking {
            enabled: false,
            rest_zoom: default_rest_zoom(),
            smoothing_s: default_pointer_smoothing_s(),
            deadband: default_pointer_deadband(),
            max_speed: default_pointer_max_speed(),
            jump: default_pointer_jump(),
            confirm: default_pointer_confirm(),
            punch_s: default_punch_s(),
            recenter_after_s: None,
        }
    }
}

impl MouseTracking {
    /// The damping these settings describe.
    pub fn damping(&self) -> crate::track::smooth::Damping {
        crate::track::smooth::Damping {
            smoothing_s: self.smoothing_s,
            deadband: self.deadband,
            max_speed: self.max_speed,
            jump: self.jump,
            confirm: self.confirm,
            recenter_after_s: self.recenter_after_s,
        }
    }
}

/// Where each layout's screen region sits on one display.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DisplayRegions {
    /// The display's point size when these origins were authored.
    ///
    /// The guard that makes an unstable display id safe to key on:
    /// `CGDirectDisplayID`s are reassigned across reboots and re-plugging, so
    /// an id match alone does not mean the same monitor. A region authored on a
    /// 1512x982 laptop screen and replayed onto a 1920x1080 external would land
    /// somewhere nobody chose.
    pub display_points: (f64, f64),
    /// Hyperframes block id to its saved placement.
    ///
    /// **Placements, never rects.** A resolved rect depends on the display's
    /// backing scale, the layout slot's size, and — for Split-Vertical — where
    /// its parent ended up. Saving the rect would freeze all three; saving the
    /// placement re-derives them every launch, so a slot that moves in Figma
    /// still resizes correctly against an origin written months ago.
    #[serde(default)]
    pub placements: BTreeMap<String, SavedPlacement>,
}

/// One region's saved offset and zoom.
///
/// A plain mirror of [`crate::region::placement::Placement`] rather than that
/// type with `Serialize` on it: this is a file format, and it should not change
/// shape just because an internal type gained a field.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct SavedPlacement {
    /// Display-local points for a root region; normalized into the parent's
    /// rect for a child. See [`crate::region::placement::Placement`].
    pub offset: (f64, f64),
    /// Multiplier on the region's 1:1 size. 1.0 is pixel-exact.
    pub zoom: f64,
}

impl Config {
    /// How this layout's region was last placed on this display, if it was
    /// saved against a display of the same size.
    pub fn placement(
        &self,
        display_uid: &str,
        block: &str,
        display_points: (f64, f64),
    ) -> Option<SavedPlacement> {
        let saved = self.screen_regions.get(display_uid)?;
        // Exact equality would be right — both sides come from the same
        // CGDisplayBounds call — but a tolerance costs nothing and stops a
        // future scaled-mode rounding change from silently discarding every
        // saved region at once.
        let same_display = (saved.display_points.0 - display_points.0).abs() < 1.0
            && (saved.display_points.1 - display_points.1).abs() < 1.0;
        same_display
            .then(|| saved.placements.get(block).copied())
            .flatten()
    }

    /// Remember where a region was dragged or scaled to.
    ///
    /// Rewrites `display_points` as well, so a display that changed resolution
    /// re-anchors to the new size on the first edit rather than staying
    /// permanently unreadable.
    pub fn set_placement(
        &mut self,
        display_uid: &str,
        block: &str,
        display_points: (f64, f64),
        placement: SavedPlacement,
    ) {
        let entry = self
            .screen_regions
            .entry(display_uid.to_string())
            .or_insert_with(|| DisplayRegions {
                display_points,
                placements: BTreeMap::new(),
            });
        if entry.display_points != display_points {
            entry.display_points = display_points;
            entry.placements.clear();
        }
        entry.placements.insert(block.to_string(), placement);
    }
}

fn path() -> Result<PathBuf> {
    let home = std::env::var_os("HOME").context("HOME not set")?;
    Ok(PathBuf::from(home)
        .join(".stream-recorder")
        .join("config.json"))
}

pub fn load() -> Config {
    let mut config: Config = path()
        .ok()
        .and_then(|p| std::fs::read_to_string(p).ok())
        .and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or_default();
    config.thumbnail.reconcile_models();
    config
}

pub fn save(cfg: &Config) -> Result<()> {
    let p = path()?;
    if let Some(dir) = p.parent() {
        std::fs::create_dir_all(dir).context("creating ~/.stream-recorder")?;
    }
    std::fs::write(&p, serde_json::to_string_pretty(cfg)?)
        .with_context(|| format!("writing {}", p.display()))
}

#[cfg(test)]
mod thumbnail_tests {
    use super::*;

    fn spec(id: &str) -> crate::thumbnail::image::ModelSpec {
        crate::thumbnail::image::ModelSpec {
            id: id.into(),
            label: id.into(),
        }
    }

    /// The exact config this found on disk: a two-entry menu, one of them the
    /// dead id, and that one selected. Every run 400'd, and correcting the
    /// shipped default could not reach it — a serde default only fills a key
    /// that is missing, and this one was present.
    #[test]
    fn a_saved_config_pointing_at_a_retired_model_heals() {
        let mut thumbnail = Thumbnail {
            models: vec![
                spec("google/gemini-3.1-flash-image"),
                spec("bytedance/seedream-5-0-pro"),
            ],
            model: "bytedance/seedream-5-0-pro".into(),
            ..Thumbnail::default()
        };
        thumbnail.reconcile_models();
        let ids: Vec<&str> = thumbnail.models.iter().map(|m| m.id.as_str()).collect();
        assert!(
            !ids.contains(&"bytedance/seedream-5-0-pro"),
            "the dead entry is gone"
        );
        assert_eq!(thumbnail.model, default_model());
        // And the menu grew rather than shrank: pruning alone left one entry and
        // no way to reach the models that had been added since.
        for shipped in default_models() {
            assert!(
                ids.contains(&shipped.id.as_str()),
                "{} is missing",
                shipped.id
            );
        }
    }

    /// A saved entry that is still real keeps its place, so hand-editing a label
    /// or an order is not undone on the next launch.
    #[test]
    fn a_saved_menu_keeps_its_own_entries_and_their_order() {
        let mut thumbnail = Thumbnail {
            models: vec![crate::thumbnail::image::ModelSpec {
                id: "google/gemini-3.1-flash-image".into(),
                label: "My Favourite".into(),
            }],
            model: "google/gemini-3.1-flash-image".into(),
            ..Thumbnail::default()
        };
        thumbnail.reconcile_models();
        assert_eq!(
            thumbnail.models[0].label, "My Favourite",
            "kept, not overwritten"
        );
        assert_eq!(
            thumbnail.model, "google/gemini-3.1-flash-image",
            "still selected"
        );
    }

    /// Both spellings are gone, and a menu of nothing but dead entries comes back
    /// as the shipped one rather than an empty dropdown that can draw nothing.
    #[test]
    fn a_menu_of_nothing_but_retired_models_is_restored() {
        let mut thumbnail = Thumbnail {
            models: vec![
                spec("bytedance/seedream-5-0-pro"),
                spec("bytedance-seed/seedream-5-0-pro"),
            ],
            model: "bytedance-seed/seedream-5-0-pro".into(),
            ..Thumbnail::default()
        };
        thumbnail.reconcile_models();
        assert_eq!(thumbnail.models, default_models());
        assert_eq!(thumbnail.model, default_model());
    }

    /// Reconciling twice must not keep appending — a launch is not an edit.
    #[test]
    fn reconciling_is_idempotent() {
        let mut thumbnail = Thumbnail::default();
        thumbnail.reconcile_models();
        let once = thumbnail.models.clone();
        thumbnail.reconcile_models();
        assert_eq!(thumbnail.models, once);
    }

    /// Every shipped default has to survive its own retirement check.
    #[test]
    fn the_shipped_defaults_are_not_themselves_retired() {
        for model in default_models() {
            assert!(
                !RETIRED.contains(&model.id.as_str()),
                "{} is dead",
                model.id
            );
        }
        assert!(default_models().iter().any(|m| m.id == default_model()));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const LAPTOP: (f64, f64) = (1512.0, 982.0);
    const EXTERNAL: (f64, f64) = (1920.0, 1080.0);

    fn placement(offset: (f64, f64), zoom: f64) -> SavedPlacement {
        SavedPlacement { offset, zoom }
    }

    fn saved() -> Config {
        let mut cfg = Config::default();
        cfg.set_placement(
            "7",
            "screen-camera-split",
            LAPTOP,
            placement((100.0, 50.0), 1.5),
        );
        cfg
    }

    fn read(cfg: &Config, block: &str, display: (f64, f64)) -> Option<((f64, f64), f64)> {
        cfg.placement("7", block, display)
            .map(|p| (p.offset, p.zoom))
    }

    #[test]
    fn a_saved_placement_comes_back_whole_for_the_same_display() {
        assert_eq!(
            read(&saved(), "screen-camera-split", LAPTOP),
            Some(((100.0, 50.0), 1.5)),
            "zoom must survive the round trip, not just the offset",
        );
    }

    #[test]
    fn a_different_layout_on_the_same_display_has_no_placement_yet() {
        assert_eq!(read(&saved(), "screen-camera-vertical", LAPTOP), None);
    }

    /// The reason `display_points` is stored at all. Display id 7 can be the
    /// laptop panel today and an external monitor after a reboot; replaying the
    /// laptop's placement onto it would put the region somewhere nobody chose.
    #[test]
    fn a_placement_authored_on_a_differently_sized_display_is_discarded() {
        assert_eq!(read(&saved(), "screen-camera-split", EXTERNAL), None);
    }

    #[test]
    fn a_resized_display_re_anchors_rather_than_staying_unreadable() {
        let mut cfg = saved();
        cfg.set_placement(
            "7",
            "screen-camera-split",
            EXTERNAL,
            placement((300.0, 20.0), 1.0),
        );
        assert_eq!(
            read(&cfg, "screen-camera-split", EXTERNAL),
            Some(((300.0, 20.0), 1.0)),
        );
        assert_eq!(
            read(&cfg, "screen-camera-split", LAPTOP),
            None,
            "the old size must not resolve once the display has been re-anchored",
        );
    }

    #[test]
    fn an_unknown_display_has_no_placement() {
        assert_eq!(
            saved()
                .placement("9", "screen-camera-split", LAPTOP)
                .map(|p| p.zoom),
            None,
        );
    }

    /// Old config files predate `screen_regions` entirely, and a recorder that
    /// refused to start because of that would be worse than one that forgets
    /// where a region was.
    #[test]
    fn a_config_without_regions_still_parses() {
        let json = r#"{"audio_device_uid":"a","camera_device_uid":"c"}"#;
        let cfg: Config = serde_json::from_str(json).expect("legacy config parses");
        assert_eq!(cfg.camera_device_uid.as_deref(), Some("c"));
        assert!(cfg.screen_regions.is_empty());
        assert_eq!(cfg.notes_model, None);
    }

    /// Placements survive a real serialize/deserialize, not just the in-memory
    /// setters — this is a file format and the point of it is outliving a
    /// restart.
    #[test]
    fn placements_round_trip_through_json() {
        let text = serde_json::to_string(&saved()).expect("serializes");
        let back: Config = serde_json::from_str(&text).expect("deserializes");
        assert_eq!(
            read(&back, "screen-camera-split", LAPTOP),
            Some(((100.0, 50.0), 1.5)),
        );
    }

    /// Both standing prompts survive a round trip. They are the two inputs a
    /// human types once and expects to still be there next launch.
    #[test]
    fn the_standing_prompts_survive_a_round_trip() {
        let mut cfg = Config::default();
        cfg.posts_prompt = Some("Write for staff engineers".into());
        cfg.thumbnail.brief = crate::thumbnail::Brief {
            title: "SHIP IT ANYWAY".into(),
            description: "Presenter grinning, editor dimmed behind them".into(),
        };
        let text = serde_json::to_string(&cfg).unwrap();
        let back: Config = serde_json::from_str(&text).unwrap();
        assert_eq!(
            back.posts_prompt.as_deref(),
            Some("Write for staff engineers")
        );
        assert_eq!(back.thumbnail.brief.title, "SHIP IT ANYWAY");
        assert!(back
            .thumbnail
            .brief
            .description
            .starts_with("Presenter grinning"));
    }

    /// A config written before either field existed still loads, and simply has
    /// neither. Both are `serde(default)` for exactly this.
    #[test]
    fn a_config_from_before_the_prompts_still_loads() {
        let older: Config = serde_json::from_str(r#"{"audio_device_uid":"mic-1"}"#).unwrap();
        assert_eq!(older.audio_device_uid.as_deref(), Some("mic-1"));
        assert_eq!(older.posts_prompt, None);
        assert!(older.thumbnail.brief.is_empty());
    }
}

/// What Render produces: the horizontal longform, the vertical longform, the
/// shorts. Each is a checkbox above the Render button and remembered here.
///
/// The shorts are the vertical chapter renders, and the vertical longform is
/// those same renders joined — so asking for the vertical longform renders the
/// chapters whether or not the shorts box is on. What the boxes decide is which
/// *outputs* are assembled; a render only draws what is missing or stale, so
/// switching one on after a render costs that output and nothing else.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct RenderTargets {
    #[serde(default = "enabled_by_default")]
    pub horizontal: bool,
    #[serde(default = "enabled_by_default")]
    pub vertical: bool,
    #[serde(default = "enabled_by_default")]
    pub shorts: bool,
}

impl Default for RenderTargets {
    fn default() -> Self {
        RenderTargets {
            horizontal: true,
            vertical: true,
            shorts: true,
        }
    }
}

impl RenderTargets {
    /// Whether Render has anything to do at all.
    pub fn any(self) -> bool {
        self.horizontal || self.vertical || self.shorts
    }

    /// Whether the vertical chapter compositions are needed: as shorts in their
    /// own right, or as the parts the vertical longform is joined from.
    pub fn vertical_parts(self) -> bool {
        self.vertical || self.shorts
    }

    /// Flips one box by the name the pane posts. `false` for a name that is
    /// not a box, so a stray message changes nothing.
    pub fn set(&mut self, name: &str, on: bool) -> bool {
        match name {
            "horizontal" => self.horizontal = on,
            "vertical" => self.vertical = on,
            "shorts" => self.shorts = on,
            _ => return false,
        }
        true
    }

    /// What the pane calls each box.
    pub fn label(name: &str) -> &str {
        match name {
            "horizontal" => "the horizontal longform",
            "vertical" => "the vertical longform",
            "shorts" => "the shorts",
            other => other,
        }
    }
}

#[cfg(test)]
mod render_target_tests {
    use super::*;

    /// A config written before the boxes existed reads as all three on, which
    /// is what every render did until now.
    #[test]
    fn absent_targets_mean_everything_on() {
        let parsed: Config = serde_json::from_str("{}").unwrap();
        assert_eq!(parsed.render, RenderTargets::default());
        assert!(parsed.render.any() && parsed.render.vertical_parts());
        let partial: RenderTargets = serde_json::from_str(r#"{"shorts": false}"#).unwrap();
        assert_eq!(
            partial,
            RenderTargets {
                horizontal: true,
                vertical: true,
                shorts: false
            }
        );
    }

    /// The vertical longform is the shorts joined, so it needs them rendered
    /// whether or not they are wanted as outputs themselves.
    #[test]
    fn the_vertical_longform_needs_the_chapter_renders() {
        let mut targets = RenderTargets::default();
        assert!(targets.set("shorts", false));
        assert!(targets.vertical_parts());
        assert!(targets.set("vertical", false));
        assert!(!targets.vertical_parts());
        assert!(targets.any());
        assert!(targets.set("horizontal", false));
        assert!(!targets.any());
        assert!(!targets.set("audio", true));
        assert_eq!(RenderTargets::label("shorts"), "the shorts");
    }
}
