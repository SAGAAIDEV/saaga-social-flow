//! The Rust ↔ webview bridge: an HTML pane that behaves like a native control.
//!
//! # The pattern
//!
//! Two directions, deliberately asymmetric.
//!
//! **Rust → webview: whole-pane HTML.** Rust renders a Jinja template and swaps
//! it in. There is no incremental update and no state in JavaScript, so the pane
//! cannot disagree with the files on disk — the class of bug that made the native
//! forms carry ticks by identity, preserve on-screen state across repaints, and
//! save before acting. Here the answer is simply that there is nothing to sync.
//!
//! **Webview → Rust: one message channel.** JavaScript posts a JSON string to a
//! single handler; Rust parses it into a [`WebEvent`] and forwards it as an
//! ordinary [`UiEvent`], the same enum the AppKit buttons post. Nothing downstream
//! knows or cares whether a click came from an `NSButton` or an `<input>`.
//!
//! So a pane is: a template, a serializable context, and a few event variants.
//! Adding one touches no other pane.
//!
//! # Why HTML posts a string
//!
//! `postMessage` can send a JS object, which arrives as an `NSDictionary` and has
//! to be bridged key by key. Posting `JSON.stringify(payload)` instead makes the
//! whole boundary one `serde_json::from_str`, typed on arrival.

use std::sync::mpsc::Sender;

use objc2::rc::Retained;
use objc2::runtime::{NSObject, NSObjectProtocol};
use objc2::{define_class, DefinedClass, MainThreadMarker, MainThreadOnly};
use objc2_app_kit::NSView;
use objc2_foundation::{NSPoint, NSRect, NSSize, NSString};
use objc2_web_kit::{
    WKScriptMessage, WKScriptMessageHandler, WKUserContentController, WKWebView,
    WKWebViewConfiguration,
};
use serde::Deserialize;

use super::UiEvent;

/// The name JavaScript posts to: `window.webkit.messageHandlers.app.postMessage(…)`.
const HANDLER: &str = "app";

/// Everything an HTML pane can ask the app to do.
///
/// Tagged by `type`, so JavaScript sends `{"type":"validate","index":2}` and this
/// is the only place the wire format is described.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum WebEvent {
    /// A checkbox moved. Carries the value rather than toggling, so a dropped
    /// message cannot invert every later click.
    Approve {
        index: usize,
        value: bool,
    },
    Validate {
        index: usize,
    },
    Apply,
    Reflect,
    /// Grab the current camera frame as the thumbnail's subject.
    CaptureFrame,
    /// Grab the screen alone, keeping the photo that was already taken.
    CaptureScreen,
    GenerateThumbnails,
    /// Draw the procedural card.
    DrawCard,
    /// The edited brief, field by field.
    SaveBrief {
        fields: std::collections::BTreeMap<String, String>,
    },
    /// The edited card, field by field.
    ImportPortrait {
        data: String,
    },
    GenerateArtwork {
        fields: std::collections::BTreeMap<String, String>,
    },
    SaveVideoBrief {
        fields: std::collections::BTreeMap<String, String>,
        apply: bool,
    },
    GenerateVideoCopy {
        fields: std::collections::BTreeMap<String, String>,
    },
    SaveYoutube {
        fields: std::collections::BTreeMap<String, String>,
    },
    SaveCard {
        fields: std::collections::BTreeMap<String, String>,
    },
    SelectThumbnail {
        id: String,
    },
    /// The image model picked in the thumbnail pane, by OpenRouter id.
    ThumbnailModel {
        value: String,
    },
    ToggleReference {
        name: String,
        value: bool,
    },
    RemoveReference {
        name: String,
    },
    /// A dropped image: base64 payload, and the name it arrived with — which is
    /// untrusted, and sanitised before it reaches the filesystem.
    AddReference {
        name: String,
        data: String,
    },

    /// Open a chapter in the Edit tab. The only one of these four that repaints.
    OpenChapter {
        chapter: u32,
    },
    /// The hand-edited keep-list, whole rather than as a change.
    ///
    /// Same reasoning as [`WebEvent::Approve`] carrying its value: a dropped or
    /// duplicated message costs the last action, where a delta would leave the pane and
    /// the file disagreeing about a video with no way to tell which was right.
    SaveEdit {
        chapter: u32,
        spans: Vec<[i64; 2]>,
    },
    /// Cut this chapter to its keep-list and re-render it.
    ApplyEdit {
        chapter: u32,
    },
    /// Throw the hand edit away and go back to the automatic cut.
    ResetEdit {
        chapter: u32,
    },

    GenerateSubstack,
    /// Seed and open the standing `substack.notes` prompt for editing.
    EditSubstackPrompt,

    PublishBlog,
    /// Write the article to disk without uploading anything.
    WriteBlog,
    /// Render the draft locally and open it.
    PreviewBlog,
    /// Seed and open the standing `blog.article` prompt for editing.
    EditBlogPrompt,
    /// Re-read the author and category lists from Strapi.
    RefreshBlogLibrary,
    /// The byline picked in the blog pane, as a Strapi relation id. Empty for
    /// "no author".
    BlogAuthor {
        value: String,
    },
    /// The category picked in the blog pane, as a relation id.
    BlogCategory {
        value: String,
    },
    /// The Blog tab's "Needs fixing" card, whole: over-limit field key → text.
    SaveBlogFields {
        fields: std::collections::BTreeMap<String, String>,
    },
    /// Shorten every over-limit field of the draft with the model.
    RepairBlog,
    /// Open the drag-to-select overlay for a figure. The chord is the usual
    /// way in; the button is for the case where the thing worth showing is
    /// already on screen and still there.
    CaptureFigure,
    /// Write a blurb for every captured figure that has none.
    WriteBlurbs,

    /// Put `text` on the system pasteboard.
    ///
    /// The words travel rather than an id, because the pane already holds
    /// exactly what is on screen — a copy button that re-fetched its own text
    /// from disk could hand over something the reader is not looking at.
    CopyText {
        text: String,
    },

    /// The Settings form. Only the boxes that were filled in travel: the pane
    /// is never handed a stored secret, so an untouched box arrives absent
    /// rather than empty, and `settings::write` leaves what it does not hear
    /// about alone.
    SaveSettings {
        fields: std::collections::BTreeMap<String, String>,
    },
    /// Check one group of credentials against the live service, by
    /// `settings::Group::slug`.
    TestSettings {
        service: String,
    },
}

