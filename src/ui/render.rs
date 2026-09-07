//! Rendering panes from Jinja templates.
//!
//! Templates are embedded with `include_str!`, so the binary carries its own UI
//! and there is nothing to install beside it. The environment is built once and
//! reused; `{% extends %}` and `{% import %}` resolve against the names registered
//! here, which is why every template must be added to [`environment`].
//!
//! Rendering is infallible from the caller's point of view: a template error is a
//! bug in a template, and returning it *as the pane* puts it on screen where it
//! will be noticed, rather than leaving a blank panel and a line in a log.

use minijinja::Environment;
use serde::Serialize;

const BASE: &str = include_str!("templates/base.html");
const COMPONENTS: &str = include_str!("templates/components.html");
const REFLECT: &str = include_str!("templates/reflect.html");
const THUMBNAIL: &str = include_str!("templates/thumbnail.html");
const REVIEW: &str = include_str!("templates/review.html");
const EDIT: &str = include_str!("templates/edit.html");
const SUBSTACK: &str = include_str!("templates/substack.html");
const BLOG: &str = include_str!("templates/blog.html");

/// The waveform editor, carried in the binary rather than fetched.
///
/// A pane is loaded from a `file://` page with no base URL, so a `<script src>` has
/// nothing to resolve against and an ESM build would be refused by CORS. Both of these
/// are UMD, which inlines cleanly — see `ui/vendor/README.md`.
const WAVESURFER_JS: &str = include_str!("vendor/wavesurfer.min.js");
const WAVESURFER_REGIONS_JS: &str = include_str!("vendor/wavesurfer-regions.min.js");

fn environment() -> Environment<'static> {
    let mut env = Environment::new();
    env.add_template("base.html", BASE).expect("base template");
    env.add_template("components.html", COMPONENTS)
        .expect("components template");
    env.add_template("reflect.html", REFLECT)
        .expect("reflect template");
    env.add_template("thumbnail.html", THUMBNAIL)
        .expect("thumbnail template");
    env.add_template("review.html", REVIEW)
        .expect("review template");
    env.add_template("edit.html", EDIT).expect("edit template");
    env.add_template("substack.html", SUBSTACK)
        .expect("substack template");
    env.add_template("blog.html", BLOG).expect("blog template");
    env.add_template("youtube.html", include_str!("templates/youtube.html")).expect("youtube template");
    env.add_template("video-brief.html", include_str!("templates/video-brief.html")).expect("video brief template");
    env
}

/// Renders `name` with `ctx`, or an HTML page describing why it could not.
pub fn page<S: Serialize>(name: &str, ctx: S) -> String {
    let env = environment();
    let template = match env.get_template(name) {
        Ok(template) => template,
        Err(err) => return error_page(&format!("no template {name}: {err}")),
    };
    match template.render(ctx) {
        Ok(html) => html,
        Err(err) => error_page(&format!("{name}: {err:#}")),
    }
}

/// The Edit tab, with its editor bundled in.
///
/// A wrapper rather than two more keys at every call site: the pane's own context comes
/// from `edit::pane`, and which JavaScript draws it is this layer's business. Serialising
/// through a JSON object is what lets the two be merged without the template having to
/// reach through a wrapper struct for every field.
pub fn edit_page(pane: &crate::edit::pane::Pane) -> String {
    let mut ctx = serde_json::to_value(pane).unwrap_or_else(|err| {
        eprintln!("stream-recorder: could not serialize the edit pane: {err}");
        serde_json::json!({ "blocked": format!("Could not read this take: {err}") })
    });
    if let Some(map) = ctx.as_object_mut() {
        map.insert("wavesurfer_js".into(), WAVESURFER_JS.into());
        map.insert("regions_js".into(), WAVESURFER_REGIONS_JS.into());
    }
    page("edit.html", ctx)
}

/// A template failure shown in the pane it replaces.
fn error_page(detail: &str) -> String {
    format!(
        "<!doctype html><html><body style=\"margin:0;background:#0b0d10;color:#ff9b9b;\
         font:13px/1.5 ui-monospace,Menlo,monospace;padding:16px\">\
         <b>template error</b><br><pre style=\"white-space:pre-wrap\">{}</pre></body></html>",
        escape(detail)
    )
}

