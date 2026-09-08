//! One approve switch per planned post.
//!
//! The Schedule tab is the only gate between generated copy and a live public post,
//! so every row shows what would go out — video, platform, the channel it lands on —
//! and a blocked row shows why instead of a switch it cannot honour.
//!
//! Ticks are read back by identity (video, platform, copy hash), never by row index:
//! a plan rebuilt while the form is on screen must not be able to move a tick from
//! the caption it was given to a different one.

use std::cell::RefCell;

use objc2::rc::Retained;
use objc2::{MainThreadMarker, MainThreadOnly};
use objc2_app_kit::{
    NSAutoresizingMaskOptions, NSButton, NSButtonType, NSScrollView, NSTextField, NSView,
};
use objc2_foundation::{NSPoint, NSRect, NSSize, NSString};

use super::schema::{PlanItem, SchedulePlan};

const PAD: f64 = 10.0;
const ROW_H: f64 = 24.0;
const GAP: f64 = 4.0;

struct Row {
    video_id: String,
    platform: String,
    copy_hash: String,
    /// This item cannot be sent at all, so it can never be approved.
    blocked: bool,
    approve: Retained<NSButton>,
}

pub struct ScheduleForm {
    scroll: Retained<NSScrollView>,
    document: Retained<NSView>,
    placeholder: Retained<NSTextField>,
    rows: RefCell<Vec<Row>>,
    mtm: MainThreadMarker,
}

impl ScheduleForm {
    pub fn attach(parent: &NSView, mtm: MainThreadMarker) -> ScheduleForm {
        let scroll = NSScrollView::initWithFrame(
            NSScrollView::alloc(mtm),
            NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(100.0, 100.0)),
        );
        scroll.setHasVerticalScroller(true);
        let document = NSView::initWithFrame(
            NSView::alloc(mtm),
            NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(100.0, 100.0)),
        );
        scroll.setDocumentView(Some(&document));
        parent.addSubview(&scroll);
        let placeholder = NSTextField::labelWithString(
            &NSString::from_str("Build a plan, then approve the posts to queue."),
            mtm,
        );
        document.addSubview(&placeholder);
        ScheduleForm {
            scroll,
            document,
            placeholder,
            rows: RefCell::new(Vec::new()),
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

    /// Renders the plan, keeping any tick already on screen for a row whose identity
    /// is unchanged.
    ///
    /// The summary refreshes on unrelated events (a distribute finishing, a version
    /// switch), and a tick lives only in the checkbox until Queue writes it. Rebuilding
    /// blindly from disk would silently clear review work mid-session, so on-screen
    /// state wins for a row that is still the same video, platform and copy.
    pub fn show(&self, plan: &SchedulePlan) {
        let carried: Vec<(String, String, String, bool)> = self
            .rows
            .borrow()
            .iter()
            .map(|row| {
                (
                    row.video_id.clone(),
                    row.platform.clone(),
                    row.copy_hash.clone(),
                    row.approve.state() != 0,
                )
            })
            .collect();
        self.clear();
        let mut rows = Vec::new();
        for item in &plan.items {
            let on_screen = carried
                .iter()
                .find(|(video, platform, hash, _)| {
                    *video == item.video_id && *platform == item.platform && *hash == item.copy_hash
                })
                .map(|(_, _, _, ticked)| *ticked);
            rows.push(self.add_row(item, on_screen.unwrap_or(item.approved)));
        }
        *self.rows.borrow_mut() = rows;
        self.relayout();
    }

    pub fn show_empty(&self) {
        self.clear();
        self.relayout();
    }

    /// Copies the on-screen ticks back onto `plan`, matched by identity. A row whose
    /// item is no longer in the plan is dropped rather than applied to a neighbour.
    pub fn apply(&self, plan: &mut SchedulePlan) {
        let rows = self.rows.borrow();
        for item in &mut plan.items {
            let Some(row) = rows.iter().find(|row| {
                row.video_id == item.video_id
                    && row.platform == item.platform
                    && row.copy_hash == item.copy_hash
            }) else {
                continue;
            };
            // A blocked item can never be sent, so it can never be approved.
            item.approved = item.skip.is_none() && row.approve.state() != 0;
        }
    }

    /// Ticks every row that can actually be sent, returning how many.
    ///
    /// Blocked rows stay off — there is nothing to approve about a post with no
    /// channel or no uploaded video, and [`ScheduleForm::apply`] would clear them
    /// again anyway.
    pub fn approve_all(&self) -> usize {
        self.rows
            .borrow()
            .iter()
            .filter(|row| !row.blocked)
            .map(|row| row.approve.setState(1))
            .count()
    }

    fn add_row(&self, item: &PlanItem, ticked: bool) -> Row {
        let approve = unsafe {
            NSButton::buttonWithTitle_target_action(
                &NSString::from_str(&row_title(item)),
                None,
                None,
                self.mtm,
            )
        };
        approve.setButtonType(NSButtonType::Switch);
        approve.setState(isize::from(ticked && item.skip.is_none()));
        // A blocked row keeps its reason visible but cannot be armed.
        approve.setEnabled(item.skip.is_none());
        self.document.addSubview(&approve);

        Row {
            video_id: item.video_id.clone(),
            platform: item.platform.clone(),
            copy_hash: item.copy_hash.clone(),
            blocked: item.skip.is_some(),
            approve,
        }
    }

    fn clear(&self) {
        for row in self.rows.borrow_mut().drain(..) {
            row.approve.removeFromSuperview();
        }
    }

    fn relayout(&self) {
        let width = self.scroll.contentView().bounds().size.width.max(80.0);
        let inner = (width - PAD * 2.0).max(60.0);
        let rows = self.rows.borrow();
        let needed = if rows.is_empty() {
            PAD * 2.0 + 40.0
        } else {
            PAD + rows.len() as f64 * (ROW_H + GAP) + PAD
        };
        let height = needed.max(self.scroll.contentView().bounds().size.height);
        self.document.setFrame(NSRect::new(
            NSPoint::new(0.0, 0.0),
            NSSize::new(width, height),
        ));
        if rows.is_empty() {
            self.placeholder.setHidden(false);
            self.placeholder.setFrame(NSRect::new(
                NSPoint::new(PAD, height - PAD - 40.0),
                NSSize::new(inner, 40.0),
            ));
            return;
        }
        self.placeholder.setHidden(true);
        let mut y = height - PAD;
        for row in rows.iter() {
            y -= ROW_H;
            row.approve
                .setFrame(NSRect::new(NSPoint::new(PAD, y), NSSize::new(inner, ROW_H)));
            y -= GAP;
        }
    }
}