impl WebEvent {
    /// Maps onto the event stream the native controls already use.
    pub fn into_ui_event(self) -> UiEvent {
        match self {
            WebEvent::Approve { index, value } => UiEvent::WebApprove { index, value },
            WebEvent::Validate { index } => UiEvent::ValidateRewrite(index),
            WebEvent::Apply => UiEvent::Action(crate::hotkeys::Action::ApplyRewrites),
            WebEvent::Reflect => UiEvent::Action(crate::hotkeys::Action::Reflect),
            WebEvent::CaptureFrame => UiEvent::Action(crate::hotkeys::Action::CaptureFrame),
            WebEvent::CaptureScreen => UiEvent::Action(crate::hotkeys::Action::CaptureScreen),
            WebEvent::GenerateThumbnails => {
                UiEvent::Action(crate::hotkeys::Action::GenerateThumbnails)
            }
            WebEvent::DrawCard => UiEvent::Action(crate::hotkeys::Action::DrawCard),
            WebEvent::SaveBrief { fields } => UiEvent::SaveBrief(fields),
            WebEvent::SaveCard { fields } => UiEvent::SaveCard(fields),
            WebEvent::GenerateArtwork { fields } => UiEvent::GenerateArtwork(fields),
            WebEvent::ImportPortrait { data } => UiEvent::ImportPortrait(data),
            WebEvent::SaveVideoBrief { fields, apply } => UiEvent::SaveVideoBrief { fields, apply },
            WebEvent::GenerateVideoCopy { fields } => UiEvent::GenerateVideoCopy(fields),
            WebEvent::SaveYoutube { fields } => UiEvent::SaveYoutube(fields),
            WebEvent::SelectThumbnail { id } => UiEvent::SelectThumbnail(id),
            WebEvent::ThumbnailModel { value } => UiEvent::ThumbnailModelSelected(value),
            WebEvent::ToggleReference { name, value } => UiEvent::ToggleReference { name, value },
            WebEvent::AddReference { name, data } => UiEvent::AddReference { name, data },
            WebEvent::RemoveReference { name } => UiEvent::RemoveReference(name),
            WebEvent::OpenChapter { chapter } => UiEvent::OpenChapter(chapter),
            WebEvent::SaveEdit { chapter, spans } => UiEvent::SaveEdit { chapter, spans },
            WebEvent::ApplyEdit { chapter } => UiEvent::ApplyEdit(chapter),
            WebEvent::ResetEdit { chapter } => UiEvent::ResetEdit(chapter),
            WebEvent::GenerateSubstack => UiEvent::Action(crate::hotkeys::Action::GenerateSubstack),
            WebEvent::EditSubstackPrompt => {
                UiEvent::Action(crate::hotkeys::Action::EditSubstackPrompt)
            }
            WebEvent::CaptureFigure => UiEvent::Action(crate::hotkeys::Action::CaptureFigure),
            WebEvent::WriteBlurbs => UiEvent::Action(crate::hotkeys::Action::WriteBlurbs),
            WebEvent::PublishBlog => UiEvent::Action(crate::hotkeys::Action::PublishBlog),
            WebEvent::WriteBlog => UiEvent::Action(crate::hotkeys::Action::WriteBlog),
            WebEvent::PreviewBlog => UiEvent::Action(crate::hotkeys::Action::PreviewBlog),
            WebEvent::EditBlogPrompt => UiEvent::Action(crate::hotkeys::Action::EditBlogPrompt),
            WebEvent::RefreshBlogLibrary => {
                UiEvent::Action(crate::hotkeys::Action::RefreshBlogLibrary)
            }
            WebEvent::BlogAuthor { value } => UiEvent::BlogAuthorSelected(value),
            WebEvent::BlogCategory { value } => UiEvent::BlogCategorySelected(value),
            WebEvent::SaveBlogFields { fields } => UiEvent::SaveBlogFields(fields),
            WebEvent::RepairBlog => UiEvent::RepairBlog,
            WebEvent::CopyText { text } => UiEvent::CopyText(text),
            WebEvent::SaveSettings { fields } => UiEvent::SaveSettings(fields),
            WebEvent::TestSettings { service } => UiEvent::TestSettings(service),
        }
    }
}