fn escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

#[cfg(test)]
mod tests {
    use super::*;
    use minijinja::context;

    #[test]
    fn every_embedded_template_compiles() {
        // `environment` panics on a malformed template, so building it is the test.
        let env = environment();
        for name in [
            "base.html",
            "components.html",
            "reflect.html",
            "thumbnail.html",
            "review.html",
            "substack.html",
            "edit.html",
            "blog.html",
            "youtube.html",
            "video-brief.html",
        ] {
            assert!(env.get_template(name).is_ok(), "{name} is registered");
        }
    }

    #[test]
    fn video_brief_preserves_and_escapes_author_notes() {
        let html = page("video-brief.html", context! {
            brief => crate::video_brief::Brief { notes: "</textarea><script>bad()</script>".into(), title: "A title".into(), description: "A description".into() },
            root => "/tmp/project", busy => true, status => "Generating…", model => "chosen-model",
        });
        assert!(!html.contains("template error"));
        assert!(!html.contains("<script>bad()</script>"));
        assert!(html.contains("<fieldset disabled>"));
        // The one input is the notes. The copy is shown here, not edited: the
        // render writes it and the YouTube tab is where it is changed.
        assert!(html.contains("name=\"notes\""));
        assert!(!html.contains("name=\"title\""));
        assert!(!html.contains("name=\"description\""));
        assert!(!html.contains("Generate title"));
        assert!(html.contains("A title") && html.contains("A description"));
        assert!(html.contains("Render video writes the title and description"));
        assert!(html.find("id=\"copy-status\"").unwrap() < html.find("id=\"copy\"").unwrap(), "feedback above the copy it describes");
    }

    /// Before any render there is no copy to show, and the pane says what will
    /// write it rather than drawing an empty box.
    #[test]
    fn video_brief_without_copy_shows_only_the_notes() {
        let html = page("video-brief.html", context! {
            brief => crate::video_brief::Brief::default(),
            root => "/tmp/project", busy => false, status => "Ready", model => "m",
        });
        assert!(!html.contains("id=\"copy\""));
        assert!(html.contains("name=\"notes\""));
        assert!(!html.contains("<fieldset disabled>"));
    }

    #[test]
    fn an_empty_reflect_pane_invites_the_first_run() {
        let html = page("reflect.html", context! { report => None::<()>, can_apply => false });
        assert!(html.contains("Press Reflect"));
        assert!(html.contains("messageHandlers.app"), "the bridge is wired");
        // The payload is HTML-escaped inside the attribute; the click handler
        // JSON.parses it back. Assert on the escaped form the template emits.
        assert!(html.contains("data-send="), "buttons carry a payload");
        assert!(html.contains("reflect"), "the Reflect action is wired");
    }

