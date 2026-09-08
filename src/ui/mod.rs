//! Native AppKit controls inside the winit record window.
//!
//! Five primary steps: video recording, thumbnails, YouTube, Blog (Strapi),
//! and Socials. Recording groups capture/render/review; Socials groups writing,
//! media upload, Buffer scheduling, analytics and prompt review.
//! The navigation contract lives in [`workflow`].

use std::cell::{Cell, RefCell};
use std::ops::Range;
use std::sync::mpsc::{channel, Receiver, Sender};

use anyhow::{anyhow, Result};
use objc2::rc::Retained;
use objc2::runtime::{AnyObject, NSObjectProtocol, Sel};
use objc2::{define_class, msg_send, sel, AnyThread, DefinedClass, MainThreadMarker, MainThreadOnly};
use objc2_app_kit::{
    NSAlert, NSAutoresizingMaskOptions, NSBorderType, NSBox, NSButton, NSButtonType,
    NSControlStateValueOff, NSControlStateValueOn,
    NSFont, NSFontWeight, NSLevelIndicator,
    NSLevelIndicatorStyle, NSPopUpButton, NSProgressIndicator,
    NSProgressIndicatorStyle, NSScrollView,
    NSSplitView, NSSplitViewDividerStyle, NSTabView, NSTabViewItem, NSTabViewType,
    NSTextField, NSTextView, NSTitlePosition, NSView,
};
use objc2_foundation::{NSObject, NSPoint, NSRect, NSSize, NSString};
use raw_window_handle::{HasWindowHandle, RawWindowHandle};
use winit::window::Window;

use crate::capture::device_picker::CaptureDevice;
use crate::hotkeys::Action;
use crate::layouts::{Orientation, Pair};
use crate::region::PointRect;

/// Which queue [`ControlTarget::confirm_clear`] was told to empty.
///
/// Mirrors `schedule::clear::Scope` rather than using it, so the AppKit layer
/// does not reach into the scheduling stage to describe a button press.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClearChoice {
    Project,
    Everything,
}

/// The deepest directory containing both paths.
///
/// For scoping a webview's file read access to exactly the trees a pane draws
/// from, and nothing wider. Falls back to `/` only when two paths share no
/// prefix at all, which on one filesystem they always do.
pub fn common_ancestor(a: &std::path::Path, b: &std::path::Path) -> std::path::PathBuf {
    let mut shared = std::path::PathBuf::new();
    for (left, right) in a.components().zip(b.components()) {
        if left != right {
            break;
        }
        shared.push(left);
    }
    if shared.as_os_str().is_empty() {
        std::path::PathBuf::from("/")
    } else {
        shared
    }
}

