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
/// The recording page's right-hand pane: details, artwork and review in one.
const VIDEO: &str = include_str!("templates/video.html");
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
    env.add_template("studio.html", include_str!("templates/studio.html"))
        .expect("studio styles");
    env.add_template("thumbnail.html", include_str!("templates/thumbnail.html"))
        .expect("thumbnail template");
    env.add_template("video.html", VIDEO)
        .expect("video template");
    env.add_template("edit.html", EDIT).expect("edit template");
    env.add_template("substack.html", SUBSTACK)
        .expect("substack template");
    env.add_template("blog.html", BLOG).expect("blog template");
    env.add_template("youtube.html", include_str!("templates/youtube.html"))
        .expect("youtube template");
    env.add_template("settings.html", include_str!("templates/settings.html"))
        .expect("settings template");
    env.add_template("project.html", include_str!("templates/project.html"))
        .expect("project template");
    env.add_template("plan.html", include_str!("templates/plan.html"))
        .expect("plan template");
    env.add_template(
        "teleprompter.html",
        include_str!("templates/teleprompter.html"),
    )
    .expect("teleprompter template");
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
            "video.html",
            "substack.html",
            "edit.html",
            "blog.html",
            "youtube.html",
            "project.html",
            "plan.html",
        ] {
            assert!(env.get_template(name).is_ok(), "{name} is registered");
        }
    }

    /// The Thumbnail tab's artwork with nothing drawn and no photo yet.
    fn empty_art() -> minijinja::Value {
        context! {
            artwork => Vec::<()>::new(), artwork_notice => None::<String>,
            still => None::<()>, screen => None::<()>,
            brief => context! { title => "", description => "" },
            candidates => Vec::<()>::new(), models => Vec::<()>::new(),
            references => Vec::<()>::new(), active_id => None::<String>,
            can_generate => false, blocked => "Capture a frame first.",
            card => context! {
                format => "horizontal", title => "", description => "", kicker => "",
                themes => vec![
                    context! { id => "dark", label => "dark", selected => true },
                    context! { id => "light", label => "light", selected => false },
                ],
                focus => "0.50", can_draw => false, hint => "Capture your photo first.",
            },
        }
    }

    /// Where the thumbnail stands, as the recording page's strip reads it.
    fn no_thumbnail() -> minijinja::Value {
        context! { review => "none", has_photo => false }
    }

    fn no_clips() -> minijinja::Value {
        context! { clips => Vec::<()>::new(), blocked => "Nothing rendered yet — run Render." }
    }

    fn no_figures() -> minijinja::Value {
        context! {
            rows => Vec::<()>::new(), can_write => false,
            hint => "⌃⇧S over the screen, then drag, to capture a figure.",
        }
    }

    fn settings_with(youtube: crate::publish::Account) -> String {
        page(
            "settings.html",
            context! {
                sections => Vec::<()>::new(), missing => Vec::<String>::new(),
                env_path => "/tmp/.env", team_count => 0, models => Vec::<()>::new(),
                photo_countdown => 3, max_photo_countdown => 10, saved => "", youtube => youtube,
            },
        )
    }

    /// Connect lives on Settings now; Copy only appears once there is a grant
    /// to copy, and the page is never handed the token itself.
    #[test]
    fn the_settings_pane_carries_the_youtube_account_card() {
        let bare = settings_with(crate::publish::Account {
            channel: None,
            shared: false,
            local_differs: false,
            can_copy: false,
        });
        assert!(!bare.contains("template error"), "{bare}");
        assert!(bare.contains(r#"{"type":"connectYoutube"}"#));
        assert!(!bare.contains("copyYoutubeRefreshToken"));
        assert!(bare.contains("not connected"));

        let shared = settings_with(crate::publish::Account {
            channel: Some("SAAGA Solve (UCLhaTNJktUZeDjge3tOv11Q)".into()),
            shared: true,
            local_differs: true,
            can_copy: true,
        });
        assert!(shared.contains("copyYoutubeRefreshToken"));
        assert!(shared.contains("Uploads go to SAAGA Solve"));
        assert!(shared.contains("team's shared grant"));
        assert!(shared.contains("overrides"));
    }

    #[test]
    fn the_video_pane_preserves_and_escapes_author_notes() {
        let html = page(
            "video.html",
            context! {
                brief => crate::video_brief::Brief { notes: "</textarea><script>bad()</script>".into(), title: "A title".into(), description: "A description".into() },
                root => "/tmp/project", busy => true, model => "chosen-model",
                thumbnail => no_thumbnail(), review => no_clips(), youtube => None::<String>, figures => no_figures(),
            },
        );
        assert!(!html.contains("template error"), "{html}");
        assert!(!html.contains("<script>bad()</script>"));
        assert!(html.contains("<fieldset disabled>"));
        // The one input is the notes. The copy is shown here, not edited: the
        // render writes it and the YouTube tab is where it is changed.
        assert!(html.contains("name=\"notes\""));
        assert!(!html.contains("name=\"title\""));
        assert!(!html.contains("Generate title"));
        assert!(html.contains("A title") && html.contains("A description"));
        assert!(
            html.contains("<b>Render video</b>") && html.contains("generateVideoCopy"),
            "the pane names the render button and has its own Write button"
        );
    }

    /// A drawn set with its photo, the design and the AI experiments, as the
    /// Thumbnail tab is handed it.
    fn drawn_art() -> minijinja::Value {
        context! {
            artwork => vec![
                context! { id => "horizontal", url => "file:///tmp/sets/a/horizontal.jpg", label => "horizontal · 1280×720", selected => true },
                context! { id => "vertical", url => "file:///tmp/sets/a/vertical.jpg", label => "vertical · 720×1280", selected => true },
                context! { id => "og", url => "file:///tmp/sets/a/og.jpg", label => "og · 1200×630", selected => true },
            ],
            artwork_notice => None::<String>,
            still => context! { url => "file:///tmp/still.jpg" },
            screen => context! { url => "file:///tmp/screen.jpg" },
            brief => context! { title => "SHIP IT", description => "Presenter in a studio" },
            candidates => vec![
                context! { id => "thumb-a", url => "file:///tmp/a.jpg", label => "Nano Banana 2", selected => true },
            ],
            models => vec![
                context! { id => "bytedance-seed/seedream-5-0-pro", label => "Seedream 5 Pro", selected => true },
            ],
            references => vec![
                context! { name => "ref.jpg", url => "file:///tmp/ref.jpg", active => true },
            ],
            active_id => "thumb-a", can_generate => true, blocked => None::<String>,
            card => context! {
                format => "horizontal", title => "Ship it anyway", description => "Why the queue fell over.",
                kicker => "SAAGA",
                themes => vec![
                    context! { id => "dark", label => "dark", selected => true },
                    context! { id => "light", label => "light", selected => false },
                ],
                focus => "0.34", can_draw => true, hint => "Redraws all three.",
            },
        }
    }

    fn thumbnail_page(art: minijinja::Value, review: &str) -> String {
        page(
            "thumbnail.html",
            context! {
                art => art, review => review, can_approve => review == "drafted",
                drawing => false, approved_at => None::<String>, title => "Ship it anyway",
                live => false, photo_countdown => 3,
            },
        )
    }

    /// Before any render there is no copy and nothing to watch, and each
    /// section says what will fill it rather than drawing an empty box.
    #[test]
    fn an_empty_video_pane_says_what_the_render_will_produce() {
        let html = page(
            "video.html",
            context! {
                brief => crate::video_brief::Brief::default(),
                root => "/tmp/project", busy => false, model => "m",
                thumbnail => no_thumbnail(), review => no_clips(), youtube => None::<String>, figures => no_figures(),
            },
        );
        assert!(!html.contains("template error"), "{html}");
        assert!(!html.contains("id=\"copy\""));
        assert!(html.contains("Nothing written yet"));
        assert!(html.contains("Nothing rendered yet"));
        assert!(html.contains("name=\"notes\""));
        assert!(!html.contains("<fieldset disabled>"));
        // The artwork is the Thumbnail tab's: none of its controls are here.
        for gone in [
            "generateArtwork",
            "captureFrame",
            "approveThumbnail",
            "data-form=\"card\"",
        ] {
            assert!(!html.contains(gone), "{gone} is on the recording page");
        }
        assert!(
            html.contains("Thumbnail</b> tab"),
            "the page says where the thumbnail went"
        );
    }

    /// After a render the recording page is the copy, the figures and the
    /// clips, in the order they are produced, and its strip says where the
    /// thumbnail stands without showing it.
    #[test]
    fn a_rendered_video_pane_shows_copy_figures_and_clips_in_that_order() {
        let html = page(
            "video.html",
            context! {
                brief => crate::video_brief::Brief { notes: "".into(), title: "Ship it anyway".into(), description: "Why the queue fell over.".into() },
                root => "/tmp/project", busy => false, model => "m",
                thumbnail => context! { review => "drafted", has_photo => true },
                review => context! {
                    blocked => None::<String>,
                    clips => vec![
                        context! { id => "longform", label => "Longform", url => "file:///tmp/render/horizontal/longform.mp4",
                                   orientation => "landscape", megabytes => 12.5 },
                        context! { id => "chapter-01", label => "Chapter 01", url => "file:///tmp/render/vertical/chapter-01.mp4",
                                   orientation => "portrait", megabytes => 3.0 },
                    ],
                },
                youtube => None::<String>,
                figures => context! {
                    can_write => true,
                    hint => "2 figures · 1 still to write about.",
                    rows => vec![
                        context! {
                            n => 2, label => "figure 02", url => "file:///tmp/figures/figure-02.webp",
                            moment => "ch 03 · 1:24", size => "1280 × 720",
                            caption => "The retry storm that took the queue down.", alt => "A log filling with 429s",
                            written => true, said => "So this is the log at three in the morning, every line a 429.",
                            said_note => "",
                        },
                        context! {
                            n => 1, label => "figure 01", url => "file:///tmp/figures/figure-01.webp",
                            moment => "ch 01 · 0:12", size => "900 × 600",
                            caption => "", alt => "", written => false, said => "",
                            said_note => "Transcribing what you said…",
                        },
                    ],
                },
            },
        );
        assert!(!html.contains("template error"), "{html}");
        assert!(
            html.contains("every line a 429"),
            "the spoken explanation is on the page"
        );
        assert!(
            html.contains("Transcribing what you said…"),
            "a figure still transcribing says so"
        );
        assert!(html.contains("The retry storm that took the queue down."));
        assert!(html.contains("captureFigure") && html.contains("writeBlurbs"));
        // Order of production is the order on the page.
        let copy = html.find("id=\"copy\"").expect("the copy box");
        let figs = html.find("figure-02.webp").expect("the figures");
        let clips = html.find("longform.mp4").expect("the clips");
        assert!(
            copy < figs && figs < clips,
            "copy, then figures, then review"
        );
        assert!(html.contains("clip landscape") && html.contains("clip portrait"));
        assert!(html.contains("12.5 MB"));
        assert!(
            !html.contains("horizontal.jpg"),
            "the artwork is on the Thumbnail tab"
        );
        // The strip: photo, render and copy done; the thumbnail waits on review.
        let steps = &html[html.find("class=\"steps\"").unwrap()..html.find("</ol>").unwrap()];
        assert_eq!(steps.matches("\"done\"").count(), 3, "{steps}");
        assert!(
            steps.contains(r#"class="review""#) && steps.contains("Thumbnail · review"),
            "{steps}"
        );
        assert!(steps.contains("YouTube"));
    }

    /// The strip's thumbnail dot follows the review: approved is done, a stale
    /// set is flagged, and nothing drawn is neither.
    #[test]
    fn the_video_strip_reads_the_thumbnail_review() {
        let strip = |state: &str| {
            let html = page(
                "video.html",
                context! {
                    brief => crate::video_brief::Brief::default(), root => "/tmp/p", busy => false, model => "m",
                    thumbnail => context! { review => state, has_photo => false },
                    review => no_clips(), youtube => None::<String>, figures => no_figures(),
                },
            );
            assert!(!html.contains("template error"), "{html}");
            let steps = &html[html.find("class=\"steps\"").unwrap()..html.find("</ol>").unwrap()];
            let item = &steps[..steps.find("Thumbnail").unwrap()];
            item[item.rfind("<li").unwrap()..].to_string()
        };
        assert!(strip("approved").contains(r#"class="done""#));
        assert!(strip("stale").contains(r#"class="stale""#));
        assert!(strip("none").contains(r#"class="""#));
    }

    /// The Thumbnail tab is the whole artwork step: the three pictures, the
    /// photo they were drawn from, every correction, and Approve.
    #[test]
    fn the_thumbnail_pane_shows_the_set_its_photo_and_approve() {
        let html = thumbnail_page(drawn_art(), "drafted");
        assert!(!html.contains("template error"), "{html}");
        assert!(
            html.contains("horizontal.jpg")
                && html.contains("vertical.jpg")
                && html.contains("og.jpg")
        );
        assert!(html.contains("still.jpg") && html.contains("screen.jpg"));
        assert!(
            html.contains("Ship it anyway"),
            "the title it was drawn for"
        );
        assert!(html.contains(r#"class="badge">Needs review"#));
        // Approve is the primary action and is live for a drafted set.
        let approve = html
            .find(r#"{"type":"approveThumbnail"}"#)
            .expect("Approve");
        let button = &html[html[..approve].rfind("<button").unwrap()..approve];
        assert!(
            button.contains("primary") && !button.contains("disabled"),
            "{button}"
        );
        // Approve uploads nothing: that is the YouTube tab's step.
        assert!(html.contains("Approving uploads nothing"));
        // Every correction moved here with it.
        for control in [
            "captureFrame",
            "captureScreen",
            "generateArtwork",
            "saveCard",
            "portrait-file",
            "retake-photo",
            "selectThumbnail",
            "toggleReference",
            "Drop images here",
            "thumbnailModel",
            "saveBrief",
        ] {
            assert!(html.contains(control), "missing {control}");
        }
        assert!(html.contains(r#"data-form="card""#) && html.contains(r#"data-field="kicker""#));
        assert!(
            html.contains(r#"value="0.34""#),
            "the focus is where it was left"
        );
        assert_eq!(html.matches("<details class=\"acc\">").count(), 2);
        // The countdown overlay and its script came along.
        assert!(html.contains("id=\"countdown\"") && html.contains("FileReader"));
    }

    /// Approve is only ever offered for a current, drafted set.
    #[test]
    fn approve_is_off_unless_a_drafted_set_is_waiting() {
        let approve_disabled = |html: &str| {
            let at = html
                .find(r#"{"type":"approveThumbnail"}"#)
                .expect("Approve");
            html[html[..at].rfind("<button").unwrap()..at].contains("disabled")
        };
        let empty = thumbnail_page(empty_art(), "none");
        assert!(!empty.contains("template error"), "{empty}");
        assert!(approve_disabled(&empty));
        assert!(empty.contains("No artwork yet") && empty.contains("No photo yet"));
        let redraw = empty.find("generateArtwork").expect("the redraw button");
        assert!(
            empty[..redraw].rfind("disabled").is_some(),
            "nothing to redraw from"
        );

        let approved = thumbnail_page(drawn_art(), "approved");
        assert!(approve_disabled(&approved));
        assert!(
            approved.contains(">Approved</button>") && approved.contains("needs approving again")
        );
    }

    /// A stale set is shown with the reason it is stale, above the pictures it
    /// is about, and cannot be approved until it is redrawn.
    #[test]
    fn a_stale_artwork_set_says_why_above_the_pictures() {
        let mut art = serde_json::to_value(empty_art()).unwrap();
        art["artwork_notice"] =
            "Design changed since the artwork was drawn — redraw it before publishing".into();
        art["artwork"] = serde_json::json!([
            { "id": "horizontal", "url": "file:///tmp/sets/a/horizontal.jpg", "label": "horizontal · 1280×720", "selected": true }
        ]);
        let html = thumbnail_page(minijinja::Value::from_serialize(&art), "stale");
        assert!(!html.contains("template error"), "{html}");
        let notice = html.find("Design changed").expect("the notice");
        let picture = html.find("horizontal.jpg").expect("the picture");
        assert!(notice < picture);
        assert!(
            html.contains(r#"class="badge warn">Stale"#),
            "the card is badged stale"
        );
        assert!(html.contains("Redraw it before it can be approved"));
    }

    /// Once the upload has happened the strip says so — the last stage of the
    /// render's chain, read without a trip to the YouTube tab.
    #[test]
    fn an_uploaded_project_completes_the_pipeline_strip() {
        let html = page(
            "video.html",
            context! {
                brief => crate::video_brief::Brief::default(),
                root => "/tmp/project", busy => false, model => "m",
                thumbnail => no_thumbnail(), review => no_clips(), figures => no_figures(),
                youtube => "https://www.youtube.com/watch?v=abc",
            },
        );
        assert!(!html.contains("template error"), "{html}");
        let steps = &html[html.find("class=\"steps\"").unwrap()..html.find("</ol>").unwrap()];
        let youtube = steps.rfind("<li").unwrap();
        assert!(steps[youtube..].contains("done"), "{steps}");
    }

    /// Both cuts get their link on the pane, the longform first. The Short
    /// lands second and its status line used to be the only link left on
    /// screen.
    #[test]
    fn an_uploaded_project_lists_both_youtube_links() {
        let html = page(
            "video.html",
            context! {
                brief => crate::video_brief::Brief::default(),
                root => "/tmp/project", busy => false, model => "m",
                thumbnail => no_thumbnail(), review => no_clips(), figures => no_figures(),
                youtube => "https://www.youtube.com/watch?v=abc",
                short => "https://www.youtube.com/shorts/def",
            },
        );
        assert!(!html.contains("template error"), "{html}");
        let card = &html[html.find("Published").expect("the upload card")..];
        let longform = card
            .find("https://www.youtube.com/watch?v=abc")
            .expect("the longform link");
        let short = card
            .find("https://www.youtube.com/shorts/def")
            .expect("the short link");
        assert!(longform < short, "{card}");
        // The S3 upload has its row too, since the tab that listed it is gone.
        assert!(card.contains("Not on S3 yet"), "{card}");

        // Nothing up yet: no card, rather than three empty rows.
        let html = page(
            "video.html",
            context! {
                brief => crate::video_brief::Brief::default(),
                root => "/tmp/project", busy => false, model => "m",
                thumbnail => no_thumbnail(), review => no_clips(), figures => no_figures(),
                youtube => None::<String>, short => None::<String>,
            },
        );
        assert!(!html.contains("Published"), "{html}");
    }

    /// The renders on S3 are what Buffer posts from, and the Upload media tab
    /// that used to list them is gone — so the card carries the count and the
    /// links file, with nothing on YouTube needed to show it.
    #[test]
    fn a_hosted_project_counts_its_s3_files_on_the_card() {
        let html = page(
            "video.html",
            context! {
                brief => crate::video_brief::Brief::default(),
                root => "/tmp/project", busy => false, model => "m",
                thumbnail => no_thumbnail(), review => no_clips(), figures => no_figures(),
                youtube => None::<String>, short => None::<String>,
                hosted => context! { count => 9, path => "/tmp/project/distribute/v1/links.json" },
            },
        );
        assert!(!html.contains("template error"), "{html}");
        let card = &html[html.find("Published").expect("the card")..];
        assert!(card.contains("9 public file(s) for Buffer"), "{card}");
        assert!(
            card.contains("/tmp/project/distribute/v1/links.json"),
            "{card}"
        );
    }

    #[test]
    fn an_empty_reflect_pane_invites_the_first_run() {
        let html = page(
            "reflect.html",
            context! { report => None::<()>, can_apply => false },
        );
        assert!(html.contains("Press Reflect"));
        assert!(html.contains("messageHandlers.app"), "the bridge is wired");
        // The payload is HTML-escaped inside the attribute; the click handler
        // JSON.parses it back. Assert on the escaped form the template emits.
        assert!(html.contains("data-send="), "buttons carry a payload");
        assert!(html.contains("reflect"), "the Reflect action is wired");
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
        assert!(
            html.contains("copyText"),
            "every row carries a copy payload"
        );
        assert!(html.contains("editSubstackPrompt"));
        assert!(html.contains("v0 (builtin)"));
        // An empty group is omitted rather than shown as a heading with nothing
        // under it, which reads as a finding of "none".
        assert!(!html.contains("Subtitle options"));
        assert!(html.contains("Closing options"));
    }

    /// A draft the CMS would refuse shows the field to fix, editable, under the
    /// key the form posts back, with the count the live counter starts from.
    /// Publish is off while the card is up, and both ways out are offered.
    #[test]
    fn the_blog_pane_lists_over_limit_fields_to_edit() {
        let html = page(
            "blog.html",
            context! {
                blocked => None::<String>,
                can_publish => false,
                author => "Andrew", category => "Education", library_hint => "read",
                authors => Vec::<()>::new(), categories => Vec::<()>::new(),
                prompt => context! { label => "v0 (builtin)", path => "/tmp/p", builtin => true },
                posted => None::<()>,
                figures => context! { can_write => false, hint => "", rows => Vec::<()>::new() },
                fixes => vec![context! {
                    key => "quote_text:4", label => "Block 5 · quote",
                    problem => "is 326 characters and the CMS holds 255",
                    text => "q".repeat(326), length => 326, limit => 255,
                }],
                article => None::<()>,
            },
        );
        assert!(!html.contains("template error"), "{html}");
        assert!(html.contains("Needs fixing before Publish"), "{html}");
        assert!(
            html.contains(r#"<textarea name="quote_text:4" rows="4" data-limit="255">"#),
            "{html}"
        );
        assert!(html.contains("326 / 255"), "{html}");
        assert!(html.contains(r#"class="count over""#), "{html}");
        assert!(html.contains("Save fixes"), "{html}");
        assert!(html.contains(r#"{"type":"repairBlog"}"#), "{html}");

        // No card at all when nothing is over.
        let html = page(
            "blog.html",
            context! {
                blocked => None::<String>, can_publish => true,
                author => "", category => "", library_hint => "",
                authors => Vec::<()>::new(), categories => Vec::<()>::new(),
                prompt => context! { label => "v0", path => "", builtin => true },
                posted => None::<()>,
                figures => context! { can_write => false, hint => "", rows => Vec::<()>::new() },
                fixes => Vec::<()>::new(), article => None::<()>,
            },
        );
        assert!(!html.contains("Needs fixing"), "{html}");
    }

    /// The slug is the one field on the page that becomes a permanent URL, so
    /// it is typed in rather than only read — until the post is live, when the
    /// draft's slug means nothing and the card above carries the real one.
    #[test]
    fn the_blog_pane_lets_the_slug_be_set_until_the_post_is_live() {
        let ctx = |posted: minijinja::Value| {
            context! {
                blocked => None::<String>, can_publish => true,
                author => "A", category => "C", library_hint => "",
                authors => Vec::<()>::new(), categories => Vec::<()>::new(),
                prompt => context! { label => "v0 (builtin)", path => "", builtin => true },
                posted => posted,
                figures => context! { can_write => false, hint => "", rows => Vec::<()>::new() },
                fixes => Vec::<()>::new(),
                article_path => "/tmp/blog/article.json",
                article => context! {
                    title => "T", h1 => "", slug => "go-to-market-update-2",
                    description => "d", description_length => 1, caption => "c",
                    keywords => Vec::<String>::new(), faq => Vec::<String>::new(),
                    keyword_targets => Vec::<String>::new(), long_description => "",
                    control_warning => "", summary => "", blocks => Vec::<()>::new(),
                },
            }
        };
        let draft = page("blog.html", ctx(minijinja::Value::from(())));
        assert!(
            draft.contains(r#"<input name="slug" value="go-to-market-update-2""#),
            "{draft}"
        );
        assert!(draft.contains("Save slug"), "{draft}");
        // Posted back under the key the limits module names the field by.
        assert!(
            draft.contains("send({type: 'saveBlogFields', fields: {slug:"),
            "{draft}"
        );

        let live = page(
            "blog.html",
            ctx(context! {
                url => "https://saagasolve.com/blog/go-to-market-update-2",
                admin_url => "https://cms/admin/x", slug => "go-to-market-update-2",
                published => true, created_at => "2026-09-16T12:00:00Z",
                warning => None::<String>,
            }),
        );
        assert!(!live.contains(r#"<input name="slug""#), "{live}");
        assert!(live.contains("change it in Strapi"), "{live}");
        assert!(live.contains("/blog/go-to-market-update-2"), "{live}");
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
        assert!(
            html.contains(r#"<input name="slug" value="why-watermarking-fails""#),
            "the slug is typed in place: {html}"
        );
        assert!(
            html.contains("140 to 160"),
            "the description target is stated"
        );
        // Both SEO fields the generator now writes. The FAQ especially: it
        // publishes as structured data, so it has to be readable before the
        // publish rather than after.
        assert!(
            html.contains("and what enforcement costs"),
            "the heading is shown"
        );
        assert!(
            html.contains("Not past the first retry."),
            "the FAQ is shown"
        );
        assert!(
            html.contains("primary · ai watermarking"),
            "the keyword brief is shown"
        );
        assert!(
            html.contains("Where it broke"),
            "the table of contents is shown"
        );
        assert!(html.contains("Ahmed Raza"), "the byline is visible");
        assert!(html.contains("Education"));
        assert!(html.contains("publishBlog"));
        assert!(html.contains("editBlogPrompt"));
        // Write and Preview come before Publish, in that order: it is the
        // order of commitment, and it is the whole reason a draft can be read
        // before a permanent slug exists.
        let write_at = html.find("writeBlog").expect("the write button");
        let preview_at = html.find("previewBlog").expect("the preview button");
        let publish_at = html.find("publishBlog").expect("the publish button");
        assert!(
            write_at < preview_at && preview_at < publish_at,
            "buttons out of order"
        );
        assert!(
            html.contains("Publish puts the draft below live as it stands"),
            "the pane says the draft on disk is what publishes"
        );
        // The pickers, and the row each one is currently on. Without `selected`
        // the dropdown would open on whatever is first and a glance at the tab
        // would report the wrong byline.
        assert!(
            html.contains("blogAuthor"),
            "the author picker is on the page"
        );
        assert!(html.contains("blogCategory"));
        assert!(html.contains("refreshBlogLibrary"));
        // The figure strip, and the two states a figure can be in. A captured
        // figure with no blurb has to say so rather than render an empty
        // caption, which reads as a blurb that came back blank.
        assert!(
            html.contains("captureFigure"),
            "the snip button is on the page"
        );
        assert!(html.contains("writeBlurbs"));
        assert!(html.contains("figure 02 · ch 03 · 1:24 · 1280 × 720"));
        assert!(html.contains("The retry storm that took the queue down."));
        assert!(html.contains("alt: A log filling with 429s"));
        assert!(
            html.contains("No blurb yet."),
            "the unwritten figure says so"
        );
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
                figures => no_figures(),
            },
        );
        // The pane rendered, rather than the error page standing in for it. In a
        // debug build that page dumps the referenced variables, so every string
        // below is in it too — which is how these two tests passed for weeks
        // without the `figures` block the template had started to need, and
        // failed the moment CI ran them in release.
        assert!(!html.contains("template error"), "{html}");
        // Displayed with `/` as `&#x2f;` — minijinja escapes it and the browser
        // decodes it back, so the host is what to assert on, not the whole URL.
        assert!(html.contains("saagasolve.com"));
        assert!(html.contains("cms.saagasolve.com"));
        assert!(html.contains("published"));
        assert!(
            html.contains("author could not be set"),
            "a warning is not swallowed"
        );
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
                figures => no_figures(),
            },
        );
        assert!(!html.contains("template error"), "{html}");
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
                    Word {
                        text: "hello".into(),
                        start: 0,
                        end: 500,
                        kept: true,
                    },
                    Word {
                        text: "um".into(),
                        start: 41_500,
                        end: 43_000,
                        kept: false,
                    },
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
        assert!(
            html.contains("3:12"),
            "another chapter's length is on the rail"
        );
        // The editor's own payloads.
        assert!(html.contains("applyEdit"));
        assert!(html.contains("resetEdit"));
        assert!(html.contains("saveEdit"));
        // The timeline's data, inlined rather than fetched.
        assert!(html.contains("[0,128,255]"), "peaks reach the pane");
        assert!(
            html.contains("[[0,41200],[43900,121000]]"),
            "so do the spans"
        );
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
        assert!(
            html.contains(r#"class="del""#),
            "a deletion is visibly marked"
        );
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
    /// Both links on the tab the upload happens on, each with its own copy and
    /// one copy for the pair — the pair is what gets pasted into a post.
    #[test]
    fn the_youtube_tab_offers_both_links_to_copy() {
        let html = page(
            "youtube.html",
            minijinja::context! {
                metadata => crate::publish::metadata::Metadata { title: String::new(), description: String::new() }, info => "",
                youtube => "https://www.youtube.com/watch?v=abc",
                short => "https://www.youtube.com/shorts/def",
                both => "https://www.youtube.com/watch?v=abc\nhttps://www.youtube.com/shorts/def",
            },
        );
        assert!(!html.contains("Template error"), "{html}");
        let card = &html[html.find("On YouTube").expect("the links card")..];
        // The autoescaper writes `/` as an entity inside the attribute; the
        // browser reads it back. The copy payload goes through `tojson`, which
        // leaves the URL as typed and sorts the keys.
        assert!(
            card.contains(r#"href="https:&#x2f;&#x2f;www.youtube.com&#x2f;watch?v=abc""#),
            "{card}"
        );
        assert!(
            card.contains(r#"href="https:&#x2f;&#x2f;www.youtube.com&#x2f;shorts&#x2f;def""#),
            "{card}"
        );
        assert!(
            card.contains(r#"{"text":"https://www.youtube.com/watch?v=abc","type":"copyText"}"#),
            "{card}"
        );
        assert!(
            card.contains(r#"{"text":"https://www.youtube.com/shorts/def","type":"copyText"}"#),
            "{card}"
        );
        assert!(card.contains("Copy both links"), "{card}");
        assert!(
            card.contains(r#"watch?v=abc\nhttps://www.youtube.com/shorts/def","type":"copyText"}"#),
            "{card}"
        );

        // Only the longform up: its link, a note for the Short, no pair to copy.
        let html = page(
            "youtube.html",
            minijinja::context! {
                metadata => crate::publish::metadata::Metadata { title: String::new(), description: String::new() }, info => "",
                youtube => "https://www.youtube.com/watch?v=abc",
                short => None::<String>, both => None::<String>,
            },
        );
        assert!(html.contains("Not uploaded yet"), "{html}");
        assert!(!html.contains("Copy both links"), "{html}");

        // Nothing up: no card at all.
        let html = page(
            "youtube.html",
            minijinja::context! {
                metadata => crate::publish::metadata::Metadata { title: String::new(), description: String::new() }, info => "",
                youtube => None::<String>, short => None::<String>, both => None::<String>,
            },
        );
        assert!(!html.contains("On YouTube"), "{html}");
    }

    #[test]
    fn youtube_details_are_editable_and_escaped() {
        let html = page(
            "youtube.html",
            minijinja::context! {
                metadata => crate::publish::metadata::Metadata {
                    title: "A <video>".into(), description: "Text & details".into(),
                }, info => "Ready to upload",
            },
        );
        assert!(html.contains("A &lt;video&gt;"));
        assert!(html.contains("Text &amp; details"));
        assert!(html.contains("saveYoutube"));
        assert!(html.contains("Save video details"));
        assert!(!html.contains("Template error"));
    }

    fn no_plan_status() -> minijinja::Value {
        context! { state => "none", text => "No plan yet — build one on the Plan tab.",
        working_title => "", chapters => 0 }
    }

    /// A fresh project: no plan, one empty version, nothing live — and each
    /// card says so rather than drawing an empty table or box.
    #[test]
    fn an_empty_project_pane_says_what_is_missing() {
        let html = page(
            "project.html",
            context! {
                title => "2026-09-26_10-00-00", folder => "2026-09-26_10-00-00",
                root => "/tmp/project", plan => no_plan_status(),
                versions => vec![context! { label => "v1", current => true, chapters => 0,
                                            rendered => false, stages => Vec::<String>::new() }],
                deck => None::<usize>, deck_from_plan => false, links => Vec::<()>::new(),
            },
        );
        assert!(!html.contains("template error"), "{html}");
        assert!(html.contains("No plan yet"));
        assert!(html.contains("No speaking notes yet"));
        assert!(html.contains("Nothing published yet"));
        assert!(html.contains("v1") && html.contains("not yet"));
    }

    #[test]
    fn the_project_pane_shows_an_approved_plan_versions_and_links() {
        let html = page(
            "project.html",
            context! {
                title => "Deploys <fast>", folder => "2026-09-26_10-00-00", root => "/tmp/project",
                plan => context! { state => "approved", text => "Plan 2 approved",
                                   working_title => "Ship it", chapters => 4 },
                versions => vec![
                    context! { label => "v1", current => false, chapters => 3, rendered => true,
                               stages => vec!["cut", "titles"] },
                    context! { label => "v2", current => true, chapters => 1, rendered => false,
                               stages => Vec::<String>::new() },
                ],
                deck => 4, deck_from_plan => true,
                links => vec![context! { name => "YouTube", url => "https://www.youtube.com/watch?v=abc" }],
            },
        );
        assert!(!html.contains("template error"), "{html}");
        assert!(html.contains("Deploys &lt;fast&gt;"), "the name is escaped");
        assert!(html.contains("Plan 2 approved") && html.contains(r#"class="tag ok">Approved"#));
        assert!(html.contains("written by the approved plan"));
        assert!(html.contains("cut, titles") && html.contains("Rendered"));
        assert!(html.contains("watch?v=abc"));
    }

    /// Un-approving leaves the plan's deck in place, and the summary must not
    /// go on calling it the approved plan's.
    #[test]
    fn a_plan_deck_with_nothing_approved_does_not_claim_an_approved_plan() {
        let html = page(
            "project.html",
            context! {
                title => "Deploys", folder => "2026-09-26_10-00-00", root => "/tmp/project",
                plan => context! { state => "draft", text => "Plan 2, not approved",
                                   working_title => "Ship it", chapters => 4 },
                versions => vec![context! { label => "v1", current => true, chapters => 0,
                                            rendered => false, stages => Vec::<String>::new() }],
                deck => 4, deck_from_plan => true, links => Vec::<()>::new(),
            },
        );
        assert!(!html.contains("template error"), "{html}");
        assert!(!html.contains("written by the approved plan"), "{html}");
        assert!(html.contains("a plan that is no longer approved"), "{html}");
    }

    fn plan_chapter(n: usize, kind: &str, label: &str, title: &str) -> minijinja::Value {
        context! {
            n => n, kind => kind, kind_label => label, title => title, goal => "", points => vec!["a point"],
            verbatim => None::<String>, cues => Vec::<String>::new(), show => "", layout => None::<String>,
            layout_key => "", est => None::<String>,
        }
    }

    fn a_plan(approved: bool) -> minijinja::Value {
        context! {
            n => 2, label => if approved { "Plan 2 (approved)" } else { "Plan 2" }, approved => approved,
            working_title => "Ship it", audience => "Engineers", promise => "A faster deploy",
            hook_line => "Your deploy takes an hour.", hook_angle => "",
            outline => vec!["The hour", "The fix"],
            chapters => vec![
                plan_chapter(1, "hook", "Hook", "The hour"),
                context! {
                    n => 2, kind => "body", kind_label => "Body", title => "The cache",
                    goal => "Why it misses", points => vec!["keys change"],
                    verbatim => None::<String>, cues => vec!["pause"], show => "the build log",
                    layout => "Split", layout_key => "split", est => "1:30",
                },
                plan_chapter(3, "cta", "Call to action", "Next time"),
            ],
            cta_line => "Subscribe for part two.", cta_placement => "",
            instructions => vec!["Have the dashboard open"],
            refined_from => "Plan 1", refine_note => "tighter", sources => vec!["take 01"],
            created_at => "", est_total => "2:10",
        }
    }

    fn layouts() -> Vec<minijinja::Value> {
        vec![
            context! { key => "talking-head", label => "Talking Head" },
            context! { key => "split", label => "Split" },
            context! { key => "outline", label => "Outline" },
        ]
    }

    fn plan_versions(approved: bool) -> Vec<minijinja::Value> {
        vec![
            context! { n => 1, label => "Plan 1", selected => false, approved => false },
            context! { n => 2, label => "Plan 2", selected => true, approved => approved },
        ]
    }

    /// Nothing recorded, typed or built: every section says what fills it,
    /// Record idea and Build plan are ready, and Plan from rehearsal is off
    /// until this version has chapters to plan from.
    #[test]
    fn an_empty_plan_pane_says_what_to_do_first() {
        let html = page(
            "plan.html",
            context! {
                root => "/tmp/project", instructions => "", typed => "", takes => Vec::<()>::new(),
                recording => None::<u32>, recording_label => None::<String>, building => false,
                versions => Vec::<()>::new(), plan => None::<()>, unreadable => None::<String>,
                locked => None::<String>, can_rehearse => false, layouts => layouts(),
            },
        );
        assert!(!html.contains("template error"), "{html}");
        assert!(html.contains("No idea takes yet"));
        assert!(
            html.contains("Audience, tone, length"),
            "the instructions box says what goes in it"
        );
        assert!(html.contains("No plan yet"));
        assert!(!html.contains("Refine Plan"), "nothing to refine yet");
        let button = |label: &str| {
            let at = html
                .find(label)
                .unwrap_or_else(|| panic!("{label} is drawn"));
            html[html[..at].rfind("<button").unwrap()..at].to_string()
        };
        assert!(!button("Record idea").contains("disabled"));
        assert!(!button("Build plan").contains("disabled"));
        assert!(button("Plan from rehearsal").contains("disabled"));
        assert!(html.contains("needs chapters recorded in this version"));
        assert!(
            html.contains(r#""root":"/tmp/project""#),
            "messages carry the project"
        );
    }

    /// The author's own words go into the page as text, never as markup.
    #[test]
    fn the_plan_pane_escapes_author_text() {
        let html = page(
            "plan.html",
            context! {
                instructions => "</p><script>bad()</script>", typed => "<b>typed</b>",
                takes => vec![context! { label => "Take 01", state => "ready",
                                         text => "<img src=x onerror=bad()>", why => "" }],
                versions => Vec::<()>::new(), plan => None::<()>, unreadable => None::<String>,
            },
        );
        assert!(!html.contains("template error"), "{html}");
        assert!(!html.contains("<script>bad()</script>"));
        assert!(!html.contains("<b>typed</b>"));
        assert!(!html.contains("<img src=x"));
        // Shown as the words typed, not dropped: escaped, not stripped.
        assert!(html.contains("&lt;script&gt;bad()"));
    }

    #[test]
    fn an_approved_plan_shows_as_approved_and_locked() {
        let html = page(
            "plan.html",
            context! {
                instructions => "For engineers.", typed => "",
                takes => vec![
                    context! { label => "Take 01", state => "ready", text => "Deploys are slow.", why => "" },
                    context! { label => "Take 02", state => "nothing", text => "", why => "silent" },
                ],
                versions => plan_versions(true), plan => a_plan(true), unreadable => None::<String>,
                root => "/tmp/project", layouts => layouts(),
                locked => "Plan 2 is approved and locked — un-approve it to edit it, refine it or build again.",
            },
        );
        assert!(!html.contains("template error"), "{html}");
        assert!(html.contains("Plan 2 (approved)"));
        assert!(html.contains("Approved — locked"));
        assert!(html.contains("Plan 2 · approved"));
        assert!(html.contains("Un-approve"));
        assert!(html.contains("Deploys are slow.") && html.contains("silent"));
        // The whole plan is on the page, in the order it is recorded.
        let hook = html.find("Your deploy takes an hour.").unwrap();
        let body = html.find("The cache").unwrap();
        let cta = html.find("Subscribe for part two.").unwrap();
        assert!(hook < body && body < cta);
        for part in [
            "Why it misses",
            "keys change",
            r#"value="the build log""#,
            r#"<option value="split" selected>Split</option>"#,
            r#"value="1:30""#,
            "Call to action",
            "Have the dashboard open",
            "Refined from Plan 1",
            "About 2:10 long",
        ] {
            assert!(html.contains(part), "missing {part}");
        }

        let draft = page(
            "plan.html",
            context! {
                instructions => "", typed => "", takes => Vec::<()>::new(),
                versions => plan_versions(false), plan => a_plan(false), unreadable => None::<String>,
                root => "/tmp/project", layouts => layouts(), locked => None::<String>,
            },
        );
        // Locked: every box read-only, and why the build buttons are off.
        assert!(html.contains("readonly"));
        assert!(html.contains("un-approve it to edit it"));
        assert!(!draft.contains("readonly"), "a draft is editable");
        assert!(!draft.contains("Approved — locked"));
        assert!(draft.contains("Not approved") && draft.contains(">Approve<"));
    }

    /// Plan versions are "Plan N" everywhere on the page. "vN" is what the
    /// recording versions are called, and one word for two things is how a
    /// take gets recorded against the wrong one.
    #[test]
    fn plan_versions_read_plan_n_never_v_n() {
        let html = page(
            "plan.html",
            context! {
                instructions => "", typed => "", takes => Vec::<()>::new(),
                versions => plan_versions(false), plan => a_plan(false), unreadable => None::<String>,
                root => "/tmp/project", layouts => layouts(), locked => None::<String>,
            },
        );
        assert!(!html.contains("template error"), "{html}");
        assert!(html.contains("Plan 1") && html.contains("Plan 2"));
        let text = html.split("<script>").next().unwrap();
        for n in 1..=3 {
            assert!(!text.contains(&format!(">v{n}")), "a plan reads as v{n}");
            assert!(!text.contains(&format!(" v{n}")), "a plan reads as v{n}");
        }
    }

    /// The team footer box, and the line that says a save now reaches the
    /// video on YouTube — only once it is up.
    #[test]
    fn the_youtube_tab_edits_the_team_footer_and_says_a_save_updates_youtube() {
        let html = page(
            "youtube.html",
            minijinja::context! {
                metadata => crate::publish::metadata::Metadata {
                    title: "A video".into(), description: "Details".into(),
                }, info => "",
                youtube => "https://youtu.be/abc",
                team => minijinja::context! {
                    footer => "Start here:\n{links} <b>", status => "2 team links.", loaded => true,
                },
            },
        );
        assert!(!html.contains("Template error"), "{html}");
        assert!(html.contains("also changes its title and description there"));
        assert!(html.contains("Team description footer"));
        assert!(html.contains("Start here:\n{links} &lt;b&gt;"));
        assert!(html.contains("2 team links."));
        assert!(html.contains("saveTeamFooter"));
        let at = html.find("Save for team").unwrap();
        let button = &html[html[..at].rfind("<button").unwrap()..at];
        assert!(!button.contains("disabled"), "{button}");

        let not_up = page(
            "youtube.html",
            minijinja::context! {
                metadata => crate::publish::metadata::Metadata {
                    title: "A video".into(), description: "Details".into(),
                }, info => "",
                team => minijinja::context! { footer => "", status => "Loading…", loaded => false },
            },
        );
        assert!(!not_up.contains("also changes its title"));
        let at = not_up.find("Save for team").unwrap();
        assert!(not_up[not_up[..at].rfind("<button").unwrap()..at].contains("disabled"));
    }

    /// The Summary card: the summary, takeaways and chapter lines, escaped;
    /// a copy button carrying the plain text; and the states before and after.
    #[test]
    fn the_summary_card_shows_what_the_final_video_says() {
        let summary = crate::summary::Summary {
            summary: "How deploys <got> fast.".into(),
            takeaways: vec!["Cache the layers".into()],
            chapters: vec![crate::summary::ChapterSummary {
                n: 2,
                title: "The fix".into(),
                summary: "Why the cache mattered.".into(),
            }],
            transcript_hash: "h".into(),
            model: "m".into(),
            created_at: String::new(),
        };
        let view = |summary: Option<&crate::summary::Summary>, stale: bool, ready: bool| {
            minijinja::context! {
                summary => summary.cloned(), text => summary.map(crate::summary::plain_text).unwrap_or_default(),
                stale => stale, busy => false, ready => ready,
            }
        };
        let pane = |summary: minijinja::Value| {
            page(
                "video.html",
                context! {
                    brief => crate::video_brief::Brief::default(),
                    root => "/tmp/p", busy => false, model => "m",
                    thumbnail => no_thumbnail(), review => no_clips(), youtube => None::<String>,
                    figures => no_figures(), summary => summary,
                },
            )
        };
        let html = pane(view(Some(&summary), false, true));
        assert!(!html.to_lowercase().contains("template error"), "{html}");
        assert!(html.contains("How deploys &lt;got&gt; fast."));
        assert!(html.contains("Cache the layers"));
        assert!(html.contains(r#"<li value="2"><b>The fix</b> — Why the cache mattered."#));
        assert!(html.contains("Summarize again"));
        assert!(html.contains("Copy summary"));
        assert!(html.contains("summarizeVideo"));

        assert!(pane(view(Some(&summary), true, true)).contains("Out of date"));

        let none = pane(view(None, false, false));
        assert!(none.contains("Nothing to summarize yet"));
        assert!(!none.contains("Copy summary"));
    }

    /// The plan as pages: an overview, a page per chapter with its line and
    /// points, and what to have ready — with the dots and the page count to
    /// turn them, and the editable boxes one tab away.
    #[test]
    fn the_plan_reads_as_pages_like_the_speaking_notes() {
        let html = page(
            "plan.html",
            context! {
                instructions => "", typed => "", takes => Vec::<()>::new(),
                versions => plan_versions(false), plan => a_plan(false), unreadable => None::<String>,
                root => "/tmp/project", layouts => layouts(), locked => None::<String>,
            },
        );
        assert!(!html.contains("template error"), "{html}");
        assert_eq!(
            html.matches(r#"<section class="page"#).count(),
            5,
            "overview, 3 chapters, ready"
        );
        assert!(html.contains("Chapter 2 of 3 · Body"));
        assert!(html.contains("<h1>The cache</h1>"));
        assert!(html.contains("On screen: <b>the build log</b>"));
        assert!(html.contains("Split · ~1:30"));
        assert!(html.contains("<span class=\"label\">Open with</span>Your deploy takes an hour."));
        assert!(html.contains("<span class=\"label\">The ask</span>Subscribe for part two."));
        assert!(html.contains("Have the dashboard open"));
        assert_eq!(html.matches(r#"class="dot""#).count(), 5);
        assert!(html.contains(r#"id="page-count">1 / 5<"#));
        assert!(html.contains(r#"id="plan-edit" data-view-panel="edit" hidden"#));
        assert!(
            html.contains(r#"data-field="ch2.title""#),
            "the boxes are still there to edit"
        );
    }

    /// The Critique card: the direction box, the button, each chapter's keep
    /// and change, the reorganization, and the plan it wrote.
    #[test]
    fn the_critique_card_shows_each_chapter_and_the_plan_it_wrote() {
        let critique = crate::critique::Critique {
            overall: "Strong <hook>, slow middle.".into(),
            chapters: vec![crate::critique::ChapterCritique {
                n: 2,
                title: "The cache".into(),
                worked: "The number lands.".into(),
                fix: "Cut the aside.".into(),
            }],
            reorganize: "Merge 2 into 1.".into(),
            direction: "Lead with the demo".into(),
            plan_written: Some(4),
            ..crate::critique::Critique::default()
        };
        let pane = |critique: minijinja::Value| {
            page(
                "video.html",
                context! {
                    brief => crate::video_brief::Brief::default(),
                    root => "/tmp/p", busy => false, model => "m",
                    thumbnail => no_thumbnail(), review => no_clips(), youtube => None::<String>,
                    figures => no_figures(), critique => critique,
                },
            )
        };
        let html = pane(context! {
            critique => critique.clone(), text => crate::critique::plain_text(&critique),
            busy => false, blocked => None::<String>,
        });
        assert!(!html.to_lowercase().contains("template error"), "{html}");
        assert!(html.contains("Critique &amp; next take"));
        assert!(html.contains("Strong &lt;hook&gt;, slow middle."));
        assert!(
            html.contains(">Lead with the demo</textarea>"),
            "the last direction is kept"
        );
        assert!(html.contains("Chapter 2 — The cache"));
        assert!(html.contains("<span class=\"keep\">Keep</span> The number lands."));
        assert!(html.contains("<span class=\"change\">Change</span> Cut the aside."));
        assert!(html.contains("Merge 2 into 1."));
        assert!(html.contains("Plan 4 was written from this and is now the speaking notes"));
        assert!(html.contains("critiqueTake"));

        let blocked = pane(context! {
            critique => None::<()>, text => "", busy => false,
            blocked => "Record a take first — there are no chapters to critique.",
        });
        assert!(blocked.contains("Record a take first"));
        let at = blocked.find("Critique and rewrite the notes").unwrap();
        assert!(blocked[blocked[..at].rfind("<button").unwrap()..at].contains("disabled"));
    }
}
