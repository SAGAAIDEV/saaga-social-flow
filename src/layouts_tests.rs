//! Tests for the 3x2 layout table.
//!
//! Most of these re-read the real composition CSS under `screencast/` and fail
//! when a re-export moves a slot or flips a paint order — the table is a
//! transcription, and this is what stops it silently becoming a stale one.
//! They skip with a printed note when that sibling project is absent, so this
//! crate still builds standalone.

use super::*;
use std::path::PathBuf;

#[test]
fn layouts_are_indexed_pair_major() {
    for pair in Pair::ALL {
        for orientation in Orientation::ALL {
            let layout = Layout::get(pair, orientation);
            assert_eq!(layout.pair, pair);
            assert_eq!(layout.orientation, orientation);
        }
    }
}

#[test]
fn only_the_split_pair_carries_a_screen() {
    for orientation in Orientation::ALL {
        assert!(
            !Layout::get(Pair::TalkingHead, orientation).needs_screen(),
            "a talking head chapter must not open a screen stream",
        );
        assert!(
            !Layout::get(Pair::Outline, orientation).needs_screen(),
            "an outline chapter's screen space is drawn at render, not captured",
        );
        assert!(Layout::get(Pair::Split, orientation).needs_screen());
    }
}

/// The outline pair records exactly what the talking head records: the chapter
/// opens and closes full frame, and the card that pushes the camera into the
/// split's column exists only at render. A camera slot here that was anything
/// but the whole canvas would be a master the composition could not open on.
#[test]
fn the_outline_pair_records_exactly_like_the_talking_head() {
    for orientation in Orientation::ALL {
        let talking = Layout::get(Pair::TalkingHead, orientation);
        let outline = Layout::get(Pair::Outline, orientation);
        assert_eq!(outline.camera_slot, talking.camera_slot, "{orientation:?}");
        assert_eq!(outline.canvas, talking.canvas, "{orientation:?}");
        assert_eq!(outline.screen_slot, None);
        assert_eq!(outline.topmost, Topmost::Camera);
        assert!(outline.parent().is_none(), "no screen region to inherit");
        assert!(!outline.composites(), "covered, like the talking head");
    }
}

/// What the graph builder keys on: only a layout with a screen has to place
/// the camera today; a camera-only layout covers its canvas.
#[test]
fn only_layouts_that_place_the_camera_composite() {
    for layout in &LAYOUTS {
        let expected = layout.pair == Pair::Split;
        assert_eq!(
            layout.composites(),
            expected,
            "{} composites={} expected {expected}",
            layout.block,
            layout.composites()
        );
    }
}