/// A `file://` URL a webview pane can load, percent-encoded.
///
/// Every pane that shows something off disk needs this, and needs it to agree with
/// every other pane: a real project path can hold a space, and a space ends a URL, so
/// the naive `format!("file://{}", path.display())` renders as a broken image or a
/// video that never loads — with nothing on screen saying why.
pub fn file_url(path: &std::path::Path) -> String {
    use std::os::unix::ffi::OsStrExt;

    let mut out = String::from("file://");
    for byte in path.as_os_str().as_bytes() {
        match byte {
            b'/' | b'-' | b'_' | b'.' | b'~' => out.push(*byte as char),
            byte if byte.is_ascii_alphanumeric() => out.push(*byte as char),
            byte => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}

/// Puts `text` on the system pasteboard, reporting whether it landed.
///
/// A native write rather than `navigator.clipboard` inside the pane. The async
/// Clipboard API wants a secure context and a pane is a `file://` page in a
/// `WKWebView`, so a copy button built that way works in a browser, silently
/// does nothing here, and says nothing about why.
pub fn copy_to_pasteboard(text: &str) -> bool {
    if MainThreadMarker::new().is_none() {
        return false;
    }
    let pasteboard = objc2_app_kit::NSPasteboard::generalPasteboard();
    // Mandatory before writing. Without it the new value joins whatever the last
    // owner declared, and the paste can come back as that older type instead.
    pasteboard.clearContents();
    unsafe {
        pasteboard.setString_forType(
            &NSString::from_str(text),
            objc2_app_kit::NSPasteboardTypeString,
        )
    }
}

mod preview;
pub mod render;
mod workflow;
pub mod web;
pub use preview::PreviewHost;
pub use web::WebPane;

/// Everything the window's controls can ask of the app.
pub enum UiEvent {
    Action(Action),
    CameraSelected(usize),
    MicSelected(usize),
    ScreenSelected(Option<usize>),
    PairSelected(usize),
    /// The Track Face checkbox moved.
    FaceTrackToggled(bool),
    /// The Track Mouse checkbox moved.
    MouseTrackToggled(bool),
    /// The YouTube tab's Visibility popup moved, as an index into
    /// [`crate::publish::youtube::Privacy::ALL`].
    YoutubePrivacySelected(usize),
    /// The background detector build finished. Posted from a worker thread, not
    /// from a control — the only [`UiEvent`] that is, and the reason this enum
    /// has to stay `Send`. See [`crate::face::FaceTracker::spawn`].
    FaceTrackReady(Result<std::sync::Arc<crate::face::FaceTracker>, String>),
    ModelSelected(usize),
    ProviderSelected(usize),
    PromptChanged(String),
    PostsModelSelected(usize),
    PostsProviderSelected(usize),
    PostsPromptChanged(String),
    VersionSelected(u32),
    ProjectSelected(usize),
    ValidateRewrite(usize),
    /// A checkbox in an HTML pane moved, carrying its new value.
    WebApprove { index: usize, value: bool },
    SaveBrief(std::collections::BTreeMap<String, String>),
    /// The Settings form, carrying only the boxes that were filled in.
    SaveSettings(std::collections::BTreeMap<String, String>),
    /// Test one group of credentials, by `settings::Group::slug`.
    TestSettings(String),
    /// A finished credential test, posted from the worker thread that ran it.
    /// Like [`UiEvent::FaceTrackReady`] this does not come from a control.
    SettingsTested(crate::settings::check::Outcome),
    /// The card's boxes, whole. Separate from [`UiEvent::SaveBrief`] even though
    /// both carry a `title` and a `description`: the two words mean different
    /// things on each form — see [`crate::card::Card`] — and one message would
    /// let a brief's stage directions land on the drawn thumbnail.
    SaveCard(std::collections::BTreeMap<String, String>),
    SaveYoutube(std::collections::BTreeMap<String, String>),
    SaveVideoBrief { fields: std::collections::BTreeMap<String, String>, apply: bool },
    GenerateVideoCopy(std::collections::BTreeMap<String, String>),
    GenerateArtwork(std::collections::BTreeMap<String, String>),
    ImportPortrait(String),
    SelectThumbnail(String),
    ThumbnailModelSelected(String),
    /// A Strapi relation id, or empty for none.
    BlogAuthorSelected(String),
    BlogCategorySelected(String),
    ToggleReference { name: String, value: bool },
    AddReference { name: String, data: String },
    RemoveReference(String),
    ProjectNameChanged(String),
    /// Words a pane asked to be put on the system pasteboard.
    CopyText(String),
    /// Which chapter the Edit tab is showing.
    OpenChapter(u32),
    /// A hand-edited keep-list, in `[start_ms, end_ms]` pairs.
    SaveEdit { chapter: u32, spans: Vec<[i64; 2]> },
    ApplyEdit(u32),
    ResetEdit(u32),
    ToggleRegions,
    RegionPlaced {
        orientation: Orientation,
        rect: PointRect,
    },
    /// A figure's rectangle, in the display-local points the snip overlay draws
    /// in — see [`crate::figure::snip`].
    FigureSnipped {
        rect: PointRect,
    },
    /// The snip was abandoned: a click that did not travel, or a right-click.
    /// Its own event rather than a `None` rect, because the App has a window to
    /// take down either way and silence would leave it up.
    FigureSnipCancelled,
}

/// The screen dropdown's leading entry, at index 0, ahead of every display.
const NO_SCREEN: &str = "No screen (camera only)";

pub const WINDOW_WIDTH: u32 = 1100;
pub const WINDOW_HEIGHT: u32 = 820;
pub const WINDOW_MIN_WIDTH: u32 = 880;
pub const WINDOW_MIN_HEIGHT: u32 = 600;
const NOTES_DEFAULT_W: f64 = 400.0;

/// Layout metrics, all in AppKit points.
const PAD: f64 = 12.0;
const LABEL_H: f64 = 16.0;
const CONTROL_H: f64 = 26.0;
const BUTTON_H: f64 = 26.0;
const GAP: f64 = 4.0;
const SECTION_GAP: f64 = 10.0;
/// The recording clock, a line taller than the labels around it because it is
/// read from across the room while talking.
const TIMER_H: f64 = 22.0;
/// The input meter's width in the detail row beside the chapter's figures.
const METER_W: f64 = 90.0;
const BUTTON_GAP: f64 = 4.0;
const GROUP_H: f64 = 114.0;
const PROMPT_H: f64 = 52.0;

pub struct ControlTargetIvars {
    tx: Sender<UiEvent>,
    camera_popup: RefCell<Option<Retained<NSPopUpButton>>>,
    mic_popup: RefCell<Option<Retained<NSPopUpButton>>>,
    screen_popup: RefCell<Option<Retained<NSPopUpButton>>>,
    pair_popup: RefCell<Option<Retained<NSPopUpButton>>>,
    /// The Track Face switch, which sits beside the Layout popup because they
    /// are the same kind of decision: both change what the recorded frame
    /// contains, and both are set before a take rather than during one.
    face_checkbox: RefCell<Option<Retained<NSButton>>>,
    /// The Track Mouse switch, beside it. Same kind of decision, one fewer
    /// state — there is nothing to load, so it is never disabled.
    mouse_checkbox: RefCell<Option<Retained<NSButton>>>,
    /// The YouTube tab's Visibility picker.
    privacy_popup: RefCell<Option<Retained<NSPopUpButton>>>,
    version_popup: RefCell<Option<Retained<NSPopUpButton>>>,
    project_popup: RefCell<Option<Retained<NSPopUpButton>>>,
    project_name: RefCell<Option<Retained<NSTextField>>>,
    model_popup: RefCell<Option<Retained<NSPopUpButton>>>,
    provider_popup: RefCell<Option<Retained<NSPopUpButton>>>,
    prompt_field: RefCell<Option<Retained<NSTextField>>>,
    chapter_button: RefCell<Option<Retained<NSButton>>>,
    retake_button: RefCell<Option<Retained<NSButton>>>,
    regions_button: RefCell<Option<Retained<NSButton>>>,
    status: RefCell<Option<Retained<NSTextField>>>,
    /// The recording clock and the speech-time ballpark. See `app::clock`.
    timer: RefCell<Option<Retained<NSTextField>>>,
    timer_detail: RefCell<Option<Retained<NSTextField>>>,
    /// Live input level, which is also what makes the speech figure believable:
    /// a meter sitting at the bottom explains a speech clock that never moves.
    meter: RefCell<Option<Retained<NSLevelIndicator>>>,

    /// The one button that starts each pipeline stage, held so [`set_stage_gates`]
    /// can switch it off while its inputs are missing.
    ///
    /// [`set_stage_gates`]: ControlTarget::set_stage_gates
    titles_button: RefCell<Option<Retained<NSButton>>>,
    render_button: RefCell<Option<Retained<NSButton>>>,
    posts_button: RefCell<Option<Retained<NSButton>>>,
    distribute_button: RefCell<Option<Retained<NSButton>>>,
    plan_button: RefCell<Option<Retained<NSButton>>>,
    approve_button: RefCell<Option<Retained<NSButton>>>,
    queue_button: RefCell<Option<Retained<NSButton>>>,
    publish_button: RefCell<Option<Retained<NSButton>>>,

    // Render tab
    render_status: RefCell<Option<Retained<NSTextField>>>,
    thumbnail_status: RefCell<Option<Retained<NSTextField>>>,
    render_summary: RefCell<Option<Retained<NSTextView>>>,

    // Post tab
    posts_model_popup: RefCell<Option<Retained<NSPopUpButton>>>,
    posts_provider_popup: RefCell<Option<Retained<NSPopUpButton>>>,
    posts_prompt_view: RefCell<Option<Retained<NSTextView>>>,
    posts_status: RefCell<Option<Retained<NSTextField>>>,

    // Distribute tab (S3)
    distribute_status: RefCell<Option<Retained<NSTextField>>>,
    distribute_info: RefCell<Option<Retained<NSTextView>>>,
    distribute_bar: RefCell<Option<Retained<NSProgressIndicator>>>,

    // YouTube tab
    publish_status: RefCell<Option<Retained<NSTextField>>>,

    // Schedule tab
    schedule_status: RefCell<Option<Retained<NSTextField>>>,
    schedule_info: RefCell<Option<Retained<NSTextView>>>,

    // Edit tab
    edit_status: RefCell<Option<Retained<NSTextField>>>,

    // Reflect tab
    reflect_status: RefCell<Option<Retained<NSTextField>>>,

    // Substack tab
    substack_status: RefCell<Option<Retained<NSTextField>>>,
    blog_status: RefCell<Option<Retained<NSTextField>>>,

    // Analytics tab
    analytics_tab: RefCell<Option<Retained<NSTabViewItem>>>,
    analytics_status: RefCell<Option<Retained<NSTextField>>>,
    analytics_info: RefCell<Option<Retained<NSTextView>>>,
}

define_class!(
    #[unsafe(super(NSObject))]
    #[ivars = ControlTargetIvars]
    pub struct ControlTarget;

    unsafe impl NSObjectProtocol for ControlTarget {}

    impl ControlTarget {
        #[unsafe(method(onNewChapter:))]
        fn on_new_chapter(&self, _sender: Option<&AnyObject>) {
            let _ = self.ivars().tx.send(UiEvent::Action(Action::NewChapter));
        }

        #[unsafe(method(onRetake:))]
        fn on_retake(&self, _sender: Option<&AnyObject>) {
            let _ = self.ivars().tx.send(UiEvent::Action(Action::Retake));
        }

        #[unsafe(method(onStop:))]
        fn on_stop(&self, _sender: Option<&AnyObject>) {
            let _ = self.ivars().tx.send(UiEvent::Action(Action::Stop));
        }

        #[unsafe(method(onCameraChanged:))]
        fn on_camera_changed(&self, _sender: Option<&AnyObject>) {
            if let Some(popup) = self.ivars().camera_popup.borrow().as_ref() {
                let idx = popup.indexOfSelectedItem();
                if idx >= 0 {
                    let _ = self.ivars().tx.send(UiEvent::CameraSelected(idx as usize));
                }
            }
        }

        #[unsafe(method(onMicChanged:))]
        fn on_mic_changed(&self, _sender: Option<&AnyObject>) {
            if let Some(popup) = self.ivars().mic_popup.borrow().as_ref() {
                let idx = popup.indexOfSelectedItem();
                if idx >= 0 {
                    let _ = self.ivars().tx.send(UiEvent::MicSelected(idx as usize));
                }
            }
        }

        #[unsafe(method(onScreenChanged:))]
        fn on_screen_changed(&self, _sender: Option<&AnyObject>) {
            if let Some(popup) = self.ivars().screen_popup.borrow().as_ref() {
                let idx = popup.indexOfSelectedItem();
                let selection = match idx {
                    ..=-1 => return,
                    0 => None,
                    n => Some(n as usize - 1),
                };
                let _ = self.ivars().tx.send(UiEvent::ScreenSelected(selection));
            }
        }

        #[unsafe(method(onFaceTrackChanged:))]
        fn on_face_track_changed(&self, _sender: Option<&AnyObject>) {
            if let Some(checkbox) = self.ivars().face_checkbox.borrow().as_ref() {
                let on = checkbox.state() == NSControlStateValueOn;
                let _ = self.ivars().tx.send(UiEvent::FaceTrackToggled(on));
            }
        }

        #[unsafe(method(onMouseTrackChanged:))]
        fn on_mouse_track_changed(&self, _sender: Option<&AnyObject>) {
            if let Some(checkbox) = self.ivars().mouse_checkbox.borrow().as_ref() {
                let on = checkbox.state() == NSControlStateValueOn;
                let _ = self.ivars().tx.send(UiEvent::MouseTrackToggled(on));
            }
        }

        #[unsafe(method(onPairChanged:))]
        fn on_pair_changed(&self, _sender: Option<&AnyObject>) {
            if let Some(popup) = self.ivars().pair_popup.borrow().as_ref() {
                let idx = popup.indexOfSelectedItem();
                if idx >= 0 {
                    let _ = self.ivars().tx.send(UiEvent::PairSelected(idx as usize));
                }
            }
        }

        #[unsafe(method(onVersionChanged:))]
        fn on_version_changed(&self, _sender: Option<&AnyObject>) {
            if let Some(popup) = self.ivars().version_popup.borrow().as_ref() {
                let Some(title) = popup.titleOfSelectedItem() else {
                    return;
                };
                let title = title.to_string();
                if let Some(n) = title
                    .strip_prefix('v')
                    .and_then(|s| s.split_whitespace().next())
                    .and_then(|s| s.parse::<u32>().ok())
                {
                    let _ = self.ivars().tx.send(UiEvent::VersionSelected(n));
                }
            }
        }

        #[unsafe(method(onModelChanged:))]
        fn on_model_changed(&self, _sender: Option<&AnyObject>) {
            if let Some(popup) = self.ivars().model_popup.borrow().as_ref() {
                let idx = popup.indexOfSelectedItem();
                if idx >= 0 {
                    let _ = self.ivars().tx.send(UiEvent::ModelSelected(idx as usize));
                }
            }
        }

        #[unsafe(method(onProviderChanged:))]
        fn on_provider_changed(&self, _sender: Option<&AnyObject>) {
            if let Some(popup) = self.ivars().provider_popup.borrow().as_ref() {
                let idx = popup.indexOfSelectedItem();
                if idx >= 0 {
                    let _ = self.ivars().tx.send(UiEvent::ProviderSelected(idx as usize));
                }
            }
        }

        #[unsafe(method(onPromptChanged:))]
        fn on_prompt_changed(&self, _sender: Option<&AnyObject>) {
            self.emit_prompt();
        }

        #[unsafe(method(controlTextDidEndEditing:))]
        fn control_text_did_end_editing(&self, _notif: Option<&AnyObject>) {
            self.emit_prompt();
        }

        #[unsafe(method(onToggleRegions:))]
        fn on_toggle_regions(&self, _sender: Option<&AnyObject>) {
            let _ = self.ivars().tx.send(UiEvent::ToggleRegions);
        }

        #[unsafe(method(onNotes:))]
        fn on_notes(&self, _sender: Option<&AnyObject>) {
            let _ = self.ivars().tx.send(UiEvent::Action(Action::Notes));
        }

        #[unsafe(method(onCopyTranscript:))]
        fn on_copy_transcript(&self, _sender: Option<&AnyObject>) {
            let _ = self.ivars().tx.send(UiEvent::Action(Action::CopyTranscript));
        }

        #[unsafe(method(onProjectChanged:))]
        fn on_project_changed(&self, _sender: Option<&AnyObject>) {
            if let Some(popup) = self.ivars().project_popup.borrow().as_ref() {
                let idx = popup.indexOfSelectedItem();
                if idx >= 0 {
                    let _ = self.ivars().tx.send(UiEvent::ProjectSelected(idx as usize));
                }
            }
        }

        #[unsafe(method(onProjectNameChanged:))]
        fn on_project_name_changed(&self, _sender: Option<&AnyObject>) {
            if let Some(field) = self.ivars().project_name.borrow().as_ref() {
                let text = field.stringValue().to_string();
                let _ = self.ivars().tx.send(UiEvent::ProjectNameChanged(text));
            }
        }

        #[unsafe(method(onCollectAllAnalytics:))]
        fn on_collect_all_analytics(&self, _sender: Option<&AnyObject>) {
            let _ = self
                .ivars()
                .tx
                .send(UiEvent::Action(Action::CollectAllAnalytics));
        }

        #[unsafe(method(onReflect:))]
        fn on_reflect(&self, _sender: Option<&AnyObject>) {
            let _ = self.ivars().tx.send(UiEvent::Action(Action::Reflect));
        }

        #[unsafe(method(onApplyRewrites:))]
        fn on_apply_rewrites(&self, _sender: Option<&AnyObject>) {
            let _ = self.ivars().tx.send(UiEvent::Action(Action::ApplyRewrites));
        }

        /// One selector serves every row: the button's tag is its index.
        #[unsafe(method(onValidateRewrite:))]
        fn on_validate_rewrite(&self, sender: Option<&AnyObject>) {
            let Some(sender) = sender else { return };
            let tag: isize = unsafe { msg_send![sender, tag] };
            if tag >= 0 {
                let _ = self
                    .ivars()
                    .tx
                    .send(UiEvent::ValidateRewrite(tag as usize));
            }
        }

        #[unsafe(method(onPullAnalytics:))]
        fn on_pull_analytics(&self, _sender: Option<&AnyObject>) {
            let _ = self.ivars().tx.send(UiEvent::Action(Action::PullAnalytics));
        }

        #[unsafe(method(onNewProject:))]
        fn on_new_project(&self, _sender: Option<&AnyObject>) {
            let _ = self.ivars().tx.send(UiEvent::Action(Action::NewProject));
        }

        #[unsafe(method(onNewVersion:))]
        fn on_new_version(&self, _sender: Option<&AnyObject>) {
            let _ = self.ivars().tx.send(UiEvent::Action(Action::NewVersion));
        }

        #[unsafe(method(onRunRender:))]
        fn on_run_render(&self, _sender: Option<&AnyObject>) {
            let _ = self.ivars().tx.send(UiEvent::Action(Action::Render));
        }

        #[unsafe(method(onGenerateTitles:))]
        fn on_generate_titles(&self, _sender: Option<&AnyObject>) {
            let _ = self.ivars().tx.send(UiEvent::Action(Action::GenerateTitles));
        }

        #[unsafe(method(onGeneratePosts:))]
        fn on_generate_posts(&self, _sender: Option<&AnyObject>) {
            let _ = self.ivars().tx.send(UiEvent::Action(Action::GeneratePosts));
        }

        #[unsafe(method(onSavePosts:))]
        fn on_save_posts(&self, _sender: Option<&AnyObject>) {
            let _ = self.ivars().tx.send(UiEvent::Action(Action::SavePosts));
        }

        #[unsafe(method(onDistribute:))]
        fn on_distribute(&self, _sender: Option<&AnyObject>) {
            let _ = self.ivars().tx.send(UiEvent::Action(Action::Distribute));
        }

        #[unsafe(method(onSchedulePlan:))]
        fn on_schedule_plan(&self, _sender: Option<&AnyObject>) {
            let _ = self.ivars().tx.send(UiEvent::Action(Action::SchedulePlan));
        }

        #[unsafe(method(onScheduleApproveAll:))]
        fn on_schedule_approve_all(&self, _sender: Option<&AnyObject>) {
            let _ = self
                .ivars()
                .tx
                .send(UiEvent::Action(Action::ScheduleApproveAll));
        }

        #[unsafe(method(onScheduleClear:))]
        fn on_schedule_clear(&self, _sender: Option<&AnyObject>) {
            let _ = self.ivars().tx.send(UiEvent::Action(Action::ScheduleClear));
        }

        #[unsafe(method(onYoutubePrivacyChanged:))]
        fn on_youtube_privacy_changed(&self, _sender: Option<&AnyObject>) {
            if let Some(popup) = self.ivars().privacy_popup.borrow().as_ref() {
                let idx = popup.indexOfSelectedItem();
                if idx >= 0 {
                    let _ = self
                        .ivars()
                        .tx
                        .send(UiEvent::YoutubePrivacySelected(idx as usize));
                }
            }
        }

        #[unsafe(method(onYoutubeUpload:))]
        fn on_youtube_upload(&self, _sender: Option<&AnyObject>) {
            let _ = self.ivars().tx.send(UiEvent::Action(Action::YoutubeUpload));
        }

        #[unsafe(method(onYoutubeConnect:))]
        fn on_youtube_connect(&self, _sender: Option<&AnyObject>) {
            let _ = self.ivars().tx.send(UiEvent::Action(Action::ConnectYoutube));
        }

        #[unsafe(method(onScheduleQueue:))]
        fn on_schedule_queue(&self, _sender: Option<&AnyObject>) {
            let _ = self.ivars().tx.send(UiEvent::Action(Action::ScheduleQueue));
        }

        #[unsafe(method(onPostsModelChanged:))]
        fn on_posts_model_changed(&self, _sender: Option<&AnyObject>) {
            if let Some(popup) = self.ivars().posts_model_popup.borrow().as_ref() {
                let idx = popup.indexOfSelectedItem();
                if idx >= 0 {
                    let _ = self.ivars().tx.send(UiEvent::PostsModelSelected(idx as usize));
                }
            }
        }

        #[unsafe(method(onPostsProviderChanged:))]
        fn on_posts_provider_changed(&self, _sender: Option<&AnyObject>) {
            if let Some(popup) = self.ivars().posts_provider_popup.borrow().as_ref() {
                let idx = popup.indexOfSelectedItem();
                if idx >= 0 {
                    let _ = self.ivars().tx.send(UiEvent::PostsProviderSelected(idx as usize));
                }
            }
        }

        /// The posts prompt is a text area, so it has no target/action to fire:
        /// Return types a newline rather than committing. This is the delegate
        /// callback the field sends when focus leaves it, and it is what makes
        /// the standing instruction survive a relaunch. Pressing Generate does
        /// not move focus, so `run_generate_posts` writes it back as well.
        #[unsafe(method(textDidEndEditing:))]
        fn text_did_end_editing(&self, _notification: Option<&AnyObject>) {
            self.emit_posts_prompt();
        }
    }
);

impl ControlTarget {
    fn new(tx: Sender<UiEvent>) -> Retained<Self> {
        let this = Self::alloc().set_ivars(ControlTargetIvars {
            tx,
            camera_popup: RefCell::new(None),
            mic_popup: RefCell::new(None),
            screen_popup: RefCell::new(None),
            pair_popup: RefCell::new(None),
            face_checkbox: RefCell::new(None),
            mouse_checkbox: RefCell::new(None),
            privacy_popup: RefCell::new(None),
            version_popup: RefCell::new(None),
            project_popup: RefCell::new(None),
            project_name: RefCell::new(None),
            model_popup: RefCell::new(None),
            provider_popup: RefCell::new(None),
            prompt_field: RefCell::new(None),
            chapter_button: RefCell::new(None),
            retake_button: RefCell::new(None),
            regions_button: RefCell::new(None),
            status: RefCell::new(None),
            timer: RefCell::new(None),
            timer_detail: RefCell::new(None),
            meter: RefCell::new(None),
            titles_button: RefCell::new(None),
            render_button: RefCell::new(None),
            posts_button: RefCell::new(None),
            distribute_button: RefCell::new(None),
            publish_button: RefCell::new(None),
            publish_status: RefCell::new(None),
            plan_button: RefCell::new(None),
            approve_button: RefCell::new(None),
            queue_button: RefCell::new(None),
            render_status: RefCell::new(None),
            thumbnail_status: RefCell::new(None),
            render_summary: RefCell::new(None),
            posts_model_popup: RefCell::new(None),
            posts_provider_popup: RefCell::new(None),
            posts_prompt_view: RefCell::new(None),
            posts_status: RefCell::new(None),
            distribute_status: RefCell::new(None),
            distribute_info: RefCell::new(None),
            distribute_bar: RefCell::new(None),
            schedule_status: RefCell::new(None),
            schedule_info: RefCell::new(None),
            edit_status: RefCell::new(None),
            reflect_status: RefCell::new(None),
            substack_status: RefCell::new(None),
            blog_status: RefCell::new(None),
            analytics_tab: RefCell::new(None),
            analytics_status: RefCell::new(None),
            analytics_info: RefCell::new(None),
        });
        unsafe { msg_send![super(this), init] }
    }

    fn emit_prompt(&self) {
        let Some(field) = self.ivars().prompt_field.borrow().clone() else {
            return;
        };
        let text = field.stringValue().to_string();
        let _ = self.ivars().tx.send(UiEvent::PromptChanged(text));
    }

    fn emit_posts_prompt(&self) {
        let Some(view) = self.ivars().posts_prompt_view.borrow().clone() else {
            return;
        };
        let text = view.string().to_string();
        let _ = self.ivars().tx.send(UiEvent::PostsPromptChanged(text));
    }

    /// Repopulates the project picker. `selected` is the folder name of the
    /// project currently open, so a rename or a new project keeps the right row
    /// highlighted.
    pub fn set_projects(&self, projects: &[crate::sessions::SessionEntry], selected: &str) {
        if MainThreadMarker::new().is_none() {
            return;
        }
        let Some(popup) = self.ivars().project_popup.borrow().clone() else {
            return;
        };
        popup.removeAllItems();
        for project in projects {
            popup.addItemWithTitle(&NSString::from_str(&project.label()));
        }
        if projects.is_empty() {
            popup.addItemWithTitle(&NSString::from_str("(new project)"));
        }
        if let Some(at) = projects.iter().position(|p| p.folder == selected) {
            popup.selectItemAtIndex(at as isize);
        }
    }

    /// Shows the open project's name without firing the field's action.
    pub fn set_project_name(&self, name: &str) {
        if MainThreadMarker::new().is_none() {
            return;
        }
        if let Some(field) = self.ivars().project_name.borrow().clone() {
            field.setStringValue(&NSString::from_str(name));
        }
    }

    pub fn set_versions(&self, versions: &[crate::session::VersionInfo], selected: Option<u32>) {
        if MainThreadMarker::new().is_none() {
            return;
        }
        let Some(popup) = self.ivars().version_popup.borrow().clone() else {
            return;
        };
        popup.removeAllItems();
        for version in versions {
            popup.addItemWithTitle(&NSString::from_str(&version.label()));
        }
        // An unversioned project selects nothing rather than falling through to
        // row 0. `removeAllItems` leaves the first item selected, so a project
        // whose takes sit flat in `drafts/` used to read as though v1 were open
        // while every path in the app was the flat one.
        let index = selected
            .and_then(|n| versions.iter().position(|v| v.n == n))
            .map(|idx| idx as isize)
            .unwrap_or(-1);
        popup.selectItemAtIndex(index);
    }

    /// Asks which queue to empty, or `None` for "leave it alone".
    ///
    /// A modal rather than a status line, and the destructive answers are not the
    /// default button: deleting a Buffer post cannot be undone from here, and the
    /// wider of the two scopes reaches posts this app never made.
    pub fn confirm_clear(&self, project: usize, project_name: &str) -> Option<ClearChoice> {
        let mtm = MainThreadMarker::new()?;
        let alert = NSAlert::new(mtm);
        alert.setMessageText(&NSString::from_str("Delete queued posts from Buffer?"));
        alert.setInformativeText(&NSString::from_str(&format!(
            "“{project_name}” has {project} post(s) still queued at Buffer.\n\n\
             Deleting is permanent — Buffer has no undo, and anything already \
             published stays published.\n\n\
             Clearing the whole queue also deletes posts this app never made.",
        )));
        // Order matters: the first button is the default and takes Return.
        alert.addButtonWithTitle(&NSString::from_str("Cancel"));
        alert.addButtonWithTitle(&NSString::from_str(&format!(
            "Delete this project's {project}"
        )));
        alert.addButtonWithTitle(&NSString::from_str("Delete the whole Buffer queue"));
        match alert.runModal() {
            // NSAlertFirstButtonReturn is 1000, and they count up from there.
            n if n == 1001 => Some(ClearChoice::Project),
            n if n == 1002 => Some(ClearChoice::Everything),
            _ => None,
        }
    }

    /// Switches each pipeline button on or off from [`crate::stage::Stages`].
    ///
    /// The button is the whole signal here; the reason a blocked one gives is
    /// written into its tab's status line by the caller, which owns that text.
    pub fn set_stage_gates(&self, stages: &crate::stage::Stages) {
        if MainThreadMarker::new().is_none() {
            return;
        }
        let ivars = self.ivars();
        for (button, gate) in [
            (&ivars.titles_button, &stages.titles),
            (&ivars.render_button, &stages.render),
            (&ivars.posts_button, &stages.posts),
            (&ivars.distribute_button, &stages.distribute),
            (&ivars.plan_button, &stages.plan),
            (&ivars.approve_button, &stages.approve),
            (&ivars.queue_button, &stages.queue),
            (&ivars.publish_button, &stages.publish),
        ] {
            if let Some(button) = button.borrow().clone() {
                button.setEnabled(gate.is_ready());
            }
        }
    }

    /// Put the Track Face switch into the state the app is actually in.
    ///
    /// Three states in two AppKit properties: off, loading (checked but
    /// disabled, and saying so), and on. The disable during a load is not
    /// decoration — clicking again mid-build is the one input that would start
    /// a second 34 MB download, and it is cheaper to make it unclickable than
    /// to reason about the race.
    pub fn set_face_tracking(&self, on: bool, loading: bool) {
        if MainThreadMarker::new().is_none() {
            return;
        }
        let Some(checkbox) = self.ivars().face_checkbox.borrow().clone() else {
            return;
        };
        checkbox.setState(if on {
            NSControlStateValueOn
        } else {
            NSControlStateValueOff
        });
        checkbox.setTitle(&NSString::from_str(if loading {
            "Track Face — loading…"
        } else {
            "Track Face"
        }));
        checkbox.setEnabled(!loading);
    }

    pub fn set_regions(&self, has_screen: bool, visible: bool) {
        if MainThreadMarker::new().is_none() {
            return;
        }
        let Some(button) = self.ivars().regions_button.borrow().clone() else {
            return;
        };
        let title = match (has_screen, visible) {
            (false, _) => "No Screen In This Layout",
            (true, false) => "Show Regions",
            (true, true) => "Hide Regions",
        };
        button.setTitle(&NSString::from_str(title));
        button.setEnabled(has_screen);
    }

    /// `pending` names a layout picked mid-take that the next chapter will start
    /// in. It rides here rather than in its own field because this method owns
    /// the status line, and two writers would just overwrite each other.
    pub fn set_recording(
        &self,
        chapter: Option<u32>,
        version: Option<u32>,
        pending: Option<&str>,
    ) {
        if MainThreadMarker::new().is_none() {
            return;
        }
        let Some(button) = self.ivars().chapter_button.borrow().clone() else {
            return;
        };
        let Some(retake) = self.ivars().retake_button.borrow().clone() else {
            return;
        };
        let Some(status) = self.ivars().status.borrow().clone() else {
            return;
        };
        match chapter {
            None => {
                button.setTitle(&NSString::from_str("Start Recording"));
                retake.setEnabled(false);
                let v = version.map(|n| format!("v{n}")).unwrap_or_default();
                status.setStringValue(&NSString::from_str(&format!(
                    "Ready to record {v} — press Start Recording"
                )));
            }
            Some(n) => {
                button.setTitle(&NSString::from_str(match pending {
                    Some(_) => "New Chapter ▸ new layout",
                    None => "New Chapter",
                }));
                retake.setEnabled(true);
                let v = version.map(|n| format!("v{n} ")).unwrap_or_default();
                let waiting = pending
                    .map(|layout| format!(" — {layout} starts at the next chapter"))
                    .unwrap_or_default();
                status.setStringValue(&NSString::from_str(&format!(
                    "Recording {v}chapter {n:02} — press New Chapter or Stop{waiting}"
                )));
            }
        }
    }

    /// Paint the recording clock. Called from the event loop's tick, so the
    /// caller decides how often — see `app::clock::RecordClock::take_paint`.
    /// Repaints the clock line and the input meter.
    ///
    /// `peak_dbfs` rather than an RMS level: the bar is there to show mic
    /// amplitude while the gain is being set, and RMS of a 20ms buffer barely
    /// moves against a voice's own 10–20dB crest.
    pub fn set_timer(
        &self,
        headline: &str,
        detail: &str,
        peak_dbfs: f32,
        clipped: bool,
        recording: bool,
    ) {
        if MainThreadMarker::new().is_none() {
            return;
        }
        if let Some(timer) = self.ivars().timer.borrow().clone() {
            timer.setStringValue(&NSString::from_str(headline));
            // Dimmed when nothing is recording: the numbers still say what the
            // session came to, without reading as a running clock.
            let colour = if recording {
                objc2_app_kit::NSColor::labelColor()
            } else {
                objc2_app_kit::NSColor::secondaryLabelColor()
            };
            timer.setTextColor(Some(&colour));
        }
        if let Some(detail_label) = self.ivars().timer_detail.borrow().clone() {
            detail_label.setStringValue(&NSString::from_str(detail));
        }
        if let Some(meter) = self.ivars().meter.borrow().clone() {
            meter.setDoubleValue(
                (peak_dbfs as f64).clamp(crate::capture::level::METER_FLOOR_DBFS as f64, 0.0),
            );
            // Once anything has clipped the whole bar goes critical and stays
            // there. The peak itself falls back within a frame, so colouring the
            // level alone would flash red for one repaint and then look fine.
            meter.setCriticalValue(if clipped {
                crate::capture::level::METER_FLOOR_DBFS as f64
            } else {
                -2.0
            });
        }
    }

    pub fn set_models(&self, menu: &[crate::notes::ModelMenuRow], selected: usize) {
        if MainThreadMarker::new().is_none() {
            return;
        }
        if let Some(popup) = self.ivars().model_popup.borrow().clone() {
            popup.removeAllItems();
            fill_model_menu(&popup, menu);
            if !menu.is_empty() {
                popup.selectItemAtIndex(selected.min(menu.len() - 1) as isize);
            }
        }
    }

    pub fn prompt_text(&self) -> String {
        self.ivars()
            .prompt_field
            .borrow()
            .as_ref()
            .map(|f| f.stringValue().to_string())
            .unwrap_or_default()
    }

    // Record tab render progress
    pub fn set_render_status(&self, text: &str) {
        if let Some(field) = self.ivars().render_status.borrow().clone() {
            field.setStringValue(&NSString::from_str(text));
        }
    }

    pub fn set_thumbnail_status(&self, text: &str) {
        if let Some(field) = self.ivars().thumbnail_status.borrow().clone() {
            field.setStringValue(&NSString::from_str(text));
        }
    }

    pub fn set_render_summary(&self, text: &str) {
        if let Some(view) = self.ivars().render_summary.borrow().clone() {
            view.setString(&NSString::from_str(text));
        }
    }

    // Post tab updates
    pub fn set_posts_status(&self, text: &str) {
        if let Some(field) = self.ivars().posts_status.borrow().clone() {
            field.setStringValue(&NSString::from_str(text));
        }
    }

    pub fn posts_prompt_text(&self) -> String {
        self.ivars()
            .posts_prompt_view
            .borrow()
            .as_ref()
            .map(|v| v.string().to_string())
            .unwrap_or_default()
    }

    pub fn set_posts_models(&self, menu: &[crate::notes::ModelMenuRow], selected: usize) {
        if let Some(popup) = self.ivars().posts_model_popup.borrow().clone() {
            popup.removeAllItems();
            fill_model_menu(&popup, menu);
            if !menu.is_empty() {
                popup.selectItemAtIndex(selected.min(menu.len() - 1) as isize);
            }
        }
    }

    // Distribute tab
    pub fn set_distribute_status(&self, text: &str) {
        if let Some(field) = self.ivars().distribute_status.borrow().clone() {
            field.setStringValue(&NSString::from_str(text));
        }
    }

    /// Shows the upload bar at `pct`. The bar is hidden until the first repaint,
    /// so an upload too small to report progress never leaves an empty bar sitting
    /// at zero.
    pub fn set_distribute_progress(&self, pct: f64) {
        if let Some(bar) = self.ivars().distribute_bar.borrow().clone() {
            bar.setHidden(false);
            bar.setDoubleValue(pct.clamp(0.0, 100.0));
        }
    }

    pub fn hide_distribute_progress(&self) {
        if let Some(bar) = self.ivars().distribute_bar.borrow().clone() {
            bar.setDoubleValue(0.0);
            bar.setHidden(true);
        }
    }

    pub fn set_distribute_info(&self, text: &str) {
        if let Some(view) = self.ivars().distribute_info.borrow().clone() {
            view.setString(&NSString::from_str(text));
        }
    }

    // YouTube tab
    pub fn set_publish_status(&self, text: &str) {
        if let Some(field) = self.ivars().publish_status.borrow().clone() {
            field.setStringValue(&NSString::from_str(text));
        }
    }



    // Schedule tab updates
    /// Puts the due count on the tab itself, so work waiting in a project you are
    /// not looking at is visible without opening anything.
    pub fn set_reflect_status(&self, text: &str) {
        if let Some(field) = self.ivars().reflect_status.borrow().clone() {
            field.setStringValue(&NSString::from_str(text));
        }
    }

    /// The Substack tab's one native line, for the same reason Reflect has one:
    /// progress arriving mid-run must not cost a whole-pane re-render, which
    /// would throw away the reader's place on a page they are typing from.
    pub fn set_substack_status(&self, text: &str) {
        if let Some(field) = self.ivars().substack_status.borrow().clone() {
            field.setStringValue(&NSString::from_str(text));
        }
    }

    /// The Blog tab's one native line. The publish job narrates several steps —
    /// lookups, the thumbnail, the create — and none of them should cost a
    /// repaint of the draft someone is reading.
    pub fn set_blog_status(&self, text: &str) {
        if let Some(field) = self.ivars().blog_status.borrow().clone() {
            field.setStringValue(&NSString::from_str(text));
        }
    }

    /// The Edit tab's one native line. Written to instead of repainting the pane,
    /// because repainting it reloads the editor.
    pub fn set_edit_status(&self, text: &str) {
        if let Some(field) = self.ivars().edit_status.borrow().clone() {
            field.setStringValue(&NSString::from_str(text));
        }
    }

    pub fn set_analytics_badge(&self, label: &str) {
        if MainThreadMarker::new().is_none() {
            return;
        }
        if let Some(item) = self.ivars().analytics_tab.borrow().clone() {
            item.setLabel(&NSString::from_str(label));
        }
    }

    pub fn set_analytics_status(&self, text: &str) {
        if let Some(field) = self.ivars().analytics_status.borrow().clone() {
            field.setStringValue(&NSString::from_str(text));
        }
    }

    pub fn set_analytics_info(&self, text: &str) {
        if let Some(view) = self.ivars().analytics_info.borrow().clone() {
            view.setString(&NSString::from_str(text));
        }
    }

    pub fn set_schedule_status(&self, text: &str) {
        if let Some(field) = self.ivars().schedule_status.borrow().clone() {
            field.setStringValue(&NSString::from_str(text));
        }
    }

    pub fn set_schedule_info(&self, text: &str) {
        if let Some(view) = self.ivars().schedule_info.borrow().clone() {
            view.setString(&NSString::from_str(text));
        }
    }
}

fn fill_model_menu(popup: &NSPopUpButton, menu: &[crate::notes::ModelMenuRow]) {
    for row in menu {
        match row {
            crate::notes::ModelMenuRow::Header(title) => {
                popup.addItemWithTitle(&NSString::from_str(title));
                if let Some(item) = popup.lastItem() {
                    item.setEnabled(false);
                    item.setIndentationLevel(0);
                }
            }
            crate::notes::ModelMenuRow::Model { label, .. } => {
                popup.addItemWithTitle(&NSString::from_str(label));
                if let Some(item) = popup.lastItem() {
                    item.setEnabled(true);
                    item.setIndentationLevel(1);
                }
            }
        }
    }
}

fn make_model_popup(
    mtm: MainThreadMarker,
    frame: NSRect,
    menu: &[crate::notes::ModelMenuRow],
    selected: usize,
    target: &Retained<ControlTarget>,
    action: Sel,
) -> Retained<NSPopUpButton> {
    let popup = NSPopUpButton::initWithFrame_pullsDown(NSPopUpButton::alloc(mtm), frame, false);
    fill_model_menu(&popup, menu);
    if !menu.is_empty() {
        popup.selectItemAtIndex(selected.min(menu.len() - 1) as isize);
    }
    unsafe {
        popup.setTarget(Some(target));
        popup.setAction(Some(action));
    }
    popup
}

fn make_popup(
    mtm: MainThreadMarker,
    frame: NSRect,
    titles: impl IntoIterator<Item = String>,
    selected: usize,
    target: &Retained<ControlTarget>,
    action: Sel,
) -> Retained<NSPopUpButton> {
    let popup = NSPopUpButton::initWithFrame_pullsDown(NSPopUpButton::alloc(mtm), frame, false);
    for title in titles {
        popup.addItemWithTitle(&NSString::from_str(&title));
    }
    unsafe {
        popup.selectItemAtIndex(selected as isize);
        popup.setTarget(Some(target));
        popup.setAction(Some(action));
    }
    popup
}

fn device_titles(devices: &[CaptureDevice]) -> impl IntoIterator<Item = String> + '_ {
    devices.iter().map(|d| d.name.clone())
}

/// Relays the views when the window or split divider moves.
pub struct Layout {
    left: Retained<NSView>,
    right: Retained<NSView>,
    status: Retained<NSTextField>,
    timer: (Retained<NSTextField>, Retained<NSTextField>),
    meter: Retained<NSLevelIndicator>,
    left_sections: Vec<(Retained<NSTextField>, Retained<NSPopUpButton>)>,
    project_name: Retained<NSTextField>,
    left_groups: Vec<(Retained<NSBox>, Vec<Retained<NSButton>>)>,
    notes_sections: Vec<(Retained<NSTextField>, Retained<NSPopUpButton>)>,
    prompt: (Retained<NSTextField>, Retained<NSTextField>),
    notes_group: (Retained<NSBox>, Vec<Retained<NSButton>>),
    render_status: Retained<NSTextField>,
    last: Cell<(u32, u32, u32, u32)>,
}

impl Layout {
    pub fn sync(&self, preview: &PreviewHost, notes: &crate::notes::NotesPane) {
        let left = self.left.bounds();
        let right = self.right.bounds();
        let key = (
            left.size.width.round() as u32,
            left.size.height.round() as u32,
            right.size.width.round() as u32,
            right.size.height.round() as u32,
        );
        if self.last.get() == key {
            return;
        }
        self.last.set(key);
        layout_left(
            left.size.width,
            left.size.height,
            &self.status,
            &self.timer,
            &self.meter,
            &self.left_sections,
            &self.project_name,
            &self.left_groups,
            &self.render_status,
            preview,
        );
        layout_right(
            right.size.width,
            right.size.height,
            &self.notes_sections,
            &self.prompt,
            &self.notes_group,
            notes,
        );
    }
}

fn fill_parent(view: &NSView) {
    view.setAutoresizingMask(
        NSAutoresizingMaskOptions::ViewWidthSizable
            | NSAutoresizingMaskOptions::ViewHeightSizable,
    );
}

fn pin_top(view: &NSView) {
    view.setAutoresizingMask(
        NSAutoresizingMaskOptions::ViewWidthSizable
            | NSAutoresizingMaskOptions::ViewMinYMargin,
    );
}

fn fill_below(view: &NSView) {
    view.setAutoresizingMask(
        NSAutoresizingMaskOptions::ViewWidthSizable
            | NSAutoresizingMaskOptions::ViewHeightSizable,
    );
}

/// Anchored to the bottom at a fixed height; the space above it takes the growth.
fn pin_bottom(view: &NSView) {
    view.setAutoresizingMask(
        NSAutoresizingMaskOptions::ViewWidthSizable
            | NSAutoresizingMaskOptions::ViewMaxYMargin,
    );
}

fn layout_left(
    width: f64,
    height: f64,
    status: &NSTextField,
    timer: &(Retained<NSTextField>, Retained<NSTextField>),
    meter: &NSLevelIndicator,
    sections: &[(Retained<NSTextField>, Retained<NSPopUpButton>)],
    project_name: &NSTextField,
    groups: &[(Retained<NSBox>, Vec<Retained<NSButton>>)],
    render_status: &NSTextField,
    preview: &PreviewHost,
) {
    let content_w = (width - PAD * 2.0).max(80.0);
    let row = |y: f64, h: f64| NSRect::new(NSPoint::new(PAD, y), NSSize::new(content_w, h));
    let mut y = height - PAD;
    y -= LABEL_H;
    status.setFrame(row(y, LABEL_H));
    y -= GAP + TIMER_H;
    timer.0.setFrame(row(y, TIMER_H));
    y -= LABEL_H;
    // The meter sits at the right end of the detail line, so the numbers and the
    // needle that explains them read as one row.
    let detail_w = (content_w - METER_W - GAP).max(60.0);
    timer.1.setFrame(NSRect::new(
        NSPoint::new(PAD, y),
        NSSize::new(detail_w, LABEL_H),
    ));
    meter.setFrame(NSRect::new(
        NSPoint::new(PAD + detail_w + GAP, y),
        NSSize::new(METER_W, LABEL_H),
    ));
    y -= SECTION_GAP;
    for (label, popup) in sections {
        y -= LABEL_H;
        label.setFrame(row(y, LABEL_H));
        y -= GAP + CONTROL_H;
        popup.setFrame(row(y, CONTROL_H));
        y -= SECTION_GAP;
    }
    // The name field sits directly under the picker it renames.
    y -= CONTROL_H;
    project_name.setFrame(row(y, CONTROL_H));
    y -= SECTION_GAP;
    let n = groups.len().max(1);
    let group_w = ((content_w - BUTTON_GAP * (n - 1) as f64) / n as f64).max(80.0);
    let group_h = groups.iter().map(|(_, buttons)| buttons.len() as f64 * BUTTON_H + buttons.len().saturating_sub(1) as f64 * BUTTON_GAP + 32.0).fold(GROUP_H, f64::max);
    let stack_bottom = (y - SECTION_GAP - group_h).max(PAD + 110.0);
    for (i, (frame, buttons)) in groups.iter().enumerate() {
        let x = PAD + i as f64 * (group_w + BUTTON_GAP);
        frame.setFrame(NSRect::new(
            NSPoint::new(x, stack_bottom),
            NSSize::new(group_w, group_h),
        ));
        layout_group_buttons(frame, buttons);
    }
    let progress_y = stack_bottom - SECTION_GAP - LABEL_H;
    render_status.setFrame(row(progress_y, LABEL_H));
    preview.set_frame(NSRect::new(
        NSPoint::new(PAD, PAD),
        NSSize::new(content_w, (progress_y - GAP - PAD).max(60.0)),
    ));
}

fn layout_right(
    width: f64,
    height: f64,
    sections: &[(Retained<NSTextField>, Retained<NSPopUpButton>)],
    prompt: &(Retained<NSTextField>, Retained<NSTextField>),
    notes_group: &(Retained<NSBox>, Vec<Retained<NSButton>>),
    notes: &crate::notes::NotesPane,
) {
    let content_w = (width - PAD * 2.0).max(80.0);
    let row = |y: f64, h: f64| NSRect::new(NSPoint::new(PAD, y), NSSize::new(content_w, h));
    let mut y = height - PAD;
    let half = ((content_w - GAP) / 2.0).max(60.0);
    y -= LABEL_H;
    // Two on one row: the first section takes the left half, the second the
    // right. The caller orders them provider-then-model, so the choice reads
    // left to right in the order it is actually made.
    if let [provider, model] = sections {
        provider.0.setFrame(NSRect::new(
            NSPoint::new(PAD, y),
            NSSize::new(half, LABEL_H),
        ));
        model.0.setFrame(NSRect::new(
            NSPoint::new(PAD + half + GAP, y),
            NSSize::new(half, LABEL_H),
        ));
        y -= GAP + CONTROL_H;
        provider.1.setFrame(NSRect::new(
            NSPoint::new(PAD, y),
            NSSize::new(half, CONTROL_H),
        ));
        model.1.setFrame(NSRect::new(
            NSPoint::new(PAD + half + GAP, y),
            NSSize::new(half, CONTROL_H),
        ));
    } else {
        for (label, popup) in sections {
            label.setFrame(row(y, LABEL_H));
            y -= GAP + CONTROL_H;
            popup.setFrame(row(y, CONTROL_H));
            y -= SECTION_GAP + LABEL_H;
        }
    }
    y -= SECTION_GAP;
    y -= LABEL_H;
    prompt.0.setFrame(row(y, LABEL_H));
    y -= GAP + PROMPT_H;
    prompt.1.setFrame(row(y, PROMPT_H));
    y -= SECTION_GAP + GROUP_H;
    notes_group.0.setFrame(NSRect::new(
        NSPoint::new(PAD, y),
        NSSize::new(content_w, GROUP_H),
    ));
    layout_group_buttons(&notes_group.0, &notes_group.1);
    let deck_h = (y - SECTION_GAP - PAD).max(80.0);
    notes.set_frame(NSRect::new(
        NSPoint::new(PAD, PAD),
        NSSize::new(content_w, deck_h),
    ));
}

fn layout_group_buttons(frame: &NSBox, buttons: &[Retained<NSButton>]) {
    let Some(content) = frame.contentView() else {
        return;
    };
    let bounds = content.bounds();
    let n = buttons.len().max(1);
    let total = n as f64 * BUTTON_H + (n - 1) as f64 * BUTTON_GAP;
    let y0 = ((bounds.size.height - total) / 2.0).max(0.0);
    for (i, button) in buttons.iter().enumerate() {
        let from_bottom = (n - 1 - i) as f64;
        let y = y0 + from_bottom * (BUTTON_H + BUTTON_GAP);
        button.setFrame(NSRect::new(
            NSPoint::new(0.0, y),
            NSSize::new(bounds.size.width.max(8.0), BUTTON_H),
        ));
    }
}

fn make_button_group(
    mtm: MainThreadMarker,
    title: &str,
    buttons: &[Retained<NSButton>],
) -> Retained<NSBox> {
    let frame = NSBox::initWithFrame(
        NSBox::alloc(mtm),
        NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(120.0, GROUP_H)),
    );
    frame.setTitle(&NSString::from_str(title));
    frame.setTitlePosition(NSTitlePosition::AtTop);
    frame.setContentViewMargins(NSSize::new(6.0, 6.0));
    if let Some(content) = frame.contentView() {
        for button in buttons {
            content.addSubview(button);
        }
    }
    frame
}