    /// The thumbnail pane renders its grid, its switches and its drop zone.
    #[test]
    fn the_thumbnail_pane_renders_candidates_and_references() {
        let html = page(
            "thumbnail.html",
            context! {
                can_generate => true,
                blocked => None::<String>,
                brief_version => "thumbnail.brief v2",
                still => context! { url => "file:///tmp/still.jpg" },
                screen => context! { url => "file:///tmp/screen.jpg" },
                models => vec![
                    context! { id => "bytedance-seed/seedream-5-0-pro",
                               label => "Seedream 5 Pro", selected => true },
                    context! { id => "google/gemini-3.1-flash-image",
                               label => "Nano Banana 2", selected => false },
                ],
                card => context! {
                    title => "Ship it anyway",
                    description => "Why the queue fell over.",
                    kicker => "SAAGA",
                    themes => vec![
                        context! { id => "dark", label => "dark", selected => true },
                        context! { id => "light", label => "light", selected => false },
                    ],
                    focus => "0.34",
                    can_draw => true,
                    hint => "Save, then draw. The card lands in the candidates below.",
                },
                brief => context! {
                    title => "SHIP IT", description => "Presenter in a studio",
                },
                candidates => vec![
                    context! { id => "thumb-a", url => "file:///tmp/a.jpg",
                               label => "Nano Banana 2", selected => true },
                    context! { id => "thumb-b", url => "file:///tmp/b.jpg",
                               label => "Seedream 5 Pro", selected => false },
                ],
                references => vec![
                    context! { name => "ref.jpg", url => "file:///tmp/ref.jpg", active => true },
                ],
                active_id => "thumb-a",
            },
        );
        // minijinja escapes `/` in attributes as `&#x2f;`, which browsers decode.
        assert!(html.contains("a.jpg"), "the candidate image is sourced");
        assert!(html.contains("still.jpg"));
        assert!(html.contains("Seedream 5 Pro"));
        assert!(html.contains("· live"), "the chosen candidate is marked");
        assert!(html.contains("selectThumbnail"));
        assert!(html.contains("toggleReference"));
        assert!(html.contains("Drop images here"));
        assert!(html.contains("screen.jpg"), "the screen grab is shown too");
        // Two editable boxes, one press that saves both.
        assert!(html.contains(r#"data-field="title""#));
        assert!(html.contains(r#"data-field="description""#));
        assert!(html.contains("SHIP IT"));
        assert!(html.contains("saveBrief"));
        // The card form is on the same tab and posts its own message. The two
        // forms both have a `title` and a `description`, so they are scoped —
        // without that the card's headline would save as the model's brief.
        assert!(html.contains("saveCard"));
        assert!(html.contains("generateArtwork"));
        assert!(html.contains(r#"data-form="card""#));
        assert!(html.contains(r#"data-form="brief""#));
        assert!(html.contains("Ship it anyway"), "the card's headline is in its box");
        assert!(html.contains(r#"data-field="kicker""#));
        assert!(html.contains(r#"value="0.34""#), "the focus is where it was left");
        // Nothing drafts a prompt any more, so there is one generate button.
        assert!(!html.contains("regenerateImages"));
        // The model is picked in the pane, and the current one is preselected.
        assert!(html.contains("thumbnailModel"));
        // The id's `/` is escaped in the attribute, as noted above.
        assert!(html.contains("seedream-5-0-pro"));
        assert!(html.contains("selected"));
    }

    /// The card form's context, for the tests whose subject is something else on
    /// the same tab. Every one of them still has to supply it: the two forms
    /// share a template, and a pane that cannot render is a pane that shows
    /// nothing at all.
    fn empty_card() -> minijinja::Value {
        context! {
            title => "", description => "", kicker => "",
            themes => vec![
                context! { id => "dark", label => "dark", selected => true },
                context! { id => "light", label => "light", selected => false },
            ],
            focus => "0.50",
            can_draw => false,
            hint => "Type a title, save, then draw.",
        }
    }

    /// Selection is optimistic in the pane, so a repaint must still agree with
    /// what the ledger says — otherwise a later refresh would silently revert it.
    #[test]
    fn a_repaint_marks_the_candidate_the_ledger_says_is_live() {
        let html = page(
            "thumbnail.html",
            context! {
                can_generate => true, blocked => None::<String>,
                brief_version => "thumbnail.brief", still => None::<()>, brief => None::<()>,
                card => empty_card(),
                references => Vec::<()>::new(),
                candidates => vec![
                    context! { id => "thumb-a", url => "a.jpg", label => "m", selected => false },
                    context! { id => "thumb-b", url => "b.jpg", label => "m", selected => true },
                ],
            },
        );
        // Look at the markup only — the pane's own script mentions "· live" too.
        let markup = &html[..html.find("<script").expect("the pane has a script")];
        let a = markup.find("thumb-a").expect("a rendered");
        let b = markup.find("thumb-b").expect("b rendered");
        assert!(a < b, "order follows the context");
        assert_eq!(markup.matches("· live").count(), 1, "exactly one is live");
        assert!(markup[b..].contains("· live"), "and it is the one the ledger names");
        assert!(!markup[a..b].contains("· live"));
    }

    #[test]
    fn a_thumbnail_pane_with_no_still_blocks_generation() {
        let html = page(
            "thumbnail.html",
            context! {
                can_generate => false,
                blocked => "Capture a frame first.",
                brief_version => "thumbnail.brief",
                still => None::<()>,
                brief => None::<()>,
                card => empty_card(),
                candidates => Vec::<()>::new(),
                references => Vec::<()>::new(),
            },
        );
        assert!(html.contains("Capture a frame first."));
        assert!(html.contains("None yet"));
        assert!(html.contains("disabled"), "Generate is not offered");
    }

    /// The pane's job is to be typed from, so the beats and the verbatim quotes
    /// have to be on it — and each one has to carry a copy payload, because the
    /// quote is the one line that must not be retyped from memory.
    #[test]
    fn the_substack_pane_renders_beats_quotes_and_their_copy_payloads() {
        let html = page(
            "substack.html",
            context! {
                blocked => None::<String>,
                can_generate => true,
                summary => "1 section(s) · 2 beat(s) · 1 quote(s)",
                prompt => context! {
                    label => "v0 (builtin)", path => "/tmp/prompts/substack.notes.txt",
                    builtin => true,
                },
                titles => vec!["The retry that cost us a week"],
                subtitles => Vec::<String>::new(),
                hooks => vec!["The queue drained at 3am."],
                sections => vec![context! {
                    index => 1, heading => "Where it broke",
                    marker => "chapter 2 · 04:12",
                    beats => vec!["the retry had no ceiling", "12k duplicate rows"],
                }],
                quotes => vec![context! {
                    text => "I assumed the ledger would catch it.",
                    marker => "chapter 2 · 05:01",
                }],
                close => vec!["Check what your retries cannot see."],
                links => vec![context! { label => "Watch", url => "https://y/1" }],
                markdown => "# Substack notes\n",
                markdown_path => "/tmp/substack/notes.md",
            },
        );
        assert!(html.contains("Where it broke"));
        assert!(html.contains("chapter 2 · 04:12"));
        assert!(html.contains("the retry had no ceiling"));
        assert!(html.contains("I assumed the ledger would catch it."));
        assert!(html.contains("verbatim"), "the quotes are labelled as such");
        assert!(html.contains("generateSubstack"));
        assert!(html.contains("copyText"), "every row carries a copy payload");
        assert!(html.contains("editSubstackPrompt"));
        assert!(html.contains("v0 (builtin)"));
        // An empty group is omitted rather than shown as a heading with nothing
        // under it, which reads as a finding of "none".
        assert!(!html.contains("Subtitle options"));
        assert!(html.contains("Closing options"));
    }

    /// Everything a human should check before a permanent public URL exists:
    /// the description length, the headings that become the table of contents,
    /// and the byline it will carry.
    #[test]
    fn the_blog_pane_shows_what_has_to_be_checked_before_publishing() {
        let html = page(
            "blog.html",
            context! {
                blocked => None::<String>,
                can_publish => true,
                author => "Ahmed Raza",
                category => "Education",
                library_hint => "3 author(s), 1 category(ies), read 2026-08-29T00:00:00Z",
                authors => vec![
                    context! { id => "", label => "— none —", selected => false },
                    context! { id => "12", label => "Ahmed Raza — Senior SEO Content Strategist",
                               selected => true },
                ],
                categories => vec![
                    context! { id => "", label => "— none —", selected => false },
                    context! { id => "2", label => "Education — Educational video content",
                               selected => true },
                ],
                prompt => context! {
                    label => "v0 (builtin)", path => "/tmp/prompts/blog.article.txt",
                    builtin => true,
                },
                posted => None::<()>,
                figures => context! {
                    can_write => true,
                    hint => "2 figures · 1 still to write about.",
                    rows => vec![
                        context! {
                            n => 2, label => "figure 02",
                            url => "file:///tmp/figures/figure-02.jpg",
                            moment => "ch 03 · 1:24", size => "1280 × 720",
                            caption => "The retry storm that took the queue down.",
                            alt => "A log filling with 429s", written => true,
                        },
                        context! {
                            n => 1, label => "figure 01",
                            url => "file:///tmp/figures/figure-01.jpg",
                            moment => "ch 01 · 0:12", size => "900 × 600",
                            caption => "", alt => "", written => false,
                        },
                    ],
                },
                article_path => "/tmp/blog/article.json",
                article => context! {
                    title => "Why watermarking fails",
                    h1 => "Why watermarking fails, and what enforcement costs",
                    slug => "why-watermarking-fails",
                    description => "A complete promise of the argument.",
                    description_length => 36,
                    caption => "Four minutes on enforcement economics",
                    keywords => vec!["ai", "watermarking"],
                    faq => vec!["Does it scale? — Not past the first retry."],
                    keyword_targets => vec!["primary · ai watermarking — informational"],
                    summary => "2 section(s) · 1 quote(s) · 0 table(s)",
                    blocks => vec![context! {
                        index => 1, kind => "text",
                        headings => vec!["Where it broke"],
                        body => "<h2>Where it broke</h2><p>Body.</p>",
                    }],
                },
            },
        );
        assert!(html.contains("Why watermarking fails"));
        assert!(html.contains("/blog/why-watermarking-fails"));
        assert!(html.contains("140 to 160"), "the description target is stated");
        // Both SEO fields the generator now writes. The FAQ especially: it
        // publishes as structured data, so it has to be readable before the
        // publish rather than after.
        assert!(html.contains("and what enforcement costs"), "the heading is shown");
        assert!(html.contains("Not past the first retry."), "the FAQ is shown");
        assert!(html.contains("primary · ai watermarking"), "the keyword brief is shown");
        assert!(html.contains("Where it broke"), "the table of contents is shown");
        assert!(html.contains("Ahmed Raza"), "the byline is visible");
        assert!(html.contains("Education"));
        assert!(html.contains("publishBlog"));
        assert!(html.contains("editBlogPrompt"));
        // Write and Preview come before Create Draft, in that order: it is the
        // order of commitment, and it is the whole reason a draft can be read
        // before a permanent slug exists.
        let write_at = html.find("writeBlog").expect("the write button");
        let preview_at = html.find("previewBlog").expect("the preview button");
        let publish_at = html.find("publishBlog").expect("the publish button");
        assert!(write_at < preview_at && preview_at < publish_at, "buttons out of order");
        assert!(
            html.contains("Create Draft publishes the draft below as it stands"),
            "the pane says the draft on disk is what publishes"
        );
        // The pickers, and the row each one is currently on. Without `selected`
        // the dropdown would open on whatever is first and a glance at the tab
        // would report the wrong byline.
        assert!(html.contains("blogAuthor"), "the author picker is on the page");
        assert!(html.contains("blogCategory"));
        assert!(html.contains("refreshBlogLibrary"));
        // The figure strip, and the two states a figure can be in. A captured
        // figure with no blurb has to say so rather than render an empty
        // caption, which reads as a blurb that came back blank.
        assert!(html.contains("captureFigure"), "the snip button is on the page");
        assert!(html.contains("writeBlurbs"));
        assert!(html.contains("figure 02 · ch 03 · 1:24 · 1280 × 720"));
        assert!(html.contains("The retry storm that took the queue down."));
        assert!(html.contains("alt: A log filling with 429s"));
        assert!(html.contains("No blurb yet."), "the unwritten figure says so");
        assert!(html.contains("1 still to write about"));
        assert!(
            html.contains(r#"<option value="12" selected>"#),
            "the chosen author is the selected option"
        );
        assert!(html.contains(r#"<option value="2" selected>"#));
    }

    /// Once it is live, both URLs are on the page — the public one to read and
    /// the admin one to fix it through.
    #[test]
    fn a_published_blog_pane_carries_both_urls_and_any_warning() {
        let html = page(
            "blog.html",
            context! {
                blocked => None::<String>,
                can_publish => false,
                author => "Andrew Melnychuk-Oseen",
                category => "AI Powered Marketing",
                prompt => context! { label => "v2", path => "/tmp/p.txt", builtin => false },
                posted => context! {
                    url => "https://saagasolve.com/blog/why-watermarking-fails",
                    admin_url => "https://cms.saagasolve.com/admin/x",
                    slug => "why-watermarking-fails",
                    published => true,
                    created_at => "2026-08-18T12:00:00Z",
                    warning => "author could not be set",
                },
                article => None::<()>,
                article_path => None::<String>,
            },
        );
        // Displayed with `/` as `&#x2f;` — minijinja escapes it and the browser
        // decodes it back, so the host is what to assert on, not the whole URL.
        assert!(html.contains("saagasolve.com"));
        assert!(html.contains("cms.saagasolve.com"));
        assert!(html.contains("published"));
        assert!(html.contains("author could not be set"), "a warning is not swallowed");
        // Both URLs are copyable, and the payload carries them unescaped. The
        // admin one especially: it is how a bad post gets fixed.
        assert!(html.contains("https://saagasolve.com/blog/why-watermarking-fails"));
        assert!(html.contains("https://cms.saagasolve.com/admin/x"));
    }

    #[test]
    fn an_empty_blog_pane_invites_the_first_run() {
        let html = page(
            "blog.html",
            context! {
                blocked => "Not on YouTube yet — upload it on the YouTube tab first",
                can_publish => false,
                author => "Andrew Melnychuk-Oseen",
                category => "AI Powered Marketing",
                prompt => context! { label => "v0 (builtin)", path => "", builtin => true },
                posted => None::<()>,
                article => None::<()>,
                article_path => None::<String>,
            },
        );
        assert!(html.contains("Not on YouTube yet"));
        assert!(!html.contains("article.json"));
    }

    /// Before the first run the pane says what to press, and Copy All is not
    /// offered — a button that puts an empty string on the clipboard is worse
    /// than no button.
    #[test]
    fn an_empty_substack_pane_invites_the_first_run() {
        let html = page(
            "substack.html",
            context! {
                blocked => "No notes yet — press Generate Notes.",
                can_generate => true,
                summary => None::<String>,
                prompt => context! { label => "v0 (builtin)", path => "", builtin => true },
                titles => Vec::<String>::new(), subtitles => Vec::<String>::new(),
                hooks => Vec::<String>::new(), sections => Vec::<()>::new(),
                quotes => Vec::<()>::new(), close => Vec::<String>::new(),
                links => Vec::<()>::new(), markdown => "", markdown_path => None::<String>,
            },
        );
        assert!(html.contains("press Generate Notes"));
        assert!(!html.contains("Copy All"));
    }

    /// The Edit pane carries a chapter rail, a waveform, word chips and its actions.
    #[test]
    fn the_edit_pane_renders_a_chapter_ready_to_trim() {
        use crate::edit::pane::{Chapter, Pane, Row, Word};

        let html = edit_page(&Pane {
            chapters: vec![
                Row {
                    n: 1,
                    label: "01".into(),
                    source_label: "2:14".into(),
                    cut_label: "2:01".into(),
                    edited: false,
                    open: true,
                },
                Row {
                    n: 2,
                    label: "02".into(),
                    source_label: "3:12".into(),
                    cut_label: "2:48".into(),
                    edited: true,
                    open: false,
                },
            ],
            open: Some(Chapter {
                n: 1,
                label: "01".into(),
                video_url: "file:///takes/chapter-01-horizontal.mp4".into(),
                duration_ms: 134_000,
                peaks_hz: 100,
                peaks: vec![0, 128, 255],
                spans: vec![[0, 41_200], [43_900, 121_000]],
                words: vec![
                    Word { text: "hello".into(), start: 0, end: 500, kept: true },
                    Word { text: "um".into(), start: 41_500, end: 43_000, kept: false },
                ],
                edited: false,
                source_label: "2:14".into(),
                cut_label: "2:01".into(),
                dropped_label: "0:13".into(),
                segments: 2,
                note: None,
            }),
            blocked: None,
        });

        // The rail lists every chapter and marks the hand-edited one.
        assert!(html.contains("openChapter"), "the rail switches chapters");
        assert!(html.contains("3:12"), "another chapter's length is on the rail");
        // The editor's own payloads.
        assert!(html.contains("applyEdit"));
        assert!(html.contains("resetEdit"));
        assert!(html.contains("saveEdit"));
        // The timeline's data, inlined rather than fetched.
        assert!(html.contains("[0,128,255]"), "peaks reach the pane");
        assert!(html.contains("[[0,41200],[43900,121000]]"), "so do the spans");
        assert!(html.contains("chapter-01-horizontal.mp4"));
        // Word chips, with the dropped one struck through.
        assert!(html.contains(r#"data-start="41500""#));
        assert!(html.contains("dropped"), "a cut word is marked");
        // And the editor itself is carried in the binary, not fetched from a CDN.
        assert!(html.contains("WaveSurfer"), "the bundle is inlined");
        assert!(html.contains("Regions"), "so is the regions plugin");
        assert!(
            !html.contains("<script src="),
            "nothing is loaded over the network"
        );
    }

    /// A take with nothing recorded says so instead of rendering a dead editor.
    #[test]
    fn an_empty_edit_pane_says_what_is_missing() {
        use crate::edit::pane::Pane;

        let html = edit_page(&Pane {
            chapters: Vec::new(),
            open: None,
            blocked: Some("No chapters recorded yet — record one first.".into()),
        });
        assert!(html.contains("record one first"));
        assert!(!html.contains("applyEdit"), "there is nothing to apply");
        // The bundle is not shipped into a pane that has no waveform to draw.
        assert!(!html.contains("WaveSurfer.create"));
    }

    #[test]
    fn a_missing_template_shows_the_error_rather_than_a_blank_pane() {
        let html = page("nope.html", context! {});
        assert!(html.contains("template error"));
        assert!(html.contains("no template nope.html"));
    }

    /// A template bug must not be able to inject markup through its own message.
    #[test]
    fn the_error_page_escapes_what_it_reports() {
        let html = error_page("<script>alert(1)</script>");
        assert!(!html.contains("<script>alert"));
        assert!(html.contains("&lt;script&gt;"));
    }

    #[test]
    fn a_rendered_rewrite_carries_its_diff_and_payloads() {
        let html = page(
            "reflect.html",
            context! {
                can_apply => true,
                applicable => 1,
                report => context! {
                    project => "vd-42",
                    inputs => "3 llm step(s)",
                    generated_at => "2026-08-16T09:00:00Z",
                    keep => vec!["short hooks"],
                    drop => Vec::<String>::new(),
                    evidence => Vec::<()>::new(),
                    rewrites => vec![context! {
                        index => 0,
                        prompt_id => "posts.social",
                        scale => "+2 −1 lines",
                        status => "ready to apply",
                        tone => "ok",
                        why => "captions were shortened by hand",
                        version_note => "v1 → would become v2",
                        approved => true,
                        validation => "12 post(s), all within platform limits",
                        diff => vec![
                            context! { kind => "same", text => "Be concise." },
                            context! { kind => "del", text => "Old rule." },
                            context! { kind => "add", text => "New rule." },
                        ],
                    }],
                }
            },
        );
        assert!(html.contains("posts.social"));
        assert!(html.contains("+2 −1 lines"));
        assert!(html.contains("ready to apply"));
        assert!(html.contains(r#"class="del""#), "a deletion is visibly marked");
        assert!(html.contains("Old rule."));
        assert!(html.contains("checked"), "an approved rewrite stays ticked");
        assert!(html.contains("12 post(s)"));
        // Keep renders, drop is omitted entirely rather than shown empty.
        assert!(html.contains("short hooks"));
        assert!(!html.contains("<h2>Drop</h2>"));
    }

    #[test]
    fn ungrounded_evidence_is_flagged_in_the_pane() {
        let html = page(
            "reflect.html",
            context! {
                can_apply => false,
                applicable => 0,
                report => context! {
                    project => "vd-42",
                    inputs => "1 llm step(s)",
                    generated_at => "now",
                    keep => Vec::<String>::new(),
                    drop => Vec::<String>::new(),
                    rewrites => Vec::<()>::new(),
                    evidence => vec![context! {
                        claim => "shorter is better",
                        grounded => false,
                        backing => "",
                    }],
                }
            },
        );
        assert!(html.contains("no posts cited"));
        assert!(html.contains("warn"));
        assert!(html.contains("No rewrites proposed"));
    }
    #[test]
    fn youtube_details_are_editable_and_escaped() {
        let html = page("youtube.html", minijinja::context! {
            metadata => crate::publish::metadata::Metadata {
                title: "A <video>".into(), description: "Text & details".into(),
            }, info => "Ready to upload",
        });
        assert!(html.contains("A &lt;video&gt;"));
        assert!(html.contains("Text &amp; details"));
        assert!(html.contains("saveYoutube"));
        assert!(html.contains("Save video details"));
        assert!(!html.contains("Template error"));
    }

}