/// What one row says. A blocked item leads with the reason it cannot be sent,
/// because that is the only thing about it worth reading. A sendable one carries
/// the opening words of its caption, so the switch is not approving an unread post —
/// the full text sits in the pane below.
fn row_title(item: &PlanItem) -> String {
    let target = if item.channel_name.is_empty() {
        item.platform.clone()
    } else {
        format!("{} → {}", item.platform, item.channel_name)
    };
    match &item.skip {
        Some(why) => format!("{} · {target} — SKIPPED: {why}", item.video_id),
        None => format!("{} · {target} — {}", item.video_id, snippet(&item.text)),
    }
}

/// The caption's opening words on one line, for a row label.
fn snippet(text: &str) -> String {
    const MAX: usize = 72;
    let flat = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.chars().count() <= MAX {
        return flat;
    }
    // Cut on a word boundary so a row never ends mid-word.
    let cut: String = flat.chars().take(MAX).collect();
    let trimmed = match cut.rsplit_once(' ') {
        Some((head, _)) if !head.is_empty() => head.to_string(),
        _ => cut,
    };
    format!("{trimmed}…")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(video_id: &str, platform: &str, channel: &str, skip: Option<&str>) -> PlanItem {
        PlanItem {
            video_id: video_id.into(),
            platform: platform.into(),
            channel_id: "id".into(),
            channel_name: channel.into(),
            url: "https://example.com/v.mp4".into(),
            text: "copy".into(),
            title: None,
            mode: "addToQueue".into(),
            scheduling_type: "automatic".into(),
            needs_approval: false,
            image: false,
            metadata: None,
            reason: "hub video".into(),
            prompt_id: "posts.social".into(),
            prompt_version: Some(1),
            copy_hash: "0badc0de0badc0de".into(),
            skip: skip.map(str::to_string),
            approved: false,
        }
    }

    #[test]
    fn a_ready_row_names_the_video_platform_channel_and_copy() {
        assert_eq!(
            row_title(&item("chapter-01", "instagram", "saagasocials", None)),
            "chapter-01 · instagram → saagasocials — copy"
        );
    }

    /// A caption is multi-line with tags; a row label is one line.
    #[test]
    fn the_snippet_flattens_and_cuts_on_a_word_boundary() {
        assert_eq!(snippet("hello\n\nthere  #rust"), "hello there #rust");
        let long = "the quick brown fox jumps over the lazy dog and keeps on running \
                    well past the limit";
        let cut = snippet(long);
        assert!(cut.ends_with('…'));
        assert!(cut.chars().count() <= 73, "72 chars plus the ellipsis");
        assert!(!cut.contains("  "));
        assert!(
            long.starts_with(cut.trim_end_matches('…')),
            "the snippet is a prefix of the caption"
        );
        // No word is chopped in half.
        let last = cut.trim_end_matches('…').split(' ').next_back().unwrap();
        assert!(long.split(' ').any(|word| word == last));
    }

    #[test]
    fn a_short_caption_is_shown_whole() {
        assert_eq!(snippet("ship it"), "ship it");
        assert!(!snippet("ship it").ends_with('…'));
    }

    /// Multi-byte copy must not panic on the cut.
    #[test]
    fn the_snippet_handles_wide_characters() {
        let emoji = "🚀".repeat(120);
        let cut = snippet(&emoji);
        assert!(cut.ends_with('…'));
        assert!(cut.chars().count() <= 73);
    }

    #[test]
    fn a_blocked_row_leads_with_the_reason() {
        let title = row_title(&item(
            "chapter-02",
            "facebook",
            "",
            Some("no facebook channel connected to Buffer"),
        ));
        assert_eq!(
            title,
            "chapter-02 · facebook — SKIPPED: no facebook channel connected to Buffer"
        );
        assert!(title.contains("SKIPPED"));
    }

    #[test]
    fn an_unresolved_channel_still_names_the_platform() {
        assert_eq!(
            row_title(&item("longform", "twitter", "", None)),
            "longform · twitter — copy"
        );
    }
}