/// Everything [`attach_controls`] hands back, named.
///
/// A struct rather than a tuple because four of these fields are `WebPane` and two more
/// are forms: in positional form, wiring the Edit pane where the Review pane belongs
/// compiles cleanly and shows up as the wrong tab on screen. Names make that a
/// compile error instead of a bug report.
pub struct Attached {
    pub rx: Receiver<UiEvent>,
    pub tx: Sender<UiEvent>,
    /// `NSControl.target` is weak — dropping this silently kills every button.
    pub target: Retained<ControlTarget>,
    pub preview: PreviewHost,
    pub notes: crate::notes::NotesPane,
    pub posts_form: crate::posts::PostsForm,
    pub schedule_form: crate::schedule::ScheduleForm,
    pub reflect_pane: WebPane,
    pub thumbnail_pane: WebPane,
    pub review_pane: WebPane,
    pub edit_pane: Option<WebPane>,
    pub substack_pane: Option<WebPane>,
    pub blog_pane: WebPane,
    pub publish_pane: WebPane,
    pub video_brief_pane: WebPane,
    pub settings_pane: WebPane,
    pub layout: Layout,
}

/// The Settings pane, drawn from whatever the credentials look like right now.
///
/// One function so the first draw and every redraw after a save agree — a pane
/// that rebuilt its own context at each call site is how a saved key goes on
/// showing as unset.
///
/// `note` is the line under the Save button: what just happened, or nothing on
/// a cold draw.
pub fn settings_page(note: Option<&str>) -> String {
    let path = crate::settings::env_path()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|_| "~/.stream-recorder/.env".into());
    render::page(
        "settings.html",
        minijinja::context! {
            sections => crate::settings::sections(),
            missing => crate::settings::missing_required(),
            env_path => path,
            team_count => crate::settings::sops::provided().len(),
            models => crate::settings::models(),
            saved => note.unwrap_or(""),
        },
    )
}

