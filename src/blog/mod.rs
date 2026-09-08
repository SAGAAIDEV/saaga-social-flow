//! Publishing the longform to the video blog at `/blog` on saagasolve.com.
//!
//! A distribution stage, unlike [`crate::substack`]: this one writes a public
//! page over the network, so it carries a ledger and a gate. It is modelled on
//! [`crate::publish`] rather than on [`crate::schedule`] — one artifact, one
//! POST, its own client — because Buffer's queue has nothing to do with a CMS.
//!
//! **The longform only.** The vertical cuts are Buffer's; the chapters here are
//! chapters *within* the one video, not the shorts cut from it. Nothing in this
//! stage reads [`crate::distribute`] or needs S3: the video is already on
//! YouTube and the thumbnail goes straight to Strapi's own upload endpoint.
//!
//! ## Three steps, not one
//!
//! [`spawn_write`] drafts `article.json` locally, [`preview`] renders it as a
//! page you can read, and [`spawn_publish`] uploads it. The split exists
//! because the last step is the only irreversible one: it mints a permanent
//! public slug for prose a model wrote.
//!
//! The load-bearing consequence is that **the publish keeps the draft it
//! finds**. Regenerating at publish time would put a different article up from
//! the one that was just read — the model does not write the same piece twice —
//! which would make previewing it theatre. Write Article is therefore the only
//! way to replace a draft, and it overwrites without asking.
//!
//! ## The gate
//!
//! The gate depends on [`crate::publish`] rather than on the render, and that is
//! the load-bearing prerequisite: the upload ledger is where the YouTube id
//! comes from, and the `video` component is now sent with `provider: youtube`
//! and that id explicitly. A post built before the upload would have no id to
//! name, and the landing renders no player at all without one — a page that is
//! wrong in the one way nothing on it announces.

use std::path::PathBuf;
use std::sync::mpsc::Sender;
use std::thread;

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};

pub mod chapters;
pub mod compat;
pub mod components;
pub mod figures;
pub mod generate;
pub mod library;
pub mod og;
pub mod pane;
pub mod payload;
pub mod preview;
pub mod schema;
pub mod strapi;
pub mod vertical;

use crate::session::Session;

/// Append-only, beside `youtube.jsonl` and for the same reason: Strapi will
/// happily take the same article twice and give the second one `slug-2`, and
/// nothing here would ever clean that up.
pub const POSTS_JSONL: &str = "blog.jsonl";

/// The rendered longform, relative to the render directory.
const LONGFORM: &str = "horizontal/longform.mp4";

/// Who the post is bylined to, and where it files.
///
/// Both are picked on the Blog tab from the CMS's own lists and remembered in
/// [`crate::config`], rather than being names compiled in here. Names were the
/// original design and were wrong: the byline default named someone who is not
/// in the authors collection, so every post would have gone up with no author
/// and one line of log.
///
/// An id resolves with no lookup at all, which is the other half of the point:
/// a relation set by id cannot silently match the wrong row, and the live CMS
/// has two authors sharing a name.
///
/// `BLOG_AUTHOR` / `BLOG_CATEGORY` remain as escape hatches for a scripted run
/// with no config, and are resolved by name against the live API at publish
/// time.
#[derive(Debug, Clone, PartialEq)]
pub struct Chosen {
    /// The relation id, when one is settled. `None` means either the name below
    /// has to be looked up live, or that nothing is chosen at all.
    pub id: Option<i64>,
    /// A name to look up, when it came from the environment.
    pub name: Option<String>,
    /// What the pane shows.
    pub label: String,
}

impl Chosen {
    /// Whether this will put a relation on the post at all — what the gate asks.
    pub fn is_set(&self) -> bool {
        self.id.is_some() || self.name.is_some()
    }

    fn resolve(
        entries: &[library::Entry],
        id: Option<i64>,
        remembered: Option<String>,
        env_key: &str,
    ) -> Chosen {
        // The environment wins, so a scripted run can override a picked value
        // without editing a config file it does not know about.
        if let Some(name) = env(env_key) {
            // Matched against the cache when it can be, which does two things:
            // the pane highlights the row the override names, and the publish
            // needs no lookup. An unmatched name still travels, to be resolved
            // live — the cache is not the authority on what exists.
            return match library::Library::by_name(entries, &name) {
                Some(entry) => Chosen {
                    id: Some(entry.id),
                    name: None,
                    label: format!("{} (from {env_key})", entry.name),
                },
                None => Chosen {
                    id: None,
                    label: format!("{name} (from {env_key})"),
                    name: Some(name),
                },
            };
        }
        match id {
            Some(id) => Chosen {
                id: Some(id),
                name: None,
                // The remembered name is only for display, so a cache that has
                // never been fetched still shows something recognisable.
                label: remembered.unwrap_or_else(|| format!("#{id}")),
            },
            None => Chosen {
                id: None,
                name: None,
                label: "none chosen".to_string(),
            },
        }
    }
}

pub fn chosen_author(cfg: &crate::config::Config, library: &library::Library) -> Chosen {
    Chosen::resolve(
        &library.authors,
        cfg.blog_author_id,
        cfg.blog_author_name.clone(),
        "BLOG_AUTHOR",
    )
}

