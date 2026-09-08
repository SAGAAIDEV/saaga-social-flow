//! Scrollable form: one labeled editor per generated post.

use std::cell::RefCell;

use objc2::rc::Retained;
use objc2::{MainThreadMarker, MainThreadOnly};
use objc2_app_kit::{NSAutoresizingMaskOptions, NSScrollView, NSTextField, NSTextView, NSView};
use objc2_foundation::{NSPoint, NSRect, NSSize, NSString};

use super::schema::{platform_label, PlatformPost, PostsManifest, VideoPosts};

const PAD: f64 = 12.0;
const LABEL_H: f64 = 18.0;
const TITLE_H: f64 = 26.0;
const TAGS_H: f64 = 26.0;
const SECTION_GAP: f64 = 16.0;

struct Field {
    video_id: String,
    video_type: String,
    platform: String,
    heading: Retained<NSTextField>,
    title: Option<Retained<NSTextField>>,
    body: Retained<NSTextView>,
    body_host: Retained<NSScrollView>,
    tags: Retained<NSTextField>,
}

pub struct PostsForm {
    scroll: Retained<NSScrollView>,
    document: Retained<NSView>,
    placeholder: Retained<NSTextField>,
    fields: RefCell<Vec<Field>>,
    mtm: MainThreadMarker,
}