/// Build the record window's controls and tabs.
pub fn attach_controls(
    window: &Window,
    cameras: &[CaptureDevice],
    camera_idx: usize,
    mics: &[CaptureDevice],
    mic_idx: usize,
    displays: &[CaptureDevice],
    screen_idx: Option<usize>,
    pair_idx: usize,
    face_tracking: bool,
    mouse_tracking: bool,
    youtube_privacy: crate::publish::youtube::Privacy,
    model_menu: &[crate::notes::ModelMenuRow],
    model_idx: usize,
    provider_labels: &[String],
    provider_idx: usize,
    notes_prompt: &str,
    posts_prompt: &str,
    versions: &[crate::session::VersionInfo],
    version: Option<u32>,
    projects: &[crate::sessions::SessionEntry],
    project_folder: &str,
    project_name: &str,
) -> Result<Attached> {
    let mtm = MainThreadMarker::new()
        .ok_or_else(|| anyhow!("controls must be created on the main thread"))?;

    let handle = window
        .window_handle()
        .map_err(|e| anyhow!("no window handle: {e}"))?;
    let RawWindowHandle::AppKit(appkit) = handle.as_raw() else {
        return Err(anyhow!("not an AppKit window"));
    };
    let view: &NSView = unsafe { appkit.ns_view.cast::<NSView>().as_ref() };

    let (tx, rx) = channel();
    let target = ControlTarget::new(tx.clone());

    let bounds = view.bounds();

    // Root tab view
    let tab_view = NSTabView::initWithFrame(NSTabView::alloc(mtm), bounds);
    fill_parent(&tab_view);
    tab_view.setTabViewType(NSTabViewType::TopTabsBezelBorder);
    view.addSubview(&tab_view);

    // ==========================================
    // TAB 0: DRAFT
    // ==========================================
    let split = NSSplitView::initWithFrame(NSSplitView::alloc(mtm), bounds);
    split.setVertical(true);
    split.setDividerStyle(NSSplitViewDividerStyle::Thin);
    fill_parent(&split);
    split.setAutosaveName(Some(&NSString::from_str("stream-recorder-split")));

    let left_w = (bounds.size.width - NOTES_DEFAULT_W).max(480.0);
    let left = NSView::initWithFrame(
        NSView::alloc(mtm),
        NSRect::new(
            NSPoint::new(0.0, 0.0),
            NSSize::new(left_w, bounds.size.height),
        ),
    );
    let right = NSView::initWithFrame(
        NSView::alloc(mtm),
        NSRect::new(
            NSPoint::new(left_w, 0.0),
            NSSize::new(
                (bounds.size.width - left_w).max(NOTES_DEFAULT_W),
                bounds.size.height,
            ),
        ),
    );
    split.addSubview(&left);
    split.addSubview(&right);
    split.setPosition_ofDividerAtIndex(left_w, 0);

    // Keep the working brief visible beside the recording controls. Existing
    // speaking-note controls and deck retain their own full-height panel.
    let notebook_tabs = NSTabView::initWithFrame(NSTabView::alloc(mtm), right.bounds());
    fill_parent(&notebook_tabs);
    notebook_tabs.setTabViewType(NSTabViewType::TopTabsBezelBorder);
    right.addSubview(&notebook_tabs);
    let brief_host = NSView::initWithFrame(NSView::alloc(mtm), right.bounds());
    let speaking_host = NSView::initWithFrame(NSView::alloc(mtm), right.bounds());
    for (id, label, host) in [("video-brief", "Video details", &brief_host), ("speaking-notes", "Speaking notes", &speaking_host)] {
        let item = unsafe { NSTabViewItem::initWithIdentifier(NSTabViewItem::alloc(), Some(&NSString::from_str(id))) };
        item.setLabel(&NSString::from_str(label));
        item.setView(Some(host));
        notebook_tabs.addTabViewItem(&item);
    }
    let video_brief_pane = WebPane::attach(&brief_host, mtm, tx.clone());
    video_brief_pane.set_frame(brief_host.bounds());
    video_brief_pane.fill_below();
    let right = speaking_host;

    let status = NSTextField::labelWithString(&NSString::from_str(""), mtm);
    left.addSubview(&status);
    *target.ivars().status.borrow_mut() = Some(status.clone());

    // The recording clock. Monospaced digits, or every tick of the seconds
    // re-measures the whole line and the numbers jitter sideways as they count.
    let timer = NSTextField::labelWithString(&NSString::from_str(""), mtm);
    timer.setFont(Some(&NSFont::monospacedDigitSystemFontOfSize_weight(
        16.0,
        NSFontWeight::from(0.3),
    )));
    left.addSubview(&timer);
    *target.ivars().timer.borrow_mut() = Some(timer.clone());

    let timer_detail = NSTextField::labelWithString(&NSString::from_str(""), mtm);
    timer_detail.setFont(Some(&NSFont::monospacedDigitSystemFontOfSize_weight(
        11.0,
        NSFontWeight::from(0.0),
    )));
    left.addSubview(&timer_detail);
    *target.ivars().timer_detail.borrow_mut() = Some(timer_detail.clone());

    let meter = NSLevelIndicator::initWithFrame(
        NSLevelIndicator::alloc(mtm),
        NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(METER_W, LABEL_H)),
    );
    meter.setLevelIndicatorStyle(NSLevelIndicatorStyle::ContinuousCapacity);
    // dBFS, drawn from the meter floor up to full scale, so the yellow and red
    // sections are the levels that are actually about to clip.
    meter.setMinValue(crate::capture::level::METER_FLOOR_DBFS as f64);
    meter.setMaxValue(0.0);
    meter.setWarningValue(-6.0);
    meter.setCriticalValue(-2.0);
    meter.setDoubleValue(crate::capture::level::METER_FLOOR_DBFS as f64);
    left.addSubview(&meter);
    *target.ivars().meter.borrow_mut() = Some(meter.clone());

    let add_section = |parent: &NSView,
                       title: &str,
                       titles: Vec<String>,
                       selected: usize,
                       action: Sel|
     -> (Retained<NSTextField>, Retained<NSPopUpButton>) {
        let label = NSTextField::labelWithString(&NSString::from_str(title), mtm);
        parent.addSubview(&label);
        let popup = make_popup(
            mtm,
            NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(100.0, CONTROL_H)),
            titles,
            selected,
            &target,
            action,
        );
        parent.addSubview(&popup);
        (label, popup)
    };

    let camera = add_section(
        &left,
        "Camera",
        device_titles(cameras).into_iter().collect(),
        camera_idx,
        sel!(onCameraChanged:),
    );
    *target.ivars().camera_popup.borrow_mut() = Some(camera.1.clone());
    let mic = add_section(
        &left,
        "Microphone",
        device_titles(mics).into_iter().collect(),
        mic_idx,
        sel!(onMicChanged:),
    );
    *target.ivars().mic_popup.borrow_mut() = Some(mic.1.clone());
    let screen_titles = std::iter::once(NO_SCREEN.to_string())
        .chain(displays.iter().map(|d| d.name.clone()))
        .collect::<Vec<_>>();
    let screen = add_section(
        &left,
        "Screen",
        screen_titles,
        screen_idx.map_or(0, |i| i + 1),
        sel!(onScreenChanged:),
    );
    *target.ivars().screen_popup.borrow_mut() = Some(screen.1.clone());
    let version_titles: Vec<String> = if versions.is_empty() {
        vec!["v1".into()]
    } else {
        versions.iter().map(|v| v.label()).collect()
    };
    let version_idx = version
        .and_then(|n| versions.iter().position(|v| v.n == n))
        .unwrap_or(0);
    let version = add_section(
        &left,
        "Version",
        version_titles,
        version_idx,
        sel!(onVersionChanged:),
    );
    *target.ivars().version_popup.borrow_mut() = Some(version.1.clone());

    // A recording lives in a project folder. Without this picker the app minted a
    // fresh timestamped folder on every launch and silently orphaned the last one.
    let project_titles: Vec<String> = if projects.is_empty() {
        vec!["(new project)".into()]
    } else {
        projects.iter().map(|p| p.label()).collect()
    };
    let project_idx = projects
        .iter()
        .position(|p| p.folder == project_folder)
        .unwrap_or(0);
    let project = add_section(
        &left,
        "Project",
        project_titles,
        project_idx,
        sel!(onProjectChanged:),
    );
    *target.ivars().project_popup.borrow_mut() = Some(project.1.clone());

    let project_name_field = NSTextField::initWithFrame(
        NSTextField::alloc(mtm),
        NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(200.0, CONTROL_H)),
    );
    project_name_field.setBezeled(true);
    project_name_field.setEditable(true);
    project_name_field
        .setPlaceholderString(Some(&NSString::from_str("Project name")));
    project_name_field.setStringValue(&NSString::from_str(project_name));
    unsafe {
        project_name_field.setTarget(Some(&target));
        project_name_field.setAction(Some(sel!(onProjectNameChanged:)));
    }
    left.addSubview(&project_name_field);
    *target.ivars().project_name.borrow_mut() = Some(project_name_field.clone());

    let left_sections = vec![camera, mic, screen, version, project];

    let model_label = NSTextField::labelWithString(&NSString::from_str("Model"), mtm);
    right.addSubview(&model_label);
    let model_popup = make_model_popup(
        mtm,
        NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(100.0, CONTROL_H)),
        model_menu,
        model_idx,
        &target,
        sel!(onModelChanged:),
    );
    right.addSubview(&model_popup);
    *target.ivars().model_popup.borrow_mut() = Some(model_popup.clone());
    let provider = add_section(
        &right,
        "Provider",
        provider_labels.to_vec(),
        provider_idx,
        sel!(onProviderChanged:),
    );
    *target.ivars().provider_popup.borrow_mut() = Some(provider.1.clone());
    let prompt_label = NSTextField::labelWithString(&NSString::from_str("Notes prompt"), mtm);
    right.addSubview(&prompt_label);
    let prompt_field = NSTextField::initWithFrame(
        NSTextField::alloc(mtm),
        NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(100.0, PROMPT_H)),
    );
    prompt_field.setBezeled(true);
    prompt_field.setEditable(true);
    prompt_field.setPlaceholderString(Some(&NSString::from_str(
        "Steer the notes, e.g. keep the intro tight",
    )));
    prompt_field.setStringValue(&NSString::from_str(notes_prompt));
    unsafe {
        prompt_field.setTarget(Some(&target));
        prompt_field.setAction(Some(sel!(onPromptChanged:)));
    }
    right.addSubview(&prompt_field);
    *target.ivars().prompt_field.borrow_mut() = Some(prompt_field.clone());
    // Provider above Model, because that is the order the choice actually has:
    // the provider decides which models are in the list below it.
    let notes_sections = vec![provider, (model_label, model_popup)];

    let pair_label = NSTextField::labelWithString(&NSString::from_str("Layout"), mtm);
    let pair_popup = make_popup(
        mtm,
        NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(160.0, CONTROL_H)),
        Pair::ALL.iter().map(|p| p.as_str().to_string()),
        pair_idx,
        &target,
        sel!(onPairChanged:),
    );
    *target.ivars().pair_popup.borrow_mut() = Some(pair_popup.clone());

    let face_checkbox = unsafe {
        NSButton::buttonWithTitle_target_action(
            &NSString::from_str("Track Face"),
            Some(&target),
            Some(sel!(onFaceTrackChanged:)),
            mtm,
        )
    };
    // A push button by default; `Switch` is what makes it a checkbox.
    face_checkbox.setButtonType(NSButtonType::Switch);
    face_checkbox.setState(if face_tracking {
        NSControlStateValueOn
    } else {
        NSControlStateValueOff
    });
    *target.ivars().face_checkbox.borrow_mut() = Some(face_checkbox.clone());

    let mouse_checkbox = unsafe {
        NSButton::buttonWithTitle_target_action(
            &NSString::from_str("Track Mouse"),
            Some(&target),
            Some(sel!(onMouseTrackChanged:)),
            mtm,
        )
    };
    mouse_checkbox.setButtonType(NSButtonType::Switch);
    mouse_checkbox.setState(if mouse_tracking {
        NSControlStateValueOn
    } else {
        NSControlStateValueOff
    });
    *target.ivars().mouse_checkbox.borrow_mut() = Some(mouse_checkbox.clone());

    // Ordered so each box below is a contiguous slice. Inserting a button
    // anywhere but the end of its own run means every later slice moves —
    // keep the RECORD/NOTES/SESSION ranges beneath in step with this list.
    let buttons: [(&str, Sel); 9] = [
        ("", sel!(onNewChapter:)),                    // 0 ┐
        ("Retake  ⌃⌥T", sel!(onRetake:)),                 // 1 │ Record
        ("Stop", sel!(onStop:)),
        ("Render video", sel!(onRunRender:)),
        ("Notes", sel!(onNotes:)),                    // 4 ┐ Notes (right pane)
        ("Copy Transcript", sel!(onCopyTranscript:)), // 5 ┘
        ("New Project", sel!(onNewProject:)),         // 6 ┐
        ("New Version", sel!(onNewVersion:)),         // 7 │ Session
        ("", sel!(onToggleRegions:)),                 // 8 ┘ title set by set_regions
    ];
    const RECORD: Range<usize> = 0..4;
    const NOTES: Range<usize> = 4..6;
    const SESSION: Range<usize> = 6..9;
    const REGIONS: usize = 8;
    let mut built = Vec::with_capacity(buttons.len());
    for (title, action) in buttons {
        let button = unsafe {
            NSButton::buttonWithTitle_target_action(
                &NSString::from_str(title),
                Some(&target),
                Some(action),
                mtm,
            )
        };
        built.push(button);
    }
    *target.ivars().chapter_button.borrow_mut() = Some(built[0].clone());
    *target.ivars().retake_button.borrow_mut() = Some(built[1].clone());
    *target.ivars().regions_button.borrow_mut() = Some(built[REGIONS].clone());

    *target.ivars().render_button.borrow_mut() = Some(built[3].clone());
    let render_status = NSTextField::labelWithString(&NSString::from_str("Record a video, then render it here."), mtm);
    left.addSubview(&render_status);
    *target.ivars().render_status.borrow_mut() = Some(render_status.clone());

    let left_groups = [("Record", &built[RECORD]), ("Session", &built[SESSION])]
        .into_iter()
        .map(|(title, group)| {
            let frame = make_button_group(mtm, title, group);
            left.addSubview(&frame);
            (frame, group.to_vec())
        })
        .collect();
    let notes_buttons = &built[NOTES];
    let notes_box = make_button_group(mtm, "Notes", notes_buttons);
    right.addSubview(&notes_box);

    let preview = PreviewHost::attach(
        &left,
        mtm,
        pair_label,
        pair_popup,
        face_checkbox,
        mouse_checkbox,
    );
    let notes = crate::notes::NotesPane::attach(&right, mtm);

    let draft_item = unsafe {
        NSTabViewItem::initWithIdentifier(
            NSTabViewItem::alloc(),
            Some(&NSString::from_str("draft")),
        )
    };
    draft_item.setLabel(&NSString::from_str("Record"));
    draft_item.setView(Some(&split));

    let thumb_host = NSView::initWithFrame(NSView::alloc(mtm), bounds);
    fill_parent(&thumb_host);
    let thumbnail_status = NSTextField::labelWithString(
        &NSString::from_str("Capture a frame, then draw a card or generate images."), mtm,
    );
    thumbnail_status.setFrame(NSRect::new(
        NSPoint::new(PAD * 2.0, bounds.size.height - 34.0),
        NSSize::new(bounds.size.width - PAD * 4.0, 20.0),
    ));
    pin_top(&thumbnail_status);
    thumb_host.addSubview(&thumbnail_status);
    *target.ivars().thumbnail_status.borrow_mut() = Some(thumbnail_status);
    let thumbnail_pane = WebPane::attach(&thumb_host, mtm, tx.clone());
    thumbnail_pane.set_frame(NSRect::new(
        NSPoint::new(PAD, PAD),
        NSSize::new(bounds.size.width - PAD * 2.0, bounds.size.height - 44.0),
    ));
    thumbnail_pane.fill_below();
    let thumbnail_item = unsafe {
        NSTabViewItem::initWithIdentifier(NSTabViewItem::alloc(), Some(&NSString::from_str("thumbnails")))
    };
    thumbnail_item.setLabel(&NSString::from_str("Thumbnails"));
    thumbnail_item.setView(Some(&thumb_host));

    // ==========================================
    // TAB 2: POST
    // ==========================================
    let post_view = NSView::initWithFrame(NSView::alloc(mtm), bounds);
    fill_parent(&post_view);

    let post_title = NSTextField::labelWithString(
        &NSString::from_str("Social posts"),
        mtm,
    );
    post_title.setFrame(NSRect::new(NSPoint::new(PAD * 2.0, bounds.size.height - 60.0), NSSize::new(600.0, 24.0)));
    pin_top(&post_title);
    post_view.addSubview(&post_title);

    // Provider first, then Model — same reasoning as the Notes tab. The prompt
    // field after them keeps its x, since 160 + 10 + 220 still clears 400.
    let p_model_label = NSTextField::labelWithString(&NSString::from_str("Model"), mtm);
    p_model_label.setFrame(NSRect::new(NSPoint::new(PAD * 2.0 + 170.0, bounds.size.height - 86.0), NSSize::new(60.0, LABEL_H)));
    pin_top(&p_model_label);
    post_view.addSubview(&p_model_label);

    let posts_model_popup = make_model_popup(
        mtm,
        NSRect::new(NSPoint::new(PAD * 2.0 + 170.0, bounds.size.height - 114.0), NSSize::new(220.0, CONTROL_H)),
        model_menu,
        model_idx,
        &target,
        sel!(onPostsModelChanged:),
    );
    pin_top(&posts_model_popup);
    post_view.addSubview(&posts_model_popup);
    *target.ivars().posts_model_popup.borrow_mut() = Some(posts_model_popup.clone());

    let p_provider_label = NSTextField::labelWithString(&NSString::from_str("Provider"), mtm);
    p_provider_label.setFrame(NSRect::new(NSPoint::new(PAD * 2.0, bounds.size.height - 86.0), NSSize::new(80.0, LABEL_H)));
    pin_top(&p_provider_label);
    post_view.addSubview(&p_provider_label);

    let posts_provider_popup = make_popup(
        mtm,
        NSRect::new(NSPoint::new(PAD * 2.0, bounds.size.height - 114.0), NSSize::new(160.0, CONTROL_H)),
        provider_labels.to_vec(),
        provider_idx,
        &target,
        sel!(onPostsProviderChanged:),
    );
    pin_top(&posts_provider_popup);
    post_view.addSubview(&posts_provider_popup);
    *target.ivars().posts_provider_popup.borrow_mut() = Some(posts_provider_popup.clone());

    // The example lives in the label, not in a placeholder: a text area has no
    // placeholder, and the hint has to survive the field being full anyway.
    let p_prompt_label = NSTextField::labelWithString(
        &NSString::from_str("Audience / Tone prompt — kept between runs, e.g. “Be concise.”"),
        mtm,
    );
    p_prompt_label.setFrame(NSRect::new(NSPoint::new(PAD * 2.0 + 400.0, bounds.size.height - 86.0), NSSize::new(420.0, LABEL_H)));
    pin_top(&p_prompt_label);
    post_view.addSubview(&p_prompt_label);

    // A text area rather than a one-line field: this is standing guidance —
    // audience, tone, the rules that repeat every run — and it is written in
    // sentences, not in a search box. It keeps whatever was last typed, so the
    // usual instruction is already in place at launch.
    let prompt_w = (bounds.size.width - PAD * 4.0 - 400.0).max(280.0);
    let prompt_scroll = NSScrollView::initWithFrame(
        NSScrollView::alloc(mtm),
        NSRect::new(
            NSPoint::new(PAD * 2.0 + 400.0, bounds.size.height - 156.0),
            NSSize::new(prompt_w, 68.0),
        ),
    );
    prompt_scroll.setHasVerticalScroller(true);
    prompt_scroll.setBorderType(NSBorderType::BezelBorder);
    pin_top(&prompt_scroll);
    let posts_prompt_view = NSTextView::initWithFrame(
        NSTextView::alloc(mtm),
        NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(prompt_w, 68.0)),
    );
    posts_prompt_view.setEditable(true);
    // Plain text: pasted guidance arrives as prose, not as whatever font and
    // colour it was copied from, and it is sent to a model as characters.
    posts_prompt_view.setRichText(false);
    posts_prompt_view.setFont(Some(&NSFont::systemFontOfSize(12.0)));
    // Grows downward under the scroller instead of being clipped at the frame
    // it was created with, and wraps rather than scrolling sideways.
    posts_prompt_view.setMinSize(NSSize::new(0.0, 0.0));
    posts_prompt_view.setMaxSize(NSSize::new(f64::MAX, f64::MAX));
    posts_prompt_view.setVerticallyResizable(true);
    posts_prompt_view.setHorizontallyResizable(false);
    posts_prompt_view.setAutoresizingMask(NSAutoresizingMaskOptions::ViewWidthSizable);
    posts_prompt_view.setString(&NSString::from_str(posts_prompt));
    unsafe {
        if let Some(container) = posts_prompt_view.textContainer() {
            container.setWidthTracksTextView(true);
        }
        // `ControlTarget` implements `textDidEndEditing:` but is not declared as
        // an `NSTextViewDelegate`: that protocol is main-thread-only and this
        // class is not. The delegate call is an ordinary message send, so it is
        // wired by hand rather than by making the whole class main-thread-bound.
        let _: () = msg_send![&*posts_prompt_view, setDelegate: &*target];
    }
    prompt_scroll.setDocumentView(Some(&posts_prompt_view));
    post_view.addSubview(&prompt_scroll);
    *target.ivars().posts_prompt_view.borrow_mut() = Some(posts_prompt_view.clone());

    let gen_posts_btn = unsafe {
        NSButton::buttonWithTitle_target_action(
            &NSString::from_str("Generate All Posts"),
            Some(&target),
            Some(sel!(onGeneratePosts:)),
            mtm,
        )
    };
    gen_posts_btn.setFrame(NSRect::new(NSPoint::new(PAD * 2.0, bounds.size.height - 156.0), NSSize::new(180.0, 32.0)));
    pin_top(&gen_posts_btn);
    post_view.addSubview(&gen_posts_btn);
    *target.ivars().posts_button.borrow_mut() = Some(gen_posts_btn.clone());

    let save_posts_btn = unsafe {
        NSButton::buttonWithTitle_target_action(
            &NSString::from_str("Save Edits"),
            Some(&target),
            Some(sel!(onSavePosts:)),
            mtm,
        )
    };
    save_posts_btn.setFrame(NSRect::new(NSPoint::new(PAD * 2.0 + 190.0, bounds.size.height - 156.0), NSSize::new(120.0, 32.0)));
    pin_top(&save_posts_btn);
    post_view.addSubview(&save_posts_btn);

    // Below the buttons, not beside them: the prompt text area now occupies the
    // right of that row.
    let posts_status = NSTextField::labelWithString(&NSString::from_str("Ready to generate social copy for 8 platforms."), mtm);
    posts_status.setFrame(NSRect::new(NSPoint::new(PAD * 2.0, bounds.size.height - 186.0), NSSize::new(700.0, 20.0)));
    pin_top(&posts_status);
    post_view.addSubview(&posts_status);
    *target.ivars().posts_status.borrow_mut() = Some(posts_status.clone());

    let post_scroll_frame = NSRect::new(
        NSPoint::new(PAD * 2.0, PAD * 2.0),
        NSSize::new(
            bounds.size.width - PAD * 4.0,
            (bounds.size.height - 216.0).max(80.0),
        ),
    );
    let posts_form = crate::posts::PostsForm::attach(&post_view, mtm);
    posts_form.set_frame(post_scroll_frame);
    posts_form.show_empty();

    let post_item = unsafe {
        NSTabViewItem::initWithIdentifier(
            NSTabViewItem::alloc(),
            Some(&NSString::from_str("post")),
        )
    };
    post_item.setLabel(&NSString::from_str("Generate posts"));
    post_item.setView(Some(&post_view));

    // ==========================================
    // TAB 3: REVIEW (watch the renders)
    // ==========================================
    // Deliberately in front of Distribute: this is the last place a bad render
    // can be caught before it is on a social network.
    let review_view = NSView::initWithFrame(NSView::alloc(mtm), bounds);
    fill_parent(&review_view);
    let review_pane = WebPane::attach(&review_view, mtm, tx.clone());
    review_pane.set_frame(NSRect::new(
        NSPoint::new(PAD, PAD),
        NSSize::new(bounds.size.width - PAD * 2.0, bounds.size.height - 44.0),
    ));
    review_pane.fill_below();

    let review_item = unsafe {
        NSTabViewItem::initWithIdentifier(
            NSTabViewItem::alloc(),
            Some(&NSString::from_str("review")),
        )
    };
    review_item.setLabel(&NSString::from_str("Review"));
    review_item.setView(Some(&review_view));

    // ==========================================
    // TAB 4: DISTRIBUTE (AWS S3)
    // ==========================================
    let distribute_view = NSView::initWithFrame(NSView::alloc(mtm), bounds);
    fill_parent(&distribute_view);

    let dist_title = NSTextField::labelWithString(
        &NSString::from_str("Upload media — prepare video URLs for Buffer"),
        mtm,
    );
    dist_title.setFrame(NSRect::new(
        NSPoint::new(PAD * 2.0, bounds.size.height - 48.0),
        NSSize::new(600.0, 20.0),
    ));
    pin_top(&dist_title);
    distribute_view.addSubview(&dist_title);

    let upload_btn = unsafe {
        NSButton::buttonWithTitle_target_action(
            &NSString::from_str("Upload to S3"),
            Some(&target),
            Some(sel!(onDistribute:)),
            mtm,
        )
    };
    upload_btn.setFrame(NSRect::new(
        NSPoint::new(PAD * 2.0, bounds.size.height - 84.0),
        NSSize::new(140.0, 28.0),
    ));
    pin_top(&upload_btn);
    distribute_view.addSubview(&upload_btn);
    *target.ivars().distribute_button.borrow_mut() = Some(upload_btn.clone());

    let dist_status = NSTextField::labelWithString(
        &NSString::from_str("Render first, then upload."),
        mtm,
    );
    dist_status.setFrame(NSRect::new(
        NSPoint::new(PAD * 2.0 + 150.0, bounds.size.height - 82.0),
        NSSize::new(620.0, 24.0),
    ));
    pin_top(&dist_status);
    distribute_view.addSubview(&dist_status);
    *target.ivars().distribute_status.borrow_mut() = Some(dist_status.clone());

    // The uploader reports real byte counts, so this is a determinate bar rather
    // than a spinner. It stays hidden until the first repaint arrives.
    let dist_bar = NSProgressIndicator::initWithFrame(
        NSProgressIndicator::alloc(mtm),
        NSRect::new(
            NSPoint::new(PAD * 2.0, bounds.size.height - 112.0),
            NSSize::new(bounds.size.width - PAD * 4.0, 16.0),
        ),
    );
    dist_bar.setStyle(NSProgressIndicatorStyle::Bar);
    dist_bar.setIndeterminate(false);
    dist_bar.setMinValue(0.0);
    dist_bar.setMaxValue(100.0);
    dist_bar.setDoubleValue(0.0);
    dist_bar.setHidden(true);
    pin_top(&dist_bar);
    distribute_view.addSubview(&dist_bar);
    *target.ivars().distribute_bar.borrow_mut() = Some(dist_bar.clone());

    let dist_scroll = NSScrollView::initWithFrame(
        NSScrollView::alloc(mtm),
        NSRect::new(
            NSPoint::new(PAD * 2.0, PAD * 2.0),
            NSSize::new(bounds.size.width - PAD * 4.0, bounds.size.height - 120.0),
        ),
    );
    fill_below(&dist_scroll);
    dist_scroll.setHasVerticalScroller(true);
    let dist_text_view = NSTextView::initWithFrame(
        NSTextView::alloc(mtm),
        NSRect::new(
            NSPoint::new(0.0, 0.0),
            NSSize::new(bounds.size.width - PAD * 4.0, bounds.size.height - 120.0),
        ),
    );
    dist_text_view.setEditable(false);
    dist_text_view.setString(&NSString::from_str(
        "Uploads longform + vertical chapters to public S3.\n\
         Links land in distribute/vN/links.json.",
    ));
    dist_scroll.setDocumentView(Some(&dist_text_view));
    distribute_view.addSubview(&dist_scroll);
    *target.ivars().distribute_info.borrow_mut() = Some(dist_text_view.clone());

    let distribute_item = unsafe {
        NSTabViewItem::initWithIdentifier(
            NSTabViewItem::alloc(),
            Some(&NSString::from_str("distribute")),
        )
    };
    distribute_item.setLabel(&NSString::from_str("Upload media"));
    distribute_item.setView(Some(&distribute_view));

    // ==========================================
    // TAB 5: YOUTUBE (the longform, uploaded directly)
    // ==========================================
    // Its own tab rather than a button on Schedule, because it is its own system:
    // Buffer queues short social posts into a channel's slots, this publishes one
    // video with a title, a description and a designed thumbnail the moment it is
    // ready. See [`crate::publish`].
    let publish_view = NSView::initWithFrame(NSView::alloc(mtm), bounds);
    fill_parent(&publish_view);

    let publish_title = NSTextField::labelWithString(
        &NSString::from_str("YouTube — the longform, uploaded directly"),
        mtm,
    );
    publish_title.setFrame(NSRect::new(
        NSPoint::new(PAD * 2.0, bounds.size.height - 48.0),
        NSSize::new(600.0, 20.0),
    ));
    pin_top(&publish_title);
    publish_view.addSubview(&publish_title);

    let publish_btn = unsafe {
        NSButton::buttonWithTitle_target_action(
            &NSString::from_str("Upload to YouTube"),
            Some(&target),
            Some(sel!(onYoutubeUpload:)),
            mtm,
        )
    };
    publish_btn.setFrame(NSRect::new(
        NSPoint::new(PAD * 2.0, bounds.size.height - 84.0),
        NSSize::new(160.0, 28.0),
    ));
    pin_top(&publish_btn);
    publish_view.addSubview(&publish_btn);
    *target.ivars().publish_button.borrow_mut() = Some(publish_btn.clone());

    // Beside Upload rather than hidden behind a failure: the grant dies on a
    // schedule Google controls, so reconnecting is routine maintenance, not an
    // error path.
    let connect_btn = unsafe {
        NSButton::buttonWithTitle_target_action(
            &NSString::from_str("Connect…"),
            Some(&target),
            Some(sel!(onYoutubeConnect:)),
            mtm,
        )
    };
    connect_btn.setFrame(NSRect::new(
        NSPoint::new(PAD * 2.0 + 172.0, bounds.size.height - 84.0),
        NSSize::new(110.0, 28.0),
    ));
    pin_top(&connect_btn);
    publish_view.addSubview(&connect_btn);

    // Beside Upload, not buried in the summary text below it, because it is the
    // one property of the upload that cannot be corrected from this app
    // afterwards: `publish` refuses a second press on the same render, so a
    // video that went up public is public until someone changes it on YouTube.
    // A setting with that little margin for error belongs next to the button
    // that commits it.
    let privacy_label = NSTextField::labelWithString(&NSString::from_str("Visibility"), mtm);
    privacy_label.setFrame(NSRect::new(
        NSPoint::new(PAD * 2.0 + 294.0, bounds.size.height - 82.0),
        NSSize::new(62.0, 24.0),
    ));
    pin_top(&privacy_label);
    publish_view.addSubview(&privacy_label);

    let privacy_popup = make_popup(
        mtm,
        NSRect::new(
            NSPoint::new(PAD * 2.0 + 360.0, bounds.size.height - 84.0),
            NSSize::new(120.0, CONTROL_H),
        ),
        crate::publish::youtube::Privacy::ALL
            .iter()
            .map(|privacy| privacy.label().to_string()),
        youtube_privacy.index(),
        &target,
        sel!(onYoutubePrivacyChanged:),
    );
    pin_top(&privacy_popup);
    publish_view.addSubview(&privacy_popup);
    *target.ivars().privacy_popup.borrow_mut() = Some(privacy_popup.clone());

    let publish_status = NSTextField::labelWithString(
        &NSString::from_str("Render the video, then save its details below."),
        mtm,
    );
    publish_status.setFrame(NSRect::new(
        NSPoint::new(PAD * 2.0 + 492.0, bounds.size.height - 82.0),
        NSSize::new((bounds.size.width - PAD * 4.0 - 492.0).max(120.0), 24.0),
    ));
    pin_top(&publish_status);
    publish_view.addSubview(&publish_status);
    *target.ivars().publish_status.borrow_mut() = Some(publish_status.clone());

    let publish_pane = WebPane::attach(&publish_view, mtm, tx.clone());
    publish_pane.set_frame(NSRect::new(
        NSPoint::new(PAD * 2.0, PAD * 2.0),
        NSSize::new(bounds.size.width - PAD * 4.0, bounds.size.height - 120.0),
    ));
    publish_pane.fill_below();

    let publish_item = unsafe {
        NSTabViewItem::initWithIdentifier(
            NSTabViewItem::alloc(),
            Some(&NSString::from_str("youtube")),
        )
    };
    publish_item.setLabel(&NSString::from_str("YouTube"));
    publish_item.setView(Some(&publish_view));

    // ==========================================
    // TAB 6: BLOG (the longform as a /blog video post)
    // ==========================================
    // Behind YouTube deliberately, because it depends on it: the post is built
    // around a YouTube embed, and `video.url` has to be a real watch URL or the
    // page renders no player at all. See [`crate::blog`].
    let blog_view = NSView::initWithFrame(NSView::alloc(mtm), bounds);
    fill_parent(&blog_view);

    let blog_status = NSTextField::labelWithString(
        &NSString::from_str(
            "Writes the article and creates it as a draft in Strapi. One model call.\n\
             Review it there and publish from the CMS.",
        ),
        mtm,
    );
    blog_status.setFrame(NSRect::new(
        NSPoint::new(PAD * 2.0, bounds.size.height - 34.0),
        NSSize::new(bounds.size.width - PAD * 4.0, 20.0),
    ));
    pin_top(&blog_status);
    blog_view.addSubview(&blog_status);
    *target.ivars().blog_status.borrow_mut() = Some(blog_status.clone());

    let blog_pane = WebPane::attach(&blog_view, mtm, tx.clone());
    blog_pane.set_frame(NSRect::new(
        NSPoint::new(PAD, PAD),
        NSSize::new(bounds.size.width - PAD * 2.0, bounds.size.height - 44.0),
    ));
    blog_pane.fill_below();

    let blog_item = unsafe {
        NSTabViewItem::initWithIdentifier(
            NSTabViewItem::alloc(),
            Some(&NSString::from_str("blog")),
        )
    };
    blog_item.setLabel(&NSString::from_str("Blog"));
    blog_item.setView(Some(&blog_view));

    // ==========================================
    // TAB 7: SCHEDULE
    // ==========================================
    let schedule_view = NSView::initWithFrame(NSView::alloc(mtm), bounds);
    fill_parent(&schedule_view);

    let schedule_title = NSTextField::labelWithString(
        &NSString::from_str("Buffer — review and queue social posts"),
        mtm,
    );
    schedule_title.setFrame(NSRect::new(
        NSPoint::new(PAD * 2.0, bounds.size.height - 48.0),
        NSSize::new(600.0, 20.0),
    ));
    pin_top(&schedule_title);
    schedule_view.addSubview(&schedule_title);

    // Two steps, deliberately separate: Build Plan only reads, Queue only sends
    // what the plan below already showed.
    let plan_btn = unsafe {
        NSButton::buttonWithTitle_target_action(
            &NSString::from_str("Build Plan"),
            Some(&target),
            Some(sel!(onSchedulePlan:)),
            mtm,
        )
    };
    plan_btn.setFrame(NSRect::new(
        NSPoint::new(PAD * 2.0, bounds.size.height - 84.0),
        NSSize::new(120.0, 28.0),
    ));
    pin_top(&plan_btn);
    schedule_view.addSubview(&plan_btn);
    *target.ivars().plan_button.borrow_mut() = Some(plan_btn.clone());

    let approve_all_btn = unsafe {
        NSButton::buttonWithTitle_target_action(
            &NSString::from_str("Approve All"),
            Some(&target),
            Some(sel!(onScheduleApproveAll:)),
            mtm,
        )
    };
    approve_all_btn.setFrame(NSRect::new(
        NSPoint::new(PAD * 2.0 + 128.0, bounds.size.height - 84.0),
        NSSize::new(110.0, 28.0),
    ));
    pin_top(&approve_all_btn);
    schedule_view.addSubview(&approve_all_btn);
    *target.ivars().approve_button.borrow_mut() = Some(approve_all_btn.clone());

    let queue_btn = unsafe {
        NSButton::buttonWithTitle_target_action(
            &NSString::from_str("Queue to Buffer"),
            Some(&target),
            Some(sel!(onScheduleQueue:)),
            mtm,
        )
    };
    queue_btn.setFrame(NSRect::new(
        NSPoint::new(PAD * 2.0 + 246.0, bounds.size.height - 84.0),
        NSSize::new(150.0, 28.0),
    ));
    pin_top(&queue_btn);
    schedule_view.addSubview(&queue_btn);
    *target.ivars().queue_button.borrow_mut() = Some(queue_btn.clone());

    let clear_btn = unsafe {
        NSButton::buttonWithTitle_target_action(
            &NSString::from_str("Clear Queue…"),
            Some(&target),
            Some(sel!(onScheduleClear:)),
            mtm,
        )
    };
    clear_btn.setFrame(NSRect::new(
        NSPoint::new(PAD * 2.0 + 404.0, bounds.size.height - 84.0),
        NSSize::new(130.0, 28.0),
    ));
    pin_top(&clear_btn);
    schedule_view.addSubview(&clear_btn);

    let sched_status = NSTextField::labelWithString(
        &NSString::from_str("Build a plan, review it, then queue."),
        mtm,
    );
    sched_status.setFrame(NSRect::new(
        NSPoint::new(PAD * 2.0 + 546.0, bounds.size.height - 82.0),
        NSSize::new((bounds.size.width - PAD * 4.0 - 546.0).max(120.0), 24.0),
    ));
    pin_top(&sched_status);
    schedule_view.addSubview(&sched_status);
    *target.ivars().schedule_status.borrow_mut() = Some(sched_status.clone());

    // The approve list fills the tab; the explainer/summary keeps a fixed strip at
    // the bottom so the rows a human has to tick always get the growing half.
    let sched_w = bounds.size.width - PAD * 4.0;
    let sched_info_h = 150.0;
    let sched_form_h = (bounds.size.height - 120.0 - sched_info_h - PAD).max(80.0);
    let schedule_form = crate::schedule::ScheduleForm::attach(&schedule_view, mtm);
    schedule_form.set_frame(NSRect::new(
        NSPoint::new(PAD * 2.0, PAD * 2.0 + sched_info_h + PAD),
        NSSize::new(sched_w, sched_form_h),
    ));
    schedule_form.show_empty();

    let sched_scroll = NSScrollView::initWithFrame(
        NSScrollView::alloc(mtm),
        NSRect::new(
            NSPoint::new(PAD * 2.0, PAD * 2.0),
            NSSize::new(sched_w, sched_info_h),
        ),
    );
    pin_bottom(&sched_scroll);
    sched_scroll.setHasVerticalScroller(true);
    let sched_text_view = NSTextView::initWithFrame(
        NSTextView::alloc(mtm),
        NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(sched_w, sched_info_h)),
    );
    sched_text_view.setEditable(false);
    sched_text_view.setString(&NSString::from_str(
        "Three steps.\n\n\
         1. Build Plan — pairs each generated post with its public S3 URL, a live \
         Buffer channel and the exact metadata it will send (YouTube privacy, \
         subscriber notification, Instagram reel/feed), then writes \
         schedule/vN/schedule.json. Nothing is posted.\n\n\
         2. Approve — tick the posts above that should go out. Nothing is sent \
         without a tick. Approval is bound to the copy: re-plan and an identical \
         caption keeps its tick, while a regenerated or edited one comes back \
         unticked, so you can never approve one caption and ship another. Blocked \
         rows show why and cannot be ticked.\n\n\
         3. Queue to Buffer — sends the approved items and nothing else. Every send \
         is appended to schedule.jsonl at the project root, so a second press \
         re-sends nothing: anything already in the ledger under the same copy is \
         skipped. Regenerating the posts changes the copy, which makes an item \
         queueable again — the plan flags those with a warning naming the post that \
         is already live.",
    ));
    sched_scroll.setDocumentView(Some(&sched_text_view));
    schedule_view.addSubview(&sched_scroll);
    *target.ivars().schedule_info.borrow_mut() = Some(sched_text_view.clone());

    let schedule_item = unsafe {
        NSTabViewItem::initWithIdentifier(
            NSTabViewItem::alloc(),
            Some(&NSString::from_str("schedule")),
        )
    };
    schedule_item.setLabel(&NSString::from_str("Buffer"));
    schedule_item.setView(Some(&schedule_view));

    // ==========================================
    // TAB 8: ANALYTICS
    // ==========================================
    let analytics_view = NSView::initWithFrame(NSView::alloc(mtm), bounds);
    fill_parent(&analytics_view);

    let analytics_title = NSTextField::labelWithString(
        &NSString::from_str("Analytics — 7 and 30 days after each post"),
        mtm,
    );
    analytics_title.setFrame(NSRect::new(
        NSPoint::new(PAD * 2.0, bounds.size.height - 48.0),
        NSSize::new(600.0, 20.0),
    ));
    pin_top(&analytics_title);
    analytics_view.addSubview(&analytics_title);

    let pull_btn = unsafe {
        NSButton::buttonWithTitle_target_action(
            &NSString::from_str("Pull & Report"),
            Some(&target),
            Some(sel!(onPullAnalytics:)),
            mtm,
        )
    };
    pull_btn.setFrame(NSRect::new(
        NSPoint::new(PAD * 2.0, bounds.size.height - 84.0),
        NSSize::new(140.0, 28.0),
    ));
    pin_top(&pull_btn);
    analytics_view.addSubview(&pull_btn);

    let collect_btn = unsafe {
        NSButton::buttonWithTitle_target_action(
            &NSString::from_str("Collect All Due"),
            Some(&target),
            Some(sel!(onCollectAllAnalytics:)),
            mtm,
        )
    };
    collect_btn.setFrame(NSRect::new(
        NSPoint::new(PAD * 2.0 + 148.0, bounds.size.height - 84.0),
        NSSize::new(140.0, 28.0),
    ));
    pin_top(&collect_btn);
    analytics_view.addSubview(&collect_btn);

    let analytics_status = NSTextField::labelWithString(
        &NSString::from_str("Queue posts first, then pull once they have gone out."),
        mtm,
    );
    analytics_status.setFrame(NSRect::new(
        NSPoint::new(PAD * 2.0 + 296.0, bounds.size.height - 82.0),
        NSSize::new((bounds.size.width - PAD * 4.0 - 296.0).max(120.0), 24.0),
    ));
    pin_top(&analytics_status);
    analytics_view.addSubview(&analytics_status);
    *target.ivars().analytics_status.borrow_mut() = Some(analytics_status.clone());

    let analytics_scroll = NSScrollView::initWithFrame(
        NSScrollView::alloc(mtm),
        NSRect::new(
            NSPoint::new(PAD * 2.0, PAD * 2.0),
            NSSize::new(bounds.size.width - PAD * 4.0, bounds.size.height - 120.0),
        ),
    );
    fill_below(&analytics_scroll);
    analytics_scroll.setHasVerticalScroller(true);
    let analytics_text_view = NSTextView::initWithFrame(
        NSTextView::alloc(mtm),
        NSRect::new(
            NSPoint::new(0.0, 0.0),
            NSSize::new(bounds.size.width - PAD * 4.0, bounds.size.height - 120.0),
        ),
    );
    analytics_text_view.setEditable(false);
    analytics_text_view.setString(&NSString::from_str(
        "Press Pull & Report.\n\n\
         Buffer decides when a queued post actually goes out, so every window is \
         measured from its real send time, not from when it was queued. Each pull \
         collects whatever has come due since the last one — leave the app closed \
         for a fortnight and the samples land late, but they land.\n\n\
         Samples: 7 days and 30 days after sending. Rows are joined to \
         schedule.jsonl on buffer_post_id, so each one carries the chapter, the \
         platform, and the prompt that wrote the copy. Posts queued outside this \
         app are not in that ledger and so are not in the report.\n\n\
         Written to analytics.jsonl and analytics-report.md at the project root.",
    ));
    analytics_scroll.setDocumentView(Some(&analytics_text_view));
    analytics_view.addSubview(&analytics_scroll);
    *target.ivars().analytics_info.borrow_mut() = Some(analytics_text_view.clone());

    let analytics_item = unsafe {
        NSTabViewItem::initWithIdentifier(
            NSTabViewItem::alloc(),
            Some(&NSString::from_str("analytics")),
        )
    };
    analytics_item.setLabel(&NSString::from_str("Analytics"));
    *target.ivars().analytics_tab.borrow_mut() = Some(analytics_item.clone());
    analytics_item.setView(Some(&analytics_view));

    // ==========================================
    // TAB 9: REFLECT  (rendered HTML, see `ui::web`)
    // ==========================================
    let reflect_view = NSView::initWithFrame(NSView::alloc(mtm), bounds);
    fill_parent(&reflect_view);

    // One native line for live progress — a status arriving mid-run should not
    // cost a whole-pane re-render, which would throw away the reader's scroll.
    let reflect_status = NSTextField::labelWithString(
        &NSString::from_str("Reads local files. Only Reflect and Validate cost a model call."),
        mtm,
    );
    reflect_status.setFrame(NSRect::new(
        NSPoint::new(PAD * 2.0, bounds.size.height - 34.0),
        NSSize::new(bounds.size.width - PAD * 4.0, 20.0),
    ));
    pin_top(&reflect_status);
    reflect_view.addSubview(&reflect_status);
    *target.ivars().reflect_status.borrow_mut() = Some(reflect_status.clone());

    // Everything else is the HTML pane: header, buttons, rows and diff all come
    // from the template, so there is no native layout left to keep in step.
    let reflect_pane = WebPane::attach(&reflect_view, mtm, tx.clone());
    reflect_pane.set_frame(NSRect::new(
        NSPoint::new(PAD, PAD),
        NSSize::new(bounds.size.width - PAD * 2.0, bounds.size.height - 44.0),
    ));
    reflect_pane.fill_below();
    reflect_pane.show(&render::page(
        "reflect.html",
        minijinja::context! { report => None::<()>, can_apply => false, applicable => 0 },
    ));

    let reflect_item = unsafe {
        NSTabViewItem::initWithIdentifier(
            NSTabViewItem::alloc(),
            Some(&NSString::from_str("reflect")),
        )
    };
    reflect_item.setLabel(&NSString::from_str("Reflect"));
    reflect_item.setView(Some(&reflect_view));

    // ==========================================
    // SETTINGS (API keys)
    // ==========================================
    // Deliberately outside `workflow::attach`: the five steps describe how a
    // video gets made, and this is not one of them — it is the thing you open
    // once on a new machine and then never again. Appending it here keeps the
    // workflow contract in `workflow::STEPS` describing only the workflow,
    // while still putting Settings last in the same row of tabs.
    let settings_view = NSView::initWithFrame(NSView::alloc(mtm), bounds);
    fill_parent(&settings_view);
    let settings_pane = WebPane::attach(&settings_view, mtm, tx.clone());
    settings_pane.set_frame(NSRect::new(
        NSPoint::new(PAD, PAD),
        NSSize::new(bounds.size.width - PAD * 2.0, bounds.size.height - 44.0),
    ));
    settings_pane.fill_below();
    settings_pane.show(&settings_page(None));

    let settings_item = unsafe {
        NSTabViewItem::initWithIdentifier(
            NSTabViewItem::alloc(),
            Some(&NSString::from_str("settings")),
        )
    };
    settings_item.setLabel(&NSString::from_str("Settings"));
    settings_item.setView(Some(&settings_view));

    workflow::attach(&tab_view, bounds, mtm, &[
        ("draft", &draft_item), ("review", &review_item),
        ("thumbnails", &thumbnail_item), ("youtube", &publish_item), ("blog", &blog_item),
        ("post", &post_item), ("distribute", &distribute_item), ("schedule", &schedule_item),
        ("analytics", &analytics_item), ("reflect", &reflect_item),
    ]);
    tab_view.addTabViewItem(&settings_item);

    let layout = Layout {
        left: left.clone(),
        right: right.clone(),
        status,
        timer: (timer, timer_detail),
        meter,
        left_sections,
        project_name: project_name_field,
        left_groups,
        notes_sections,
        prompt: (prompt_label, prompt_field),
        notes_group: (notes_box, notes_buttons.to_vec()),
        render_status,
        last: Cell::new((0, 0, 0, 0)),
    };
    layout.sync(&preview, &notes);

    Ok(Attached {
        rx,
        tx,
        target,
        preview,
        notes,
        posts_form,
        schedule_form,
        reflect_pane,
        thumbnail_pane,
        review_pane,
        edit_pane: None,
        substack_pane: None,
        blog_pane,
        publish_pane,
        video_brief_pane,
        settings_pane,
        layout,
    })
}