/// The record the render reads to tell an outline chapter from a passthrough
/// one. Absent for an older take, and absent reads as "not outline".
#[test]
fn a_chapters_layout_round_trips_beside_its_media() {
    let dir = std::env::temp_dir().join(format!("layouts-record-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    assert_eq!(chapter_pair(&dir, 3), None, "nothing recorded yet");
    record_chapter_layout(&dir, 3, Pair::Outline).unwrap();
    assert_eq!(chapter_pair(&dir, 3), Some(Pair::Outline));
    assert_eq!(chapter_pair(&dir, 4), None, "another chapter is unaffected");
    let text = std::fs::read_to_string(layout_path(&dir, 3)).unwrap();
    assert!(text.contains(r#""pair": "outline""#), "{text}");
    assert!(text.contains("outline-horizontal"), "{text}");
    assert!(text.contains("outline-vertical"), "{text}");
    // A pair with a hyphenated name is spelled the way `--layout` spells it.
    record_chapter_layout(&dir, 5, Pair::TalkingHead).unwrap();
    assert!(std::fs::read_to_string(layout_path(&dir, 5))
        .unwrap()
        .contains(r#""pair": "talking-head""#));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn the_two_split_slots_have_the_aspects_the_regions_lock_to() {
    let (w, h) = Layout::get(Pair::Split, Orientation::Horizontal)
        .slot_size()
        .expect("split-horizontal has a screen");
    assert!((w / h - 1.2987).abs() < 1e-3, "split-h aspect drifted");

    let (w, h) = Layout::get(Pair::Split, Orientation::Vertical)
        .slot_size()
        .expect("split-vertical has a screen");
    assert!((w / h - 0.84375).abs() < 1e-6, "split-v aspect drifted");
}

#[test]
fn only_split_vertical_is_parented_and_the_link_terminates() {
    let child = Layout::get(Pair::Split, Orientation::Vertical);
    let parent = child.parent().expect("split-vertical follows horizontal");
    assert_eq!(parent.block, "screen-camera-split");
    assert!(
        parent.parent().is_none(),
        "the parent must be a root, or resolving needs a topological sort",
    );
    for layout in &LAYOUTS {
        if layout.block != child.block {
            assert!(
                layout.parent().is_none(),
                "{} grew a parent; resolution order assumes exactly one link",
                layout.block,
            );
        }
    }
}

#[test]
fn a_parent_always_has_a_screen_slot_of_its_own() {
    for layout in &LAYOUTS {
        if let Some(parent) = layout.parent() {
            assert!(
                parent.needs_screen(),
                "{} is parented to a layout with no region to inherit from",
                layout.block,
            );
        }
    }
}

#[test]
fn every_block_id_resolves_and_is_unique() {
    let ids = Layout::block_ids();
    for layout in &LAYOUTS {
        assert_eq!(
            Layout::from_block(layout.block).map(|l| l.block),
            Some(layout.block),
        );
    }
    let mut unique = ids.to_vec();
    unique.sort_unstable();
    unique.dedup();
    assert_eq!(unique.len(), ids.len(), "two cells share a block id");
    assert_eq!(Layout::from_block("screen-camera-circle"), None);
}

#[test]
fn every_slot_fits_inside_its_canvas() {
    for layout in &LAYOUTS {
        let mut slots = vec![("camera", layout.camera_slot)];
        slots.extend(layout.screen_slot.map(|s| ("screen", s)));
        for (which, (x, y, w, h)) in slots {
            assert!(
                x >= 0.0 && y >= 0.0 && x + w <= layout.canvas.0 && y + h <= layout.canvas.1,
                "{}'s {which} slot escapes its canvas",
                layout.block,
            );
        }
    }
}

/// The overlap that makes [`Topmost`] matter at all. Split-Horizontal's two
/// slots deliberately share 4.5px; every other layout's are disjoint, and a
/// fixture where none overlapped would make the paint-order test vacuous.
#[test]
fn only_split_horizontal_overlaps_its_two_slots() {
    for layout in &LAYOUTS {
        let Some(screen) = layout.screen_slot else {
            continue;
        };
        let (cx, cy, cw, ch) = layout.camera_slot;
        let overlaps = cx < screen.0 + screen.2
            && screen.0 < cx + cw
            && cy < screen.1 + screen.3
            && screen.1 < cy + ch;
        let expected = layout.block == "screen-camera-split";
        assert_eq!(
            overlaps, expected,
            "{} overlap={overlaps}, expected {expected}",
            layout.block,
        );
    }
}

/// The camera aspects a composite has to fit a 16:9 camera into. Pinned
/// because `region::cover`'s tests hardcode the same numbers, and two
/// copies of a constant is how they drift apart.
#[test]
fn the_camera_slots_have_the_aspects_cover_is_tested_against() {
    let aspect = |layout: &Layout| {
        let (w, h) = layout.camera_slot_size();
        w / h
    };
    let split_h = Layout::get(Pair::Split, Orientation::Horizontal);
    let split_v = Layout::get(Pair::Split, Orientation::Vertical);
    let talk_h = Layout::get(Pair::TalkingHead, Orientation::Horizontal);
    let talk_v = Layout::get(Pair::TalkingHead, Orientation::Vertical);

    assert!(
        (aspect(split_h) - 522.0 / 1080.0).abs() < 1e-9,
        "split column"
    );
    assert!(
        (aspect(split_v) - 1080.0 / 640.0).abs() < 1e-9,
        "vertical panel"
    );
    assert!(
        (aspect(talk_h) - 16.0 / 9.0).abs() < 1e-9,
        "talking-head-horizontal must match a 16:9 camera exactly, or the \
         framing offset stops being inert there",
    );
    assert!(
        (aspect(talk_v) - 9.0 / 16.0).abs() < 1e-9,
        "talking-head-vertical"
    );
    // The outline pair frames its camera exactly as the talking head does.
    assert_eq!(
        Layout::get(Pair::Outline, Orientation::Horizontal).camera_slot_size(),
        talk_h.camera_slot_size()
    );
    assert_eq!(
        Layout::get(Pair::Outline, Orientation::Vertical).camera_slot_size(),
        talk_v.camera_slot_size()
    );
}

/// Where the compositions live — the vendored `components/` in this repo, via
/// the same resolution the renderer uses, so a test cannot pass against a
/// different copy than the one that gets rendered.
///
/// Still an `Option`: an installed release has no crate root, and these are
/// checks on the library rather than on the code, so having nothing to check is
/// a skip rather than a failure.
fn compositions_dir() -> Option<PathBuf> {
    let dir = crate::edit::compose::components_root().join("compositions");
    dir.is_dir().then_some(dir)
}

/// The declaration block of `.<class> { … }`, or `None` if there is no such
/// rule. Comments in these files sit between rules rather than inside them,
/// so stopping at the first `}` is enough — and if that ever stops being
/// true this test fails loudly, which is the right direction to fail in.
fn css_rule<'a>(style: &'a str, class: &str) -> Option<&'a str> {
    let needle = format!(".{class} {{");
    let start = style.find(&needle)? + needle.len();
    let end = style[start..].find('}')? + start;
    Some(&style[start..end])
}

/// One `px` (or bare-zero) declaration out of a rule body.
fn css_px(rule: &str, property: &str) -> Option<f64> {
    for declaration in rule.split(';') {
        let Some((name, value)) = declaration.split_once(':') else {
            continue;
        };
        if name.trim() != property {
            continue;
        }
        let value = value.trim().trim_end_matches("px");
        return value.parse().ok();
    }
    None
}

/// Read the block's canvas and its screen hole straight out of the
/// composition, the same way a human would: find the `<video>` bound to
/// `screenSrc`, take its first class, and read that class's rect.
fn slot_from_html(html: &str) -> (Option<(f64, f64)>, Option<(f64, f64, f64, f64)>) {
    let canvas = html.find("data-width=\"").and_then(|i| {
        let rest = &html[i + "data-width=\"".len()..];
        let (w, rest) = rest.split_once('"')?;
        let j = rest.find("data-height=\"")? + "data-height=\"".len();
        let (h, _) = rest[j..].split_once('"')?;
        Some((w.parse().ok()?, h.parse().ok()?))
    });

    let Some(line) = html
        .lines()
        .find(|line| line.contains("data-var-src=\"screenSrc\""))
    else {
        return (canvas, None);
    };
    let after =
        &line[line.find("class=\"").expect("the screen video has a class") + "class=\"".len()..];
    let class = after
        .split([' ', '"'])
        .next()
        .expect("the class attribute is not empty");

    let style = &html[html
        .find("<style>")
        .expect("a composition has a style block")
        ..html.find("</style>").expect("the style block is closed")];
    let rule = css_rule(style, class).unwrap_or_else(|| panic!("no CSS rule for .{class}"));
    // Both Split layouts anchor their screen at the origin with explicit
    // left/top; treating a missing one as 0 also covers an `inset: 0` rule
    // without needing to parse the shorthand.
    let slot = (
        css_px(rule, "left").unwrap_or(0.0),
        css_px(rule, "top").unwrap_or(0.0),
        css_px(rule, "width").unwrap_or_else(|| panic!(".{class} declares no width")),
        css_px(rule, "height").unwrap_or_else(|| panic!(".{class} declares no height")),
    );
    (canvas, Some(slot))
}

/// Which CSS class actually carries each layout's camera rect.
///
/// Test-only knowledge, and asymmetric in a way the screen side is not.
/// The talking heads position the `<video>` element itself; Split-Horizontal
/// positions a *wrapper* around it and leaves the video at `100%`; and
/// Split-Vertical positions the peach *panel*, with the camera `inset: 0`
/// inside it. Following the `cameraSrc` element the way
/// `slot_from_html` follows `screenSrc` would therefore read `100%` for two
/// of the four, so the class is named here instead.
fn camera_rect_class(block: &str) -> &'static str {
    match block {
        "talking-head-horizontal" => "thh-camera",
        "talking-head-vertical" => "th-camera",
        "screen-camera-split" => "screen-camera-split-camera-wrap",
        "screen-camera-vertical" => "vert-demo-panel",
        "outline-horizontal" => "olh-camera-wrap",
        "outline-vertical" => "olv-camera-wrap",
        other => panic!("no camera class recorded for {other}"),
    }
}

/// The rect one named class declares, defaulting a missing `left`/`top` to
/// zero the same way an `inset: 0` rule means.
fn rect_of_class(html: &str, class: &str) -> (f64, f64, f64, f64) {
    let style = &html[html
        .find("<style>")
        .expect("a composition has a style block")
        ..html.find("</style>").expect("the style block is closed")];
    let rule = css_rule(style, class).unwrap_or_else(|| panic!("no CSS rule for .{class}"));
    (
        css_px(rule, "left").unwrap_or(0.0),
        css_px(rule, "top").unwrap_or(0.0),
        css_px(rule, "width").unwrap_or_else(|| panic!(".{class} declares no width")),
        css_px(rule, "height").unwrap_or_else(|| panic!(".{class} declares no height")),
    )
}

/// The camera half of the drift check. Same contract as the screen one: a
/// re-export that nudges a slot becomes a red test rather than a composite
/// that draws the camera in the wrong place.
#[test]
fn camera_slots_match_the_composition_css() {
    let Some(dir) = compositions_dir() else {
        println!(
            "skipping: no components/ library resolved, so there is no composition CSS \
             to check the camera slots against"
        );
        return;
    };

    for layout in &LAYOUTS {
        let path = dir.join(format!("{}.html", layout.block));
        let html = std::fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("reading {}: {e}", path.display()));
        let css = rect_of_class(&html, camera_rect_class(layout.block));
        let table = layout.camera_slot;
        let close = |a: f64, b: f64| (a - b).abs() < 1e-3;
        assert!(
            close(table.0, css.0)
                && close(table.1, css.1)
                && close(table.2, css.2)
                && close(table.3, css.3),
            "{}'s camera slot moved: table says {table:?}, the CSS says {css:?}",
            layout.block,
        );
    }
}

/// Paint order is read from the same CSS, because it is as easy to get
/// wrong as the rects and the two Split layouts disagree about it.
#[test]
fn paint_order_matches_the_composition_css() {
    let Some(dir) = compositions_dir() else {
        println!("skipping: no components/ library resolved");
        return;
    };

    for layout in &LAYOUTS {
        let Some(_) = layout.screen_slot else {
            // Camera-only: nothing to be on top of.
            assert_eq!(layout.topmost, Topmost::Camera, "{}", layout.block);
            continue;
        };
        let html = std::fs::read_to_string(dir.join(format!("{}.html", layout.block)))
            .expect("composition reads");
        let style = &html[html.find("<style>").unwrap()..html.find("</style>").unwrap()];
        let z = |class: &str| -> f64 {
            let rule = css_rule(style, class).unwrap_or_else(|| panic!(".{class}"));
            css_px(rule, "z-index").unwrap_or(0.0)
        };
        let camera_z = z(camera_rect_class(layout.block));
        let screen_class = screen_rect_class(&html);
        let screen_z = z(&screen_class);
        let want = if screen_z > camera_z {
            Topmost::Screen
        } else {
            Topmost::Camera
        };
        assert_eq!(
            layout.topmost, want,
            "{}: camera z={camera_z}, screen z={screen_z}",
            layout.block,
        );
    }
}

/// The class the screen video carries, for the paint-order check.
fn screen_rect_class(html: &str) -> String {
    let line = html
        .lines()
        .find(|line| line.contains("data-var-src=\"screenSrc\""))
        .expect("a screen-bearing layout has a screenSrc element");
    let after = &line[line.find("class=\"").expect("it has a class") + "class=\"".len()..];
    after
        .split([' ', '"'])
        .next()
        .expect("the class attribute is not empty")
        .to_string()
}

/// The table above is a transcription. This is what notices when the source
/// of that transcription moves — a re-export from Figma that nudges the
/// split by a few pixels would otherwise ship as a silently mis-cropped
/// recording rather than as a failing test.
#[test]
fn slots_match_the_composition_css() {
    let Some(dir) = compositions_dir() else {
        println!(
            "skipping: no components/ library resolved, so there is no composition CSS \
             to check the layout table against"
        );
        return;
    };

    for layout in &LAYOUTS {
        let path = dir.join(format!("{}.html", layout.block));
        let html = std::fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("reading {}: {e}", path.display()));
        let (canvas, slot) = slot_from_html(&html);

        assert_eq!(
            canvas,
            Some(layout.canvas),
            "{}'s canvas moved in the composition",
            layout.block,
        );

        match (layout.screen_slot, slot) {
            (Some(table), Some(css)) => {
                let close = |a: f64, b: f64| (a - b).abs() < 1e-3;
                assert!(
                    close(table.0, css.0)
                        && close(table.1, css.1)
                        && close(table.2, css.2)
                        && close(table.3, css.3),
                    "{}'s screen slot moved: table says {table:?}, the CSS says {css:?}",
                    layout.block,
                );
            }
            (None, None) => {}
            (Some(_), None) => panic!(
                "{} lost its screen element — the layout table still expects one",
                layout.block,
            ),
            (None, Some(css)) => panic!(
                "{} grew a screen element at {css:?} — the layout table says it has none, \
                 so recording it would skip screen capture entirely",
                layout.block,
            ),
        }
    }
}