impl PostsForm {
    pub fn attach(parent: &NSView, mtm: MainThreadMarker) -> PostsForm {
        let scroll = NSScrollView::initWithFrame(
            NSScrollView::alloc(mtm),
            NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(100.0, 100.0)),
        );
        scroll.setHasVerticalScroller(true);
        scroll.setHasHorizontalScroller(false);
        let document = NSView::initWithFrame(
            NSView::alloc(mtm),
            NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(100.0, 100.0)),
        );
        scroll.setDocumentView(Some(&document));
        parent.addSubview(&scroll);

        let placeholder = NSTextField::labelWithString(
            &NSString::from_str(
                "Generate posts to get a separate editor for each video and platform.",
            ),
            mtm,
        );
        document.addSubview(&placeholder);

        PostsForm {
            scroll,
            document,
            placeholder,
            fields: RefCell::new(Vec::new()),
            mtm,
        }
    }

    pub fn set_frame(&self, frame: NSRect) {
        self.scroll.setFrame(frame);
        self.scroll.setAutoresizingMask(
            NSAutoresizingMaskOptions::ViewWidthSizable
                | NSAutoresizingMaskOptions::ViewHeightSizable,
        );
        self.relayout();
    }

    pub fn show(&self, manifest: &PostsManifest) {
        self.clear();
        self.placeholder.setHidden(true);
        let mut fields = Vec::new();
        for item in &manifest.items {
            for post in &item.posts {
                fields.push(self.add_field(item, post));
            }
        }
        *self.fields.borrow_mut() = fields;
        self.relayout();
    }

    pub fn show_empty(&self) {
        self.clear();
        self.placeholder.setHidden(false);
        self.placeholder.setStringValue(&NSString::from_str(
            "Generate posts to get a separate editor for each video and platform.",
        ));
        self.relayout();
    }

    /// Rebuilds the manifest from the on-screen editors.
    ///
    /// `attribution` is the prompt version the copy was generated under, carried
    /// through unchanged. A hand-edited caption is still the model's output plus a
    /// human's correction, and that delta is the signal reflection reads — dropping
    /// the attribution here would throw it away.
    pub fn collect(
        &self,
        version: Option<u32>,
        attribution: (Option<u32>, String),
    ) -> PostsManifest {
        let mut items: Vec<VideoPosts> = Vec::new();
        for field in self.fields.borrow().iter() {
            let title = field.title.as_ref().and_then(|t| {
                let s = t.stringValue().to_string();
                let s = s.trim().to_string();
                (!s.is_empty()).then_some(s)
            });
            let content = field.body.string().to_string();
            let tags = field
                .tags
                .stringValue()
                .to_string()
                .split_whitespace()
                .map(|t| t.trim_start_matches('#').to_string())
                .filter(|t| !t.is_empty())
                .collect();
            let post = PlatformPost {
                platform: field.platform.clone(),
                title,
                content,
                tags,
            };
            if let Some(item) = items
                .iter_mut()
                .find(|i| i.video_id == field.video_id && i.video_type == field.video_type)
            {
                item.posts.push(post);
            } else {
                items.push(VideoPosts {
                    video_id: field.video_id.clone(),
                    video_type: field.video_type.clone(),
                    video_path: None,
                    posts: vec![post],
                });
            }
        }
        let (prompt_version, prompt_hash) = attribution;
        PostsManifest {
            version,
            prompt_version,
            prompt_hash,
            items,
        }
    }

    fn add_field(&self, item: &VideoPosts, post: &PlatformPost) -> Field {
        let heading = NSTextField::labelWithString(
            &NSString::from_str(&format!(
                "{} · {}",
                item.video_id,
                platform_label(&post.platform)
            )),
            self.mtm,
        );
        self.document.addSubview(&heading);

        let wants_title = post.platform == "youtube" || post.platform == "youtube_shorts";
        let title = if wants_title {
            let field = NSTextField::initWithFrame(
                NSTextField::alloc(self.mtm),
                NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(100.0, TITLE_H)),
            );
            field.setBezeled(true);
            field.setEditable(true);
            field.setPlaceholderString(Some(&NSString::from_str("Title")));
            field.setStringValue(&NSString::from_str(post.title.as_deref().unwrap_or("")));
            self.document.addSubview(&field);
            Some(field)
        } else {
            None
        };

        let body_h = body_height(&post.platform);
        let body_host = NSScrollView::initWithFrame(
            NSScrollView::alloc(self.mtm),
            NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(100.0, body_h)),
        );
        body_host.setHasVerticalScroller(true);
        let body = NSTextView::initWithFrame(
            NSTextView::alloc(self.mtm),
            NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(100.0, body_h)),
        );
        body.setEditable(true);
        body.setString(&NSString::from_str(&post.content));
        body_host.setDocumentView(Some(&body));
        self.document.addSubview(&body_host);

        let tags = NSTextField::initWithFrame(
            NSTextField::alloc(self.mtm),
            NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(100.0, TAGS_H)),
        );
        tags.setBezeled(true);
        tags.setEditable(true);
        tags.setPlaceholderString(Some(&NSString::from_str("tags (space-separated)")));
        tags.setStringValue(&NSString::from_str(&post.tags.join(" ")));
        self.document.addSubview(&tags);

        Field {
            video_id: item.video_id.clone(),
            video_type: item.video_type.clone(),
            platform: post.platform.clone(),
            heading,
            title,
            body,
            body_host,
            tags,
        }
    }

    fn clear(&self) {
        for field in self.fields.borrow_mut().drain(..) {
            field.heading.removeFromSuperview();
            if let Some(title) = field.title {
                title.removeFromSuperview();
            }
            field.body_host.removeFromSuperview();
            field.tags.removeFromSuperview();
        }
    }

    pub fn relayout(&self) {
        let width = self.scroll.contentView().bounds().size.width.max(80.0);
        let inner = (width - PAD * 2.0).max(60.0);
        let fields = self.fields.borrow();
        let mut needed = PAD;
        if fields.is_empty() {
            needed += 40.0 + PAD;
        } else {
            for field in fields.iter() {
                needed += LABEL_H + 4.0;
                if field.title.is_some() {
                    needed += TITLE_H + 4.0;
                }
                needed += body_height(&field.platform) + 4.0 + TAGS_H + SECTION_GAP;
            }
        }
        let height = needed.max(self.scroll.contentView().bounds().size.height);
        self.document.setFrame(NSRect::new(
            NSPoint::new(0.0, 0.0),
            NSSize::new(width, height),
        ));

        if fields.is_empty() {
            self.placeholder.setHidden(false);
            self.placeholder.setFrame(NSRect::new(
                NSPoint::new(PAD, height - PAD - 40.0),
                NSSize::new(inner, 40.0),
            ));
            return;
        }

        self.placeholder.setHidden(true);
        let mut cursor = height - PAD;
        for field in fields.iter() {
            cursor -= LABEL_H;
            field.heading.setFrame(NSRect::new(
                NSPoint::new(PAD, cursor),
                NSSize::new(inner, LABEL_H),
            ));
            cursor -= 4.0;
            if let Some(title) = field.title.as_ref() {
                cursor -= TITLE_H;
                title.setFrame(NSRect::new(
                    NSPoint::new(PAD, cursor),
                    NSSize::new(inner, TITLE_H),
                ));
                cursor -= 4.0;
            }
            let body_h = body_height(&field.platform);
            cursor -= body_h;
            field.body_host.setFrame(NSRect::new(
                NSPoint::new(PAD, cursor),
                NSSize::new(inner, body_h),
            ));
            field.body.setFrame(NSRect::new(
                NSPoint::new(0.0, 0.0),
                NSSize::new(inner, body_h),
            ));
            cursor -= 4.0 + TAGS_H;
            field.tags.setFrame(NSRect::new(
                NSPoint::new(PAD, cursor),
                NSSize::new(inner, TAGS_H),
            ));
            cursor -= SECTION_GAP;
        }
    }
}

fn body_height(platform: &str) -> f64 {
    match platform {
        "twitter" | "bluesky" | "tiktok" => 72.0,
        "youtube" | "linkedin" | "facebook" => 140.0,
        _ => 100.0,
    }
}