#[cfg(test)]
mod settings_pane_tests {
    /// `render::page` turns a template failure into an error *page* rather than
    /// an `Err`, so a broken loop or a renamed field would ship as a red panel
    /// nobody sees until they open the tab. This is the check that a real
    /// context renders.
    #[test]
    fn the_settings_pane_renders_every_section() {
        let html = super::settings_page(Some("Saved."));
        assert!(!html.contains("template error"), "{html}");
        for group in crate::settings::Group::ALL {
            assert!(html.contains(group.title()), "missing section {}", group.title());
            assert!(
                html.contains(&format!("result-{}", group.slug())),
                "missing test-result slot for {}",
                group.slug()
            );
        }
        for field in crate::settings::FIELDS {
            assert!(html.contains(field.key), "missing input for {}", field.key);
        }
        assert!(html.contains("Saved."), "the status note is not drawn");
    }

    /// The pane is handed to a webview, so a stored secret in the HTML is a
    /// secret one `view-source` away. Only the four-character tail may appear.
    #[test]
    fn a_stored_secret_never_reaches_the_page() {
        // SAFETY: single-threaded test, and the value is scoped to this process.
        unsafe { std::env::set_var("OPENROUTER_API_KEY", "sk-or-v1-topsecretvalue9999") };
        let html = super::settings_page(None);
        unsafe { std::env::remove_var("OPENROUTER_API_KEY") };
        assert!(!html.contains("topsecretvalue"), "the pane leaked a stored key");
        assert!(html.contains("…9999"), "the tail hint is missing: {html}");
        // Not supplied by the team file in this test, so it must not claim to be.
        assert!(!html.contains("from the team"), "misattributed a local value");
    }
}