pub struct MessageHandlerIvars {
    tx: Sender<UiEvent>,
}

define_class!(
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "StreamRecorderWebBridge"]
    #[ivars = MessageHandlerIvars]
    pub struct MessageHandler;

    unsafe impl NSObjectProtocol for MessageHandler {}

    unsafe impl WKScriptMessageHandler for MessageHandler {
        #[unsafe(method(userContentController:didReceiveScriptMessage:))]
        fn did_receive(&self, _controller: &WKUserContentController, message: &WKScriptMessage) {
            let body = unsafe { message.body() };
            let Ok(text) = body.downcast::<NSString>() else {
                eprintln!("stream-recorder: webview posted a non-string message");
                return;
            };
            let text = text.to_string();
            match serde_json::from_str::<WebEvent>(&text) {
                Ok(event) => {
                    let _ = self.ivars().tx.send(event.into_ui_event());
                }
                // A message we cannot parse is a bug in the pane, not the user's
                // problem — say what arrived so it is findable.
                Err(err) => eprintln!("stream-recorder: bad webview message {text:?}: {err}"),
            }
        }
    }
);

impl MessageHandler {
    fn new(mtm: MainThreadMarker, tx: Sender<UiEvent>) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(MessageHandlerIvars { tx });
        unsafe { objc2::msg_send![super(this), init] }
    }
}

/// An HTML pane living in the AppKit view tree.
pub struct WebPane {
    webview: Retained<WKWebView>,
    /// The controller holds the handler weakly, so dropping this would silently
    /// stop every button in the pane from working.
    _handler: Retained<MessageHandler>,
}

