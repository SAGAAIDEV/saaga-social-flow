//! The app's one menu: Edit.
//!
//! macOS delivers ⌘C, ⌘V, ⌘X, ⌘A and ⌘Z to a text box through the main
//! menu's key equivalents — a text field or a web view does not see the
//! keystroke itself. The event loop is built with winit's default menu off
//! (`app::startup`), so for a long time there was no menu at all and copy and
//! paste did nothing in any box: the YouTube page, the plan, the settings,
//! the native fields.
//!
//! This installs Edit and nothing else. winit's default menu would also bring
//! Quit (⌘Q) and Hide (⌘H), one keystroke from ending or hiding a take in
//! progress — the app has its own ⌃⌥Q, deliberately harder to hit. Every item
//! has no target, so it goes to whatever has the focus: the web view's text
//! box, a native field, the upload dialog's boxes.
//!
//! The app runs as an accessory — no Dock icon, no visible menu bar — which
//! does not stop the key equivalents working while its window has focus.

use objc2::rc::Retained;
use objc2::runtime::Sel;
use objc2::{sel, MainThreadMarker, MainThreadOnly};
use objc2_app_kit::{NSApplication, NSEventModifierFlags, NSMenu, NSMenuItem};
use objc2_foundation::NSString;

/// The Edit menu's items: title, action, key, and whether ⇧ is part of it.
pub const EDIT_ITEMS: [(&str, &str, &str, bool); 6] = [
    ("Undo", "undo:", "z", false),
    ("Redo", "redo:", "z", true),
    ("Cut", "cut:", "x", false),
    ("Copy", "copy:", "c", false),
    ("Paste", "paste:", "v", false),
    ("Select All", "selectAll:", "a", false),
];

fn action(name: &str) -> Sel {
    match name {
        "undo:" => sel!(undo:),
        "redo:" => sel!(redo:),
        "cut:" => sel!(cut:),
        "copy:" => sel!(copy:),
        "paste:" => sel!(paste:),
        _ => sel!(selectAll:),
    }
}

fn item(
    mtm: MainThreadMarker,
    title: &str,
    action: Option<Sel>,
    key: &str,
) -> Retained<NSMenuItem> {
    unsafe {
        NSMenuItem::initWithTitle_action_keyEquivalent(
            NSMenuItem::alloc(mtm),
            &NSString::from_str(title),
            action,
            &NSString::from_str(key),
        )
    }
}

/// Install the Edit menu as the app's main menu. Call once, after the event
/// loop exists — that is what creates `NSApplication`.
pub fn install(mtm: MainThreadMarker) {
    let main = NSMenu::new(mtm);
    // The first item of a main menu is always the application menu; AppKit
    // treats it specially whatever it holds. Left empty: no Quit, no Hide.
    let app_item = item(mtm, "", None, "");
    app_item.setSubmenu(Some(&NSMenu::new(mtm)));
    main.addItem(&app_item);

    let edit = NSMenu::initWithTitle(NSMenu::alloc(mtm), &NSString::from_str("Edit"));
    for (title, name, key, shift) in EDIT_ITEMS {
        let entry = item(mtm, title, Some(action(name)), key);
        if shift {
            entry.setKeyEquivalentModifierMask(
                NSEventModifierFlags::Command | NSEventModifierFlags::Shift,
            );
        }
        edit.addItem(&entry);
    }
    let edit_item = item(mtm, "Edit", None, "");
    edit_item.setSubmenu(Some(&edit));
    main.addItem(&edit_item);

    NSApplication::sharedApplication(mtm).setMainMenu(Some(&main));
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Copy and paste are there, and nothing that ends or hides a take is.
    #[test]
    fn the_menu_is_edit_only_with_the_usual_keys() {
        let keys: Vec<(&str, &str)> = EDIT_ITEMS.iter().map(|(_, a, k, _)| (*a, *k)).collect();
        for want in [
            ("copy:", "c"),
            ("paste:", "v"),
            ("cut:", "x"),
            ("selectAll:", "a"),
            ("undo:", "z"),
        ] {
            assert!(keys.contains(&want), "{want:?}");
        }
        assert!(EDIT_ITEMS.iter().all(|(_, _, k, _)| *k != "q" && *k != "h"));
        assert!(EDIT_ITEMS
            .iter()
            .any(|(t, _, k, shift)| *t == "Redo" && *k == "z" && *shift));
    }

    #[test]
    fn every_item_names_a_standard_action() {
        for (_, name, _, _) in EDIT_ITEMS {
            assert_eq!(action(name).name().to_str().unwrap(), name);
        }
    }
}