/// The one taxonomy a video post carries.
///
/// Shared with `blog-articles`, and what `/blog`, `/blog/category/[name]`,
/// search and the breadcrumbs all read. It used to be `educationCategory` here,
/// pointed at an `education-categories` collection that has since been retired —
/// which meant every post this pipeline made named a dead relation and carried
/// no live taxonomy at all.
pub fn chosen_category(cfg: &crate::config::Config, library: &library::Library) -> Chosen {
    Chosen::resolve(
        &library.categories,
        cfg.blog_category_id,
        cfg.blog_category_name.clone(),
        "BLOG_CATEGORY",
    )
}

fn env(key: &str) -> Option<String> {
    std::env::var(key)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Post {
    pub document_id: String,
    pub slug: String,
    pub url: String,
    pub admin_url: String,
    /// The YouTube video this post is about. The dedupe key: a re-upload is
    /// legitimately a new post, a second press on the same one is not.
    pub video_id: String,
    pub published: bool,
    pub created_at: String,
    /// Kept on the row rather than only logged — a post that went up with a
    /// relation missing is worth being able to find later.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub warning: Option<String>,
}

pub enum BlogEvent {
    Status(String),
    Ready(Post),
    /// The author and category lists came back. Carries the counts only — the
    /// pane reads the lists themselves from the cache on disk, so there is one
    /// source of truth for what is on screen.
    LibraryReady {
        authors: usize,
        categories: usize,
    },
    /// A draft was written to disk and nothing was uploaded. Distinct from
    /// [`BlogEvent::Ready`], which means a live CMS entry exists — nothing is
    /// reversible after that one, and everything is after this one.
    Written(PathBuf),
    /// Terminal failure. Distinct from a `Status` with the same words: the app
    /// has to know the thread is gone so it can re-enable the button.
    Failed(String),
}

/// Pulls the author and category lists from Strapi into the cache.
///
/// On a thread for the same reason the publish is: two round trips to a CMS over
/// the network, and the Blog tab is repainted from the UI thread.
pub fn spawn_refresh_library(tx: Sender<BlogEvent>) {
    let unstarted = tx.clone();
    if let Err(err) = thread::Builder::new()
        .name("blog-library".into())
        .spawn(move || {
            let _ = tx.send(BlogEvent::Status("Reading the lists from Strapi…".into()));
            match library::refresh() {
                Ok(library) => {
                    let _ = tx.send(BlogEvent::LibraryReady {
                        authors: library.authors.len(),
                        categories: library.categories.len(),
                    });
                }
                Err(err) => {
                    eprintln!("stream-recorder: strapi library refresh failed: {err:#}");
                    let _ = tx.send(BlogEvent::Failed(format!("Could not read Strapi: {err:#}")));
                }
            }
        })
    {
        let _ = unstarted.send(BlogEvent::Failed(format!(
            "Could not start the Strapi read: {err}"
        )));
    }
}

/// Remembers the picked author. `id` is a string because it arrives from the
/// pane's dropdown; an unparseable one clears the choice rather than erroring,
/// which is also how the "none" option works.
pub fn select_author(id: &str) -> Result<String> {
    let library = library::load();
    let entry = id.parse::<i64>().ok().and_then(|id| library.author(id));
    let mut cfg = crate::config::load();
    cfg.blog_author_id = entry.map(|entry| entry.id);
    cfg.blog_author_name = entry.map(|entry| entry.name.clone());
    crate::config::save(&cfg).context("saving the blog author")?;
    Ok(match entry {
        Some(entry) => format!("Posting as {}.", entry.name),
        None => "No author — the post will have no byline.".to_string(),
    })
}

pub fn select_category(id: &str) -> Result<String> {
    let library = library::load();
    let entry = id.parse::<i64>().ok().and_then(|id| library.category(id));
    let mut cfg = crate::config::load();
    cfg.blog_category_id = entry.map(|entry| entry.id);
    cfg.blog_category_name = entry.map(|entry| entry.name.clone());
    crate::config::save(&cfg).context("saving the blog category")?;
    Ok(match entry {
        Some(entry) => format!("Filing under {}.", entry.name),
        None => "No category — the post will not appear under any filter.".to_string(),
    })
}

/// Writes the article to `{project}/blog/article.json` and stops there.
///
/// The half of [`spawn_publish`] that costs nothing to undo. It exists so the
/// piece can be read, and the figures' placement checked, before a permanent
/// public URL is created — and so a prompt can be tuned and the draft rewritten
/// as many times as it takes. Nothing here touches Strapi.
///
/// Overwrites whatever draft is on disk, deliberately: the publish *keeps* the
/// draft it finds, so this is the only way to replace one, and a button that
/// refused to would leave no way to rewrite.
pub fn spawn_write(
    session: Session,
    model: String,
    provider: Option<String>,
    tx: Sender<BlogEvent>,
) {
    let unstarted = tx.clone();
    if let Err(err) = thread::Builder::new()
        .name("blog-write".into())
        .spawn(
            move || match write_draft(&session, &model, provider.as_deref(), &tx) {
                Ok(path) => {
                    eprintln!("stream-recorder: blog draft → {}", path.display());
                    let _ = tx.send(BlogEvent::Written(path));
                }
                Err(err) => {
                    eprintln!("stream-recorder: blog draft failed: {err:#}");
                    let _ = tx.send(BlogEvent::Failed(format!("Draft failed: {err:#}")));
                }
            },
        )
    {
        eprintln!("stream-recorder: could not start the blog draft: {err}");
        let _ = unstarted.send(BlogEvent::Failed(format!(
            "Could not start the blog draft: {err}"
        )));
    }
}

fn write_draft(
    session: &Session,
    model: &str,
    provider: Option<&str>,
    tx: &Sender<BlogEvent>,
) -> Result<PathBuf> {
    let longform = crate::longform::build(session);
    if longform.chapters.is_empty() {
        bail!(
            "no transcribed chapters in {} — record one and let it transcribe first",
            session.dir.display()
        );
    }
    let offers = figures::offers(session);
    let requested = components::requested(session);
    let _ = tx.send(BlogEvent::Status(drafting_status(
        longform.chapters.len(),
        offers.len(),
        figures::unblurbed(session),
        model,
        generate::stale_preamble_note(Some(&session.root), &offers, &requested).as_deref(),
    )));
    let (article, step) = generate::generate_article(
        &longform,
        session.version,
        model,
        provider,
        Some(&session.root),
        &offers,
        &requested,
    )?;
    let dir = session.blog_dir();
    let path = schema::save(&dir, &article)?;
    crate::agent::trace::write_step(&dir, &session.root, &step)?;
    Ok(path)
}

/// The line a draft starts with: what it is written from, what it may place,
/// and what it is leaving out — said where the button was pressed rather than
/// left to the log.
///
/// The two things it can leave out are the two ways a captured figure fails to
/// reach the article. A figure with no blurb is never offered — see
/// [`figures::offers`] — and a standing prompt that predates figures never
/// places one — see [`generate::stale_preamble`]. Both used to be silent, and
/// the first anyone knew was an article with no pictures in it.
fn drafting_status(
    chapters: usize,
    offered: usize,
    unblurbed: usize,
    model: &str,
    stale_prompt: Option<&str>,
) -> String {
    let mut line = format!("Writing the article from {chapters} chapter(s)");
    if offered > 0 {
        line.push_str(&format!(" with {offered} figure(s) to place"));
    }
    line.push_str(&format!(" via {model}…"));
    if unblurbed > 0 {
        line.push_str(&format!(
            " {unblurbed} figure(s) have no blurb and are left out — press Write Blurbs, then \
             Write Article again."
        ));
    }
    if let Some(note) = stale_prompt {
        line.push_str(&format!(" Note: {note}."));
    }
    line
}

/// What a preview left on disk.
pub struct Previewed {
    /// The rendered page, which is also what was opened.
    pub page: PathBuf,
    /// The create body, when it could be built. `None` is not a failure: see
    /// [`preview`].
    pub body: Option<PathBuf>,
}

/// Renders the draft on disk as a local page, writes the body Strapi would be
/// sent beside it, and opens the page.
///
/// Synchronous: it reads a few files and writes two, with no network at all, so
/// a thread would only add a way for the click and the page to disagree about
/// which draft is being looked at.
///
/// The two outputs answer different questions and neither replaces the other.
/// The page is how the prose and the figures' placement get read; the JSON is
/// how a flattened table, a dropped figure or a missing relation gets caught,
/// and it is what the landing repo's `npm run content:validate` takes.
pub fn preview(session: &Session) -> Result<Previewed> {
    let dir = session.blog_dir();
    let article = schema::load(&dir).context("no draft to preview — press Write Article first")?;
    // Best effort, and deliberately not fatal: the JSON is a second opinion on
    // the same draft, and a figure whose file has been moved should not cost
    // you the page you asked to read.
    let body = match write_payload(session) {
        Ok(path) => Some(path),
        Err(err) => {
            eprintln!("stream-recorder: could not write the video post: {err:#}");
            None
        }
    };
    let page = preview::write(&dir, &article, &crate::figure::load(&session.root))?;
    let opened = std::process::Command::new("open").arg(&page).status();
    match opened {
        Ok(status) if status.success() => {}
        // The file is written either way, so the path is the useful thing to
        // report rather than a failure: it can be opened by hand.
        Ok(status) => eprintln!("stream-recorder: `open` exited {status}"),
        Err(err) => eprintln!("stream-recorder: could not open the preview: {err}"),
    }
    Ok(Previewed { page, body })
}

/// Builds the create body from what is on disk and writes it out, touching
/// nothing.
///
/// The counterpart to [`preview`]. That one renders the draft as a page for a
/// human; this one renders it as the JSON Strapi is sent — which is what the
/// landing repo's `npm run content:validate` reads, and the only way to see a
/// flattened table or a dropped figure before the page is permanent.
///
/// Two fields cannot be honest before an upload, and both say so rather than
/// guessing. `thumbnail` is a media id that exists only afterwards, so it is
/// omitted; each figure's `src` is the local file path, which no CMS URL could
/// be mistaken for. Everything else is what the publish would send.
pub fn write_payload(session: &Session) -> Result<PathBuf> {
    let dir = session.blog_dir();
    let article =
        schema::load(&dir).context("no draft to write out — press Write Article first")?;
    let figures = figures::local(session, &article.figures())?;
    let longform = crate::longform::build(session);
    let timeline = chapters::build(session, &longform);

    // The upload is the gate on a real publish, but not on this: seeing the
    // body before the video is up is most of the reason to look at it.
    let upload = crate::publish::load(session).into_iter().next_back();
    let duration = crate::edit::cut::probe_duration_seconds(&session.render_dir().join(LONGFORM))
        .unwrap_or_default();

    let cfg = crate::config::load();
    let cached = library::load();
    let post = payload::NewVideoPost {
        date: payload::today(),
        video_url: upload
            .as_ref()
            .map(|row| row.url.clone())
            .unwrap_or_default(),
        video_id: upload.as_ref().map(|row| row.video_id.clone()),
        duration: duration.round().max(0.0) as u32,
        thumbnail_id: None,
        thumbnail_vertical_id: None,
        // Both halves of the vertical pair are upload results, so a dry run has
        // neither. What it can still show is whether the cut is even there —
        // see the note this leaves on the way out.
        vertical: None,
        // An upload result, like the vertical pair — a dry run has none.
        og_image_id: None,
        // The text of it is knowable without a call; only its image is not.
        cta: cta(&cfg, None),
        embeds: components::built(session),
        chapters: timeline.chapters,
        transcript: timeline.transcript,
        // Ids only. A name from the environment is resolved against the live
        // API on publish, and this path makes no calls — so an override that
        // the cache cannot match is left off rather than looked up.
        author_id: chosen_author(&cfg, &cached).id,
        category_id: chosen_category(&cfg, &cached).id,
        figures,
        article,
    };
    // The one thing the dry run knows and cannot show. Said out loud rather
    // than left to be inferred from a field that is missing for two different
    // reasons — no cut, or a cut not yet uploaded.
    if session.render_dir().join("vertical/longform.mp4").is_file() {
        eprintln!(
            "stream-recorder: a vertical cut is rendered; the publish will upload it as \
             videoVertical (this body cannot, having uploaded nothing)"
        );
    }
    payload::save(&dir, &post.body())
}

pub fn spawn_publish(
    session: Session,
    model: String,
    provider: Option<String>,
    tx: Sender<BlogEvent>,
) {
    let unstarted = tx.clone();
    if let Err(err) = thread::Builder::new()
        .name("blog-publish".into())
        .spawn(
            move || match run(&session, &model, provider.as_deref(), &tx) {
                Ok(post) => {
                    eprintln!("stream-recorder: blog → {}", post.url);
                    let _ = tx.send(BlogEvent::Ready(post));
                }
                Err(err) => {
                    eprintln!("stream-recorder: blog publish failed: {err:#}");
                    let _ = tx.send(BlogEvent::Failed(format!("Blog failed: {err:#}")));
                }
            },
        )
    {
        eprintln!("stream-recorder: could not start the blog job: {err}");
        let _ = unstarted.send(BlogEvent::Failed(format!(
            "Could not start the blog job: {err}"
        )));
    }
}

fn run(
    session: &Session,
    model: &str,
    provider: Option<&str>,
    tx: &Sender<BlogEvent>,
) -> Result<Post> {
    let status = |msg: String| {
        let _ = tx.send(BlogEvent::Status(msg));
    };

    // Everything that can be refused for free is refused before the first call:
    // an LLM request and a thumbnail upload are both wasted if the gate below
    // was going to stop this anyway.
    let upload = crate::publish::load(session)
        .into_iter()
        .next_back()
        .context("the longform is not on YouTube yet — upload it on the YouTube tab first")?;
    if let Some(prior) = posted(session, &upload.video_id) {
        bail!(
            "this video is already on the blog at {} — edit it in Strapi, or re-upload to post again",
            prior.url
        );
    }

    let video = session.render_dir().join(LONGFORM);
    if !video.is_file() {
        bail!("no longform rendered yet — run Render first");
    }
    let duration = crate::edit::cut::probe_duration_seconds(&video)
        .with_context(|| format!("probing the length of {}", video.display()))?;

    let artwork = crate::card::assets::ready(&session.root)?;
    let images = artwork.snapshot(&session.root, &session.blog_dir().join("artwork"))?;
    let thumbnail = images.join("horizontal.jpg");
    let portrait = images.join("vertical.jpg");
    let og_path = images.join("og.jpg");

    status("Reading the longform's transcripts…".into());
    let longform = crate::longform::build(session);
    if longform.chapters.is_empty() {
        bail!(
            "no transcribed chapters in {} — record one and let it transcribe first",
            session.dir.display()
        );
    }
    let timeline = chapters::build(session, &longform);

    let client = strapi::Strapi::from_env()?;

    // Picked ids need no call at all. A name only appears here when it came from
    // the environment, and that is the one case worth a lookup.
    let cfg = crate::config::load();
    let cached = library::load();
    let chosen_author = chosen_author(&cfg, &cached);
    let chosen_category = chosen_category(&cfg, &cached);
    if chosen_author.name.is_some() || chosen_category.name.is_some() {
        status("Looking up the author and category…".into());
    }
    let author = match (chosen_author.id, chosen_author.name.as_deref()) {
        (Some(id), _) => Some(id),
        (None, Some(name)) => client.find_author(name),
        (None, None) => None,
    };
    let category = match (chosen_category.id, chosen_category.name.as_deref()) {
        (Some(id), _) => Some(id),
        (None, Some(name)) => client.find_category(name),
        (None, None) => None,
    };

    // A named override that did not resolve is worth stopping for. It is an
    // explicit instruction that did not take effect, and the post it would make
    // is one with no byline — which is exactly the state this work exists to fix.
    if author.is_none() && chosen_author.name.is_some() {
        bail!(
            "no author named {} in Strapi — pick one on the Blog tab, or fix BLOG_AUTHOR",
            chosen_author.label
        );
    }
    if category.is_none() {
        eprintln!(
            "stream-recorder: no category set — the post will not appear under any filter on \
             /blog. Pick one on the Blog tab."
        );
    }

    // The draft on disk wins, and that is the whole point of there being a
    // preview. Regenerating here would publish a different article from the one
    // that was just read and approved — the model does not write the same piece
    // twice — which would make previewing it theatre. Write Article is how a
    // draft is replaced.
    let dir = session.blog_dir();
    let article = match schema::load(&dir) {
        Ok(existing) => {
            status("Publishing the article already on disk…".into());
            existing
        }
        Err(_) => {
            // Written last of the cheap-to-refuse steps, because it is the
            // expensive one: a byline that does not resolve should cost nothing.
            let offers = figures::offers(session);
            let requested = components::requested(session);
            status(drafting_status(
                longform.chapters.len(),
                offers.len(),
                figures::unblurbed(session),
                model,
                generate::stale_preamble_note(Some(&session.root), &offers, &requested).as_deref(),
            ));
            let (article, step) = generate::generate_article(
                &longform,
                session.version,
                model,
                provider,
                Some(&session.root),
                &offers,
                &requested,
            )?;
            schema::save(&dir, &article)?;
            crate::agent::trace::write_step(&dir, &session.root, &step)?;
            article
        }
    };

    // Refused before any upload, and refused rather than dropped for the reason
    // `figures::upload` gives: the article was read and approved *with* that
    // component in it, and a page published silently short is a page nobody
    // approved. The CMS would not complain either — an unrecognised block just
    // renders as nothing.
    let placed = article.embeds();
    let embeds = components::built(session);
    if let Some(missing) = placed.iter().find(|id| !embeds.contains_key(*id)) {
        bail!(
            "the article places component {missing:?}, which has not been built — run the \
             component stage for it, or take the block out of the article"
        );
    }

    let wanted = article.figures();
    let figures = match wanted.is_empty() {
        true => Default::default(),
        false => {
            status(format!("Uploading {} figure(s)…", wanted.len()));
            figures::upload(&client, session, &wanted)?
        }
    };

    status("Uploading the thumbnail…".into());
    let thumbnail_id = client.upload_media(&thumbnail, Some(&article.title))?;
    let vertical = vertical::upload(&client, session, status)?;
    let og_image_id = Some(og::upload(&client, &og_path, &article.title, status)?);
    let cta = cta(&cfg, cta_image(&client, &cfg, status));
    let thumbnail_vertical_id = Some(client.upload_media(&portrait, Some(&article.title))?);

    let new_post = payload::NewVideoPost {
        date: payload::today(),
        video_url: upload.url.clone(),
        video_id: Some(upload.video_id.clone()),
        duration: duration.round().max(1.0) as u32,
        thumbnail_id: Some(thumbnail_id),
        thumbnail_vertical_id,
        vertical,
        og_image_id,
        cta,
        embeds,
        chapters: timeline.chapters,
        transcript: timeline.transcript,
        author_id: author,
        category_id: category,
        figures,
        article,
    };
    // On disk before the wire, so a body Strapi refuses is still readable —
    // which is the one case where reading it back matters most.
    let body = payload::save(&dir, &new_post.body())?;
    eprintln!("stream-recorder: video post → {}", body.display());

    status(format!("Creating “{}” in Strapi…", new_post.article.title));
    let created = client.create_video_post(
        &new_post,
        // Draft, not published. Reversed from the original decision: the
        // article is written by a model and the page is permanent and public,
        // so the last read happens in the CMS while it can still be changed.
        // `create_video_post` omits `?status=published` for this, which leaves
        // Strapi's draft untouched by any publish step — the admin URL on the
        // ledger row is where it gets reviewed and sent live.
        false,
    )?;

    let post = Post {
        document_id: created.document_id,
        slug: created.slug,
        url: created.public_url,
        admin_url: created.admin_url,
        video_id: upload.video_id.clone(),
        published: created.published,
        created_at: crate::schedule::ledger::now_rfc3339(),
        warning: created.warning,
    };
    append(session, &post)?;
    Ok(post)
}

/// The standing call to action, with its image already uploaded.
///
/// `None` when no URL is configured, which is the default and not a gap: the
/// field is optional and a CTA card pointing nowhere is worse than none.
///
/// The image is passed in rather than uploaded here so the dry run can build
/// the same CTA off the network — everything but the picture is knowable
/// without a call.
fn cta(cfg: &crate::config::Config, image_id: Option<i64>) -> Option<payload::MagicLinkCta> {
    let configured = &cfg.blog_cta;
    let url = configured.url.trim();
    if url.is_empty() {
        return None;
    }
    Some(payload::MagicLinkCta {
        url: url.to_string(),
        label: configured.label.trim().to_string(),
        description: configured.description.trim().to_string(),
        image_id,
    })
}

/// The CTA's image, uploaded, or `None`.
///
/// A missing or unreadable file is a warning rather than a failure: the card
/// renders without an image, and losing a finished article over the picture on
/// a standing CTA would be the tail wagging the dog.
fn cta_image(
    client: &strapi::Strapi,
    cfg: &crate::config::Config,
    status: impl Fn(String),
) -> Option<i64> {
    let path = cfg.blog_cta.image.as_ref()?;
    if !path.is_file() {
        eprintln!(
            "stream-recorder: the CTA image {} is not there — the card will render without it",
            path.display()
        );
        return None;
    }
    status("Uploading the CTA image…".into());
    client
        .upload_media(path, Some(&cfg.blog_cta.label))
        .inspect_err(|err| eprintln!("stream-recorder: CTA image not uploaded: {err:#}"))
        .ok()
}

/// The most recent post made from this exact video, if there is one.
pub fn posted(session: &Session, video_id: &str) -> Option<Post> {
    load(session)
        .into_iter()
        .rev()
        .find(|row| row.video_id == video_id)
}

pub fn load(session: &Session) -> Vec<Post> {
    let Ok(text) = std::fs::read_to_string(session.root.join(POSTS_JSONL)) else {
        return Vec::new();
    };
    text.lines()
        .filter(|line| !line.trim().is_empty())
        .filter_map(|line| serde_json::from_str(line).ok())
        .collect()
}

fn append(session: &Session, post: &Post) -> Result<()> {
    use std::io::Write;

    let path = session.root.join(POSTS_JSONL);
    let line = serde_json::to_string(post).context("serializing the blog row")?;
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .with_context(|| format!("opening {}", path.display()))?;
    writeln!(file, "{line}").with_context(|| format!("appending to {}", path.display()))
}

/// Opens the standing article prompt for editing, creating it from the builtin
/// first if it is not there yet.
///
/// Not seeded automatically, for the reason [`crate::substack::edit_prompt`]
/// gives: copying the builtin into a file freezes it, and later improvements to
/// the shipped default stop arriving. Freezing it is a fine choice; it just has
/// to be a choice.
pub fn edit_prompt() -> Result<PathBuf> {
    let path = crate::agent::prompt::ensure_library_overlay(
        crate::agent::prompt::BLOG,
        generate::SYSTEM_PROMPT,
    )?;
    if let Err(err) = std::process::Command::new("open")
        .arg("-t")
        .arg(&path)
        .status()
    {
        eprintln!("stream-recorder: could not open {}: {err}", path.display());
    }
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn session(root: &std::path::Path) -> Session {
        Session {
            root: root.to_path_buf(),
            dir: root.join("drafts"),
            version: None,
        }
    }

    fn temp(tag: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("stream-recorder-blog-{}-{tag}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn post(video_id: &str, slug: &str) -> Post {
        Post {
            document_id: format!("doc-{slug}"),
            slug: slug.into(),
            url: format!("https://saagasolve.com/blog/{slug}"),
            admin_url: "https://cms.saagasolve.com/admin/…".into(),
            video_id: video_id.into(),
            published: true,
            created_at: "2026-08-18T12:00:00Z".into(),
            warning: None,
        }
    }

    /// The guard that matters: Strapi takes the same article twice and names the
    /// second one `slug-2`, and nothing here would ever clean that up.
    #[test]
    fn the_same_video_is_recognised_as_already_posted() {
        let root = temp("dupe");
        let session = session(&root);
        append(&session, &post("vid-1", "why-it-fails")).unwrap();
        assert_eq!(posted(&session, "vid-1").unwrap().slug, "why-it-fails");
        // A different upload is different work.
        assert!(posted(&session, "vid-2").is_none());
        let _ = std::fs::remove_dir_all(&root);
    }

    /// The newest row wins, so a second attempt after a partial failure reads as
    /// the current state rather than the first one.
    #[test]
    fn the_newest_row_for_a_video_is_the_one_that_counts() {
        let root = temp("newest");
        let session = session(&root);
        append(&session, &post("vid-1", "first-try")).unwrap();
        let mut fixed = post("vid-1", "second-try");
        fixed.published = false;
        append(&session, &fixed).unwrap();

        let found = posted(&session, "vid-1").unwrap();
        assert_eq!(found.slug, "second-try");
        assert!(!found.published);
        assert_eq!(load(&session).len(), 2, "the history is kept");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn no_ledger_reads_as_nothing_posted() {
        let root = temp("empty");
        assert!(load(&session(&root)).is_empty());
        assert!(posted(&session(&root), "vid-1").is_none());
        let _ = std::fs::remove_dir_all(&root);
    }

    /// A warning survives the round trip: a post that went up with no author is
    /// worth being able to find again.
    #[test]
    fn a_warning_is_kept_on_the_row() {
        let root = temp("warned");
        let session = session(&root);
        let mut warned = post("vid-1", "slug");
        warned.warning = Some("author could not be set".into());
        append(&session, &warned).unwrap();
        assert_eq!(
            posted(&session, "vid-1").unwrap().warning.as_deref(),
            Some("author could not be set")
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    fn entry(id: i64, name: &str) -> library::Entry {
        library::Entry {
            id,
            name: name.into(),
            slug: name.to_lowercase().replace(' ', "-"),
            detail: None,
        }
    }

    fn cfg(author: Option<i64>, name: Option<&str>) -> crate::config::Config {
        crate::config::Config {
            blog_author_id: author,
            blog_author_name: name.map(str::to_string),
            ..Default::default()
        }
    }

    /// The default is nothing, which is the change: it used to be a name that is
    /// not in the CMS, so the gate passed and the post went up bylineless.
    #[test]
    fn nothing_is_chosen_until_something_is_picked() {
        let chosen = Chosen::resolve(&[], None, None, "BLOG_AUTHOR_UNSET_IN_TESTS");
        assert!(!chosen.is_set());
        assert_eq!(chosen.label, "none chosen");
    }

    /// A picked id needs no lookup and no cache — the label is cosmetic, the id
    /// is what travels.
    #[test]
    fn a_picked_id_resolves_without_the_cache() {
        let library = library::Library::default();
        let chosen = chosen_author(&cfg(Some(12), Some("Ahmed Raza")), &library);
        assert_eq!(chosen.id, Some(12));
        assert_eq!(chosen.name, None, "an id never needs looking up");
        assert_eq!(chosen.label, "Ahmed Raza");
        assert!(chosen.is_set());
    }

    /// A remembered id with no remembered name still posts. The pane shows `#12`
    /// rather than pretending the choice is gone.
    #[test]
    fn an_id_with_no_remembered_name_still_counts() {
        let chosen = chosen_author(&cfg(Some(12), None), &library::Library::default());
        assert_eq!(chosen.id, Some(12));
        assert_eq!(chosen.label, "#12");
        assert!(chosen.is_set());
    }

    /// The env escape hatch, resolved against the cache when it can be so the
    /// dropdown highlights the right row and the publish makes no extra call.
    #[test]
    fn an_env_override_matches_the_cache_when_it_can() {
        let entries = vec![entry(12, "Ahmed Raza")];
        temp_env("BLOG_AUTHOR_TEST_MATCH", "ahmed raza", || {
            let chosen = Chosen::resolve(&entries, Some(99), None, "BLOG_AUTHOR_TEST_MATCH");
            assert_eq!(chosen.id, Some(12), "the override beats the picked id");
            assert_eq!(chosen.name, None);
            assert!(chosen.label.contains("Ahmed Raza"));
        });
    }

    /// An override the cache has never heard of is carried as a name, to be
    /// looked up live — a cold cache must not silently drop the instruction.
    #[test]
    fn an_unmatched_env_override_is_carried_as_a_name() {
        temp_env("BLOG_AUTHOR_TEST_MISS", "Someone New", || {
            let chosen = Chosen::resolve(&[], None, None, "BLOG_AUTHOR_TEST_MISS");
            assert_eq!(chosen.id, None);
            assert_eq!(chosen.name.as_deref(), Some("Someone New"));
            assert!(chosen.is_set(), "a name is still something to try");
        });
    }

    /// The taxonomy is picked, not compiled in. Nothing is filed anywhere until
    /// a category is chosen on the Blog tab or named in `BLOG_CATEGORY` — the
    /// alternative is a default that names a row this CMS may not have.
    #[test]
    fn nothing_is_filed_under_a_category_nobody_picked() {
        let chosen = chosen_category(
            &crate::config::Config::default(),
            &library::Library::default(),
        );
        assert_eq!(chosen.id, None);
        assert!(!chosen.is_set());
        assert_eq!(chosen.label, "none chosen");
    }

    fn temp_env(key: &str, value: &str, body: impl FnOnce()) {
        // Safety: single-threaded within the closure, and the key is unique per
        // test so a parallel run cannot observe it.
        unsafe { std::env::set_var(key, value) };
        body();
        unsafe { std::env::remove_var(key) };
    }

    /// Refused before the ledger is even read: with nothing on YouTube there is
    /// no URL for `video.url`, and the landing renders no player without one.
    #[test]
    fn a_project_with_nothing_on_youtube_is_refused() {
        let root = temp("no-upload");
        let (tx, _rx) = std::sync::mpsc::channel();
        let err = run(&session(&root), "m", None, &tx).unwrap_err();
        assert!(err.to_string().contains("not on YouTube yet"), "{err}");
        let _ = std::fs::remove_dir_all(&root);
    }

    /// The whole point of the dry run: the body exists on disk, off the network,
    /// with nothing uploaded and nothing on YouTube yet.
    #[test]
    fn the_create_body_is_written_out_without_a_single_call() {
        let root = temp("payload");
        let session = session(&root);
        let dir = session.blog_dir();
        schema::save(
            &dir,
            &schema::Article {
                title: "Why watermarking fails".into(),
                slug: "why-watermarking-fails".into(),
                description: "A complete promise of the argument.".into(),
                keywords: vec!["ai".into()],
                h1: "Why watermarking fails, and what enforcement really costs".into(),
                faq: vec![schema::Faq {
                    question: "Does it scale?".into(),
                    answer: "Not past the first retry.".into(),
                }],
                blocks: vec![
                    schema::Block::Text {
                        html: "<p>Body.</p>".into(),
                    },
                    schema::Block::Embed {
                        id: "retry_steps".into(),
                    },
                ],
                ..Default::default()
            },
        )
        .unwrap();

        // A built component, resolved through the same path a publish uses.
        let job = dir.join(components::JOB_DIR);
        std::fs::create_dir_all(&job).unwrap();
        std::fs::write(
            job.join("components.json"),
            r#"{"version":1,"sourceId":"vd-42","blocks":[{"stepId":"retry_steps",
               "block":{"__component":"content.embed","componentKey":"step_cards",
               "componentVersion":1,"heading":"How the retry works",
               "config":{"steps":[{"title":"Back off","body":"Wait, then halve."}]}}}]}"#,
        )
        .unwrap();

        let path = write_payload(&session).unwrap();
        assert!(
            path.ends_with(payload::VIDEO_POST_JSON),
            "{}",
            path.display()
        );
        let body: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        let data = body["data"].as_object().unwrap();
        assert_eq!(data["title"], "Why watermarking fails");
        assert_eq!(data["keywords"][0], "ai");
        // The two nothing can know before an upload, both absent rather than
        // guessed at — a body that has uploaded nothing must not read as if it
        // has.
        assert_eq!(
            data["video"]["provider"], "upload",
            "nothing on youtube yet"
        );
        assert_eq!(
            data["h1"],
            "Why watermarking fails, and what enforcement really costs"
        );
        assert_eq!(data["faq"][0]["title"], "Does it scale?");
        // The component reached the zone as the stage validated it.
        assert_eq!(data["content"][1]["__component"], "content.embed");
        assert_eq!(data["content"][1]["componentKey"], "step_cards");
        // Every field that only exists after an upload, absent together — the
        // dry run has uploaded nothing and must not read as though it has.
        for field in ["thumbnail", "ogImage", "videoVertical", "thumbnailVertical"] {
            assert!(
                !data.contains_key(field),
                "{field} in a body that uploaded nothing"
            );
        }
        let _ = std::fs::remove_dir_all(&root);
    }

    /// The article was read and approved with that component in it, so the
    /// publish stops rather than quietly publishing the page without it — the
    /// CMS would not complain either, it just renders nothing there.
    #[test]
    fn an_article_placing_an_unbuilt_component_is_refused() {
        let root = temp("unbuilt-embed");
        let session = session(&root);
        schema::save(
            &session.blog_dir(),
            &schema::Article {
                title: "Why watermarking fails".into(),
                slug: "why-watermarking-fails".into(),
                description: "A complete promise of the argument.".into(),
                blocks: vec![schema::Block::Embed {
                    id: "retry_steps".into(),
                }],
                ..Default::default()
            },
        )
        .unwrap();
        let (tx, _rx) = std::sync::mpsc::channel();
        let err = run(&session, "m", None, &tx).unwrap_err();
        // Refused for the missing upload first, which is the earlier gate — the
        // point pinned here is that the component check reads the article's own
        // placements rather than the ledger.
        assert!(
            err.to_string().contains("not on YouTube yet")
                || err.to_string().contains("retry_steps"),
            "{err}"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    /// What the draft leaves out is said where the button was pressed, not in
    /// the log: an unblurbed figure is never offered, and a stale standing
    /// prompt never places one, and the first anyone knew of either was an
    /// article with no pictures in it.
    #[test]
    fn the_drafting_status_says_what_is_placed_and_what_is_left_out() {
        assert_eq!(
            drafting_status(3, 0, 0, "m", None),
            "Writing the article from 3 chapter(s) via m…"
        );
        let figured = drafting_status(3, 2, 1, "m", None);
        assert!(
            figured.contains("with 2 figure(s) to place via m…"),
            "{figured}"
        );
        assert!(figured.contains("1 figure(s) have no blurb"), "{figured}");
        assert!(
            figured.contains("press Write Blurbs"),
            "says which button: {figured}"
        );

        let stale = drafting_status(3, 2, 0, "m", Some("the standing blog prompt predates it"));
        assert!(
            stale.ends_with("Note: the standing blog prompt predates it."),
            "{stale}"
        );
        assert!(
            !stale.contains("no blurb"),
            "nothing was left out for want of a blurb"
        );
    }

    /// A draft is the prerequisite, and saying so is the difference between a
    /// button that looks broken and one that tells you which button to press.
    #[test]
    fn writing_the_body_with_no_draft_says_which_button_to_press() {
        let root = temp("payload-empty");
        let err = write_payload(&session(&root)).unwrap_err();
        assert!(err.to_string().contains("Write Article"), "{err}");
        let _ = std::fs::remove_dir_all(&root);
    }
}