impl WebPane {
    pub fn attach(parent: &NSView, mtm: MainThreadMarker, tx: Sender<UiEvent>) -> WebPane {
        let handler = MessageHandler::new(mtm, tx);
        let controller = unsafe { WKUserContentController::new(mtm) };
        unsafe {
            controller.addScriptMessageHandler_name(
                objc2::runtime::ProtocolObject::from_ref(&*handler),
                &NSString::from_str(HANDLER),
            );
        }
        let config = unsafe { WKWebViewConfiguration::new(mtm) };
        unsafe { config.setUserContentController(&controller) };

        let webview = unsafe {
            WKWebView::initWithFrame_configuration(
                WKWebView::alloc(mtm),
                NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(100.0, 100.0)),
                &config,
            )
        };
        parent.addSubview(&webview);
        WebPane {
            webview,
            _handler: handler,
        }
    }

    pub fn set_frame(&self, frame: NSRect) {
        self.webview.setFrame(frame);
    }

    /// Shows HTML that references files on disk.
    ///
    /// `loadHTMLString` has no base URL, so `file://` images never resolve — fine
    /// for text panes, useless for a grid of pictures. This writes the page beside
    /// the images and grants read access to `root` only, so the pane can show a
    /// project's thumbnails and nothing else on the machine.
    /// Writes the page into `root` and loads it, able to read anything under
    /// `access`.
    ///
    /// The two are separate because a pane's images are not all in one place:
    /// the thumbnail pane shows stills and candidates from the project *and*
    /// style references from the shared library beside it. `allowingReadAccessTo`
    /// takes a single directory, so anything outside it is silently refused —
    /// which is what made every reference render as a broken image.
    /// `page` is the file name this pane writes into `root`, and every pane needs
    /// its own. `loadFileURL` reads asynchronously, so two panes repainted in the
    /// same tick through one shared file would race — the second write landing
    /// before the first webview had read it, and a pane showing another's page.
    pub fn show_local(
        &self,
        html: &str,
        root: &std::path::Path,
        access: &std::path::Path,
        page: &str,
    ) {
        let page = root.join(page);
        if let Err(err) = std::fs::write(&page, html) {
            eprintln!("stream-recorder: could not write {}: {err}", page.display());
            return;
        }
        let Some(url) = path_url(&page) else { return };
        let Some(access) = path_url(access) else {
            return;
        };
        unsafe {
            self.webview
                .loadFileURL_allowingReadAccessToURL(&url, &access);
        }
    }

    /// Grows with its parent, so the pane never needs relayout code of its own —
    /// the HTML reflows instead.
    pub fn fill_below(&self) {
        self.webview.setAutoresizingMask(
            objc2_app_kit::NSAutoresizingMaskOptions::ViewWidthSizable
                | objc2_app_kit::NSAutoresizingMaskOptions::ViewHeightSizable,
        );
    }

    /// Runs JavaScript in the pane, leaving the page as it is.
    ///
    /// For updating one element when a redraw would be wrong — the Settings
    /// pane's test results land while there is half-typed text in the boxes
    /// around them, and `show` would throw that away.
    ///
    /// Errors are dropped: the only caller sends a fixed script with a
    /// serialised payload, so a failure here is a bug in that script rather
    /// than a condition the pane can act on.
    pub fn eval(&self, script: &str) {
        unsafe {
            self.webview
                .evaluateJavaScript_completionHandler(&NSString::from_str(script), None);
        }
    }

    /// Replaces the pane with freshly rendered HTML.
    ///
    /// `baseURL` is `None` because every pane is self-contained — styles inline,
    /// no external assets — so there is nothing for a relative path to resolve
    /// against and nothing to ship beside the binary.
    pub fn show(&self, html: &str) {
        unsafe {
            self.webview
                .loadHTMLString_baseURL(&NSString::from_str(html), None);
        }
    }
}

