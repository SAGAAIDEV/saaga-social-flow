//! Primary workflow order, independent of the native controls in each pane.
use objc2::{AnyThread, MainThreadMarker, MainThreadOnly};
use objc2_app_kit::{NSTabView, NSTabViewItem, NSTabViewType};
use objc2_foundation::{NSRect, NSString};

/// Seven steps. Project and Plan come before recording: the project is picked
/// and named first, and a video is planned — hook, chapters, call to action —
/// before a camera turns on. Both used to live on the recording tab, which
/// held the project picker and built its speaking notes backwards from a
/// rehearsal. See `docs/plan-tab-plan.md`.
///
/// Thumbnail sits between recording and YouTube. The render drafts the
/// artwork set, and nothing is uploaded until it has been looked at and
/// approved here — it was folded into the recording page once, and then
/// nothing stood between the draw and the upload. See
/// `docs/thumbnail-tab-plan.md`.
pub const STEPS: [(&str, &str, &[&str]); 7] = [
    ("project", "Project", &["project"]),
    ("plan", "Plan", &["plan"]),
    ("video", "Video recording", &["draft"]),
    ("thumbnail", "Thumbnail", &["thumbnail"]),
    ("youtube", "YouTube", &["youtube"]),
    ("blog", "Blog (Strapi)", &["blog"]),
    (
        "socials",
        "Socials",
        &["post", "schedule", "analytics", "reflect"],
    ),
];

/// The pane the window opens on. Recording, not the first step: the app
/// resumes the last project at launch, and what someone opening it wants to
/// see first is the camera preview and the record buttons. Picking another
/// project or planning is one tab to the left.
pub const OPENS_ON: &str = "draft";

pub fn attach(
    root: &NSTabView,
    bounds: NSRect,
    mtm: MainThreadMarker,
    panes: &[(&str, &NSTabViewItem)],
) {
    for (id, label, children) in STEPS {
        let pane = |key: &str| {
            panes
                .iter()
                .find(|(name, _)| *name == key)
                .expect("workflow pane exists")
                .1
        };
        if children.len() == 1 {
            let item = pane(children[0]);
            item.setLabel(&NSString::from_str(label));
            root.addTabViewItem(item);
        } else {
            let tabs = NSTabView::initWithFrame(NSTabView::alloc(mtm), bounds);
            super::fill_parent(&tabs);
            tabs.setTabViewType(NSTabViewType::TopTabsBezelBorder);
            for child in children {
                tabs.addTabViewItem(pane(child));
            }
            let item = unsafe {
                NSTabViewItem::initWithIdentifier(
                    NSTabViewItem::alloc(),
                    Some(&NSString::from_str(id)),
                )
            };
            item.setLabel(&NSString::from_str(label));
            item.setView(Some(&tabs));
            root.addTabViewItem(&item);
        }
    }
    // A single-child step is added as the pane's own item, so the pane's key is
    // the identifier the root tab view knows it by.
    unsafe { root.selectTabViewItemWithIdentifier(&NSString::from_str(OPENS_ON)) };
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn the_workflow_has_seven_ordered_steps_and_each_pane_once() {
        assert_eq!(
            STEPS.map(|(id, _, _)| id),
            [
                "project",
                "plan",
                "video",
                "thumbnail",
                "youtube",
                "blog",
                "socials"
            ]
        );
        let children: Vec<_> = STEPS
            .iter()
            .flat_map(|(_, _, children)| children.iter().copied())
            .collect();
        assert_eq!(children.len(), 10);
        // Planned before it is recorded: the plan is per project and outlives
        // versions, and versioning stays with recording.
        let at = |pane: &str| children.iter().position(|c| *c == pane).unwrap();
        assert!(at("project") < at("plan") && at("plan") < at("draft"));
        // Reviewed after it is recorded and before anything goes public.
        assert!(at("draft") < at("thumbnail") && at("thumbnail") < at("youtube"));
        // The launch tab must be a step of its own, or it has no identifier
        // on the root tab view and the window opens on Project instead.
        assert!(STEPS.iter().any(|(_, _, children)| *children == [OPENS_ON]));
        assert!(!children.contains(&"render"));
        // The S3 upload runs off the render now — see `App::host_after_render`.
        assert!(!children.contains(&"distribute"));
        let unique: std::collections::HashSet<_> = children.iter().collect();
        assert_eq!(unique.len(), children.len());
        assert!(!children.contains(&"edit"));
        assert!(!children.contains(&"substack"));
        // Folded into the recording page rather than a tab of its own.
        assert!(!children.contains(&"review"));
    }
}