#[cfg(test)]
mod tests {
    use super::common_ancestor;
    use std::path::{Path, PathBuf};

    /// The scope the thumbnail pane needs: the project holds the stills and
    /// candidates, the library beside it holds the style references, and a
    /// webview granted only the project silently refuses every reference.
    #[test]
    fn the_project_and_the_library_share_the_apps_own_folder() {
        let session = Path::new("/Users/andrew/.stream-recorder/sessions/2026-08-15_18-19-05");
        let library = Path::new("/Users/andrew/.stream-recorder/references");
        assert_eq!(
            common_ancestor(session, library),
            PathBuf::from("/Users/andrew/.stream-recorder"),
            "scoped to the app's folder, not the whole home directory"
        );
    }

    #[test]
    fn a_path_inside_the_other_is_the_ancestor() {
        assert_eq!(
            common_ancestor(Path::new("/a/b/c"), Path::new("/a/b")),
            PathBuf::from("/a/b")
        );
    }

    /// A `THUMBNAIL_REFERENCES` pointing somewhere unrelated still resolves to a
    /// real directory rather than an empty path.
    #[test]
    fn unrelated_trees_fall_back_to_the_root() {
        assert_eq!(
            common_ancestor(Path::new("/Users/andrew/x"), Path::new("/Volumes/refs")),
            PathBuf::from("/")
        );
    }
}