fn path_url(path: &std::path::Path) -> Option<objc2::rc::Retained<objc2_foundation::NSURL>> {
    let text = path.to_str()?;
    Some(objc2_foundation::NSURL::fileURLWithPath(
        &NSString::from_str(text),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn video_brief_events_keep_project_scope_and_apply_intent() {
        let raw = r#"{"type":"saveVideoBrief","fields":{"root":"/tmp/project","notes":"Source notes","title":"Title","description":"Description"},"apply":true}"#;
        let UiEvent::SaveVideoBrief { fields, apply } = serde_json::from_str::<WebEvent>(raw)
            .unwrap()
            .into_ui_event()
        else {
            panic!("wrong event")
        };
        assert!(apply);
        assert_eq!(fields["root"], "/tmp/project");
        assert_eq!(fields["notes"], "Source notes");
        let raw =
            r#"{"type":"generateVideoCopy","fields":{"root":"/tmp/project","notes":"Notes"}}"#;
        assert!(matches!(
            serde_json::from_str::<WebEvent>(raw)
                .unwrap()
                .into_ui_event(),
            UiEvent::GenerateVideoCopy(_)
        ));
    }

    #[test]
    fn a_checkbox_message_parses_with_its_value() {
        let got: WebEvent =
            serde_json::from_str(r#"{"type":"approve","index":2,"value":true}"#).unwrap();
        assert_eq!(
            got,
            WebEvent::Approve {
                index: 2,
                value: true
            }
        );
    }

    #[test]
    fn the_action_messages_parse() {
        assert_eq!(
            serde_json::from_str::<WebEvent>(r#"{"type":"validate","index":0}"#).unwrap(),
            WebEvent::Validate { index: 0 }
        );
        assert_eq!(
            serde_json::from_str::<WebEvent>(r#"{"type":"apply"}"#).unwrap(),
            WebEvent::Apply
        );
        assert_eq!(
            serde_json::from_str::<WebEvent>(r#"{"type":"reflect"}"#).unwrap(),
            WebEvent::Reflect
        );
    }

    /// A pane sending nonsense must be a logged bug, never a panic in the UI thread.
    #[test]
    fn the_thumbnail_messages_parse() {
        assert_eq!(
            serde_json::from_str::<WebEvent>(r#"{"type":"captureFrame"}"#).unwrap(),
            WebEvent::CaptureFrame
        );
        assert_eq!(
            serde_json::from_str::<WebEvent>(r#"{"type":"captureScreen"}"#).unwrap(),
            WebEvent::CaptureScreen
        );
        assert_eq!(
            serde_json::from_str::<WebEvent>(r#"{"type":"selectThumbnail","id":"thumb-a"}"#)
                .unwrap(),
            WebEvent::SelectThumbnail {
                id: "thumb-a".into()
            }
        );
        assert_eq!(
            serde_json::from_str::<WebEvent>(
                r#"{"type":"toggleReference","name":"a.jpg","value":true}"#
            )
            .unwrap(),
            WebEvent::ToggleReference {
                name: "a.jpg".into(),
                value: true
            }
        );
        let brief: WebEvent =
            serde_json::from_str(r#"{"type":"saveBrief","fields":{"mood":"tense"}}"#).unwrap();
        assert!(matches!(brief, WebEvent::SaveBrief { ref fields } if fields["mood"] == "tense"));
    }

    /// The clipboard payload carries the words. A copy button that sent only an
    /// id would have to re-read the file, and could then hand over something
    /// other than what the reader is looking at.
    #[test]
    fn the_substack_messages_parse() {
        assert_eq!(
            serde_json::from_str::<WebEvent>(r#"{"type":"generateSubstack"}"#).unwrap(),
            WebEvent::GenerateSubstack
        );
        assert_eq!(
            serde_json::from_str::<WebEvent>(r#"{"type":"editSubstackPrompt"}"#).unwrap(),
            WebEvent::EditSubstackPrompt
        );
        assert_eq!(
            serde_json::from_str::<WebEvent>(r#"{"type":"copyText","text":"a quote"}"#).unwrap(),
            WebEvent::CopyText {
                text: "a quote".into()
            }
        );
        // Newlines survive the trip, which is the whole of Copy All.
        let all: WebEvent =
            serde_json::from_str(r#"{"type":"copyText","text":"one\ntwo"}"#).unwrap();
        assert_eq!(
            all,
            WebEvent::CopyText {
                text: "one\ntwo".into()
            }
        );
    }

    #[test]
    fn the_blog_messages_parse() {
        assert_eq!(
            serde_json::from_str::<WebEvent>(r#"{"type":"publishBlog"}"#).unwrap(),
            WebEvent::PublishBlog
        );
        assert_eq!(
            serde_json::from_str::<WebEvent>(r#"{"type":"editBlogPrompt"}"#).unwrap(),
            WebEvent::EditBlogPrompt
        );
        assert_eq!(
            serde_json::from_str::<WebEvent>(r#"{"type":"writeBlog"}"#).unwrap(),
            WebEvent::WriteBlog
        );
        assert_eq!(
            serde_json::from_str::<WebEvent>(r#"{"type":"previewBlog"}"#).unwrap(),
            WebEvent::PreviewBlog
        );
        // The figure strip's two buttons live on the same tab. Both map onto
        // hotkey actions, so the button and the chord cannot drift apart.
        assert_eq!(
            serde_json::from_str::<WebEvent>(r#"{"type":"captureFigure"}"#).unwrap(),
            WebEvent::CaptureFigure
        );
        assert_eq!(
            serde_json::from_str::<WebEvent>(r#"{"type":"writeBlurbs"}"#).unwrap(),
            WebEvent::WriteBlurbs
        );
        // `UiEvent` is not `PartialEq` — one variant carries a tracker — so the
        // mapping is checked by shape.
        assert!(matches!(
            WebEvent::CaptureFigure.into_ui_event(),
            UiEvent::Action(crate::hotkeys::Action::CaptureFigure),
        ));
        assert!(matches!(
            WebEvent::WriteBlurbs.into_ui_event(),
            UiEvent::Action(crate::hotkeys::Action::WriteBlurbs),
        ));
    }

    /// The Edit tab's four messages, and the keep-list wire format.
    #[test]
    fn the_edit_messages_parse() {
        assert_eq!(
            serde_json::from_str::<WebEvent>(r#"{"type":"openChapter","chapter":3}"#).unwrap(),
            WebEvent::OpenChapter { chapter: 3 }
        );
        assert_eq!(
            serde_json::from_str::<WebEvent>(r#"{"type":"applyEdit","chapter":3}"#).unwrap(),
            WebEvent::ApplyEdit { chapter: 3 }
        );
        assert_eq!(
            serde_json::from_str::<WebEvent>(r#"{"type":"resetEdit","chapter":3}"#).unwrap(),
            WebEvent::ResetEdit { chapter: 3 }
        );
        assert_eq!(
            serde_json::from_str::<WebEvent>(
                r#"{"type":"saveEdit","chapter":3,"spans":[[0,41200],[43900,118750]]}"#
            )
            .unwrap(),
            WebEvent::SaveEdit {
                chapter: 3,
                spans: vec![[0, 41_200], [43_900, 118_750]],
            }
        );
    }

    /// A keep-list is a video's contents. A malformed one has to be refused at the
    /// boundary rather than parsed into something plausible and then cut.
    #[test]
    fn a_malformed_keep_list_is_refused_not_guessed_at() {
        // A span that is not a pair.
        assert!(serde_json::from_str::<WebEvent>(
            r#"{"type":"saveEdit","chapter":1,"spans":[[0]]}"#
        )
        .is_err());
        assert!(serde_json::from_str::<WebEvent>(
            r#"{"type":"saveEdit","chapter":1,"spans":[[0,1,2]]}"#
        )
        .is_err());
        // Times that are not numbers.
        assert!(serde_json::from_str::<WebEvent>(
            r#"{"type":"saveEdit","chapter":1,"spans":[["a","b"]]}"#
        )
        .is_err());
        // No chapter to attribute it to.
        assert!(
            serde_json::from_str::<WebEvent>(r#"{"type":"saveEdit","spans":[[0,100]]}"#).is_err()
        );
        // An empty list parses — `edit::keep::save` is what refuses to write it, with a
        // message about dropping the whole chapter rather than a parse error.
        assert_eq!(
            serde_json::from_str::<WebEvent>(r#"{"type":"saveEdit","chapter":1,"spans":[]}"#)
                .unwrap(),
            WebEvent::SaveEdit {
                chapter: 1,
                spans: Vec::new()
            }
        );
    }

    #[test]
    fn an_unknown_message_is_an_error_not_a_panic() {
        assert!(serde_json::from_str::<WebEvent>(r#"{"type":"selfDestruct"}"#).is_err());
        assert!(serde_json::from_str::<WebEvent>("not json").is_err());
        assert!(serde_json::from_str::<WebEvent>(r#"{"type":"approve"}"#).is_err());
    }

    /// Carrying the value rather than toggling means a dropped or duplicated
    /// message cannot leave the pane and the report disagreeing.
    #[test]
    fn approve_carries_the_value_rather_than_toggling() {
        let off: WebEvent =
            serde_json::from_str(r#"{"type":"approve","index":1,"value":false}"#).unwrap();
        assert_eq!(
            off,
            WebEvent::Approve {
                index: 1,
                value: false
            }
        );
    }
    /// The "Needs fixing" card posts its boxes under the limits' target keys,
    /// and they have to arrive as typed — the app parses the keys back.
    #[test]
    fn blog_fixes_reach_the_ui_event_under_their_keys() {
        let event: WebEvent = serde_json::from_str(
            r#"{"type":"saveBlogFields","fields":{"quote_text:4":"Shorter.","title":"T"}}"#,
        )
        .unwrap();
        let UiEvent::SaveBlogFields(fields) = event.into_ui_event() else {
            panic!("wrong event")
        };
        assert_eq!(fields["quote_text:4"], "Shorter.");
        assert_eq!(fields["title"], "T");
        let event: WebEvent = serde_json::from_str(r#"{"type":"repairBlog"}"#).unwrap();
        assert!(matches!(event.into_ui_event(), UiEvent::RepairBlog));
    }

    #[test]
    fn youtube_details_reach_the_ui_event() {
        let event: WebEvent = serde_json::from_str(
            r#"{"type":"saveYoutube","fields":{"title":"A video","description":"Details"}}"#,
        )
        .unwrap();
        let UiEvent::SaveYoutube(fields) = event.into_ui_event() else {
            panic!("wrong event")
        };
        assert_eq!(fields["title"], "A video");
        assert_eq!(fields["description"], "Details");
    }
}
