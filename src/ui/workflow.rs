//! Primary workflow order, independent of the native controls in each pane.
use objc2::{AnyThread, MainThreadMarker, MainThreadOnly};
use objc2_app_kit::{NSTabView, NSTabViewItem, NSTabViewType};
use objc2_foundation::{NSRect, NSString};

/// Four steps. Thumbnails used to be one of its own; the render button now
/// draws the artwork set, and the recording page shows it beside the title,
/// the description and the rendered clips — so there is nothing left for a
/// separate tab to hold.
pub const STEPS: [(&str, &str, &[&str]); 4] = [
    ("video", "Video recording", &["draft"]),
    ("youtube", "YouTube", &["youtube"]),
    ("blog", "Blog (Strapi)", &["blog"]),
    (
        "socials",
        "Socials",
        &["post", "distribute", "schedule", "analytics", "reflect"],
    ),
];

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
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn the_workflow_has_four_ordered_steps_and_each_pane_once() {
        assert_eq!(
            STEPS.map(|(id, _, _)| id),
            ["video", "youtube", "blog", "socials"]
        );
        let children: Vec<_> = STEPS
            .iter()
            .flat_map(|(_, _, children)| children.iter().copied())
            .collect();
        assert_eq!(children.len(), 8);
        assert!(!children.contains(&"render"));
        let unique: std::collections::HashSet<_> = children.iter().collect();
        assert_eq!(unique.len(), children.len());
        assert!(!children.contains(&"edit"));
        assert!(!children.contains(&"substack"));
        // Folded into the recording page rather than tabs of their own.
        assert!(!children.contains(&"thumbnails"));
        assert!(!children.contains(&"review"));
    }
}
