//! Which pipeline steps can run right now, and what the blocked ones are waiting for.
//!
//! Every stage reads the previous stage's files off disk, but it does that
//! inside its own worker thread — so pressing a button too early does not fail
//! fast. It spawns a job that dies later with a path in the message, and in the
//! meantime the tab looks like it is working. The same paths are cheap to stat
//! up front, so this module answers the question before the press: a stage
//! whose inputs are not there is switched off, and says the one thing it needs.
//!
//! It is deliberately the *hard* prerequisites only. Titles are optional to a
//! render (it falls back to notes), and a render is optional to posts (they
//! fall back to the cut), so neither is gated on the step before it. Gating on
//! taste rather than on what the code actually reads would block real work.

use std::path::Path;

use crate::session::Session;

const LONGFORM: &str = "horizontal/longform.mp4";

/// Whether one stage will accept a press.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Gate {
    /// Inputs are on disk. The button runs.
    Ready,
    /// This stage's worker is already running. The button is off, but nothing is
    /// missing — and the reason is deliberately not a message, because the job
    /// is already writing better ones into the same status line.
    Busy,
    /// The one thing that has to exist first.
    Missing(String),
}

impl Gate {
    pub fn is_ready(&self) -> bool {
        matches!(self, Gate::Ready)
    }

    /// The blocking reason worth showing the user, which a busy stage does not
    /// have — its own job is narrating.
    pub fn missing(&self) -> Option<&str> {
        match self {
            Gate::Missing(reason) => Some(reason),
            _ => None,
        }
    }
}

/// Jobs already in flight, which block their own stage without anything being
/// absent. Both distribute and schedule write publicly, so a double press must
/// not start a second thread over the same files.
#[derive(Debug, Clone, Copy, Default)]
pub struct Busy {
    pub render: bool,
    pub distribute: bool,
    pub schedule: bool,
    pub publish: bool,
    pub substack: bool,
    pub blog: bool,
}

/// Every stage button's state, read from one pass over the project.
#[derive(Debug, Clone)]
pub struct Stages {
    pub titles: Gate,
    pub render: Gate,
    pub posts: Gate,
    /// The Substack notes. Nothing distributes them, so nothing downstream
    /// depends on this — it is gated only on there being words to write from.
    pub substack: Gate,
    pub distribute: Gate,
    pub plan: Gate,
    pub approve: Gate,
    pub queue: Gate,
    /// The YouTube upload, which is not a Buffer stage — see [`crate::publish`].
    pub publish: Gate,
    /// The `/blog` video post. Gated on the YouTube upload rather than on the
    /// render, because the `video` component is sent with the YouTube id the
    /// upload ledger holds — and the landing renders no player without one.
    pub blog: Gate,
}

impl Stages {
    pub fn read(session: &Session, busy: Busy) -> Stages {
        let chapters = !crate::notes::closed_chapter_numbers(&session.dir).is_empty();
        let recorded = (chapters, "No chapters recorded yet — record one first");

        let rendered = has_render(&session.render_dir());
        // Distribute takes any cut; YouTube takes the longform specifically, and
        // there is no substitute for it there.
        let longform = session.render_dir().join(LONGFORM).is_file();
        let linked = session
            .distribute_dir()
            .join(crate::distribute::schema::LINKS_JSON)
            .is_file();
        let posted = session
            .posts_dir()
            .join(crate::posts::schema::POSTS_JSON)
            .is_file();
        let planned = session
            .schedule_dir()
            .join(crate::schedule::schema::SCHEDULE_JSON)
            .is_file();

        let bucket = (
            env_set("S3_BUCKET"),
            "S3_BUCKET is unset — add it to stream-recorder/.env",
        );
        let buffer = (
            env_set("BUFFER_API_KEY"),
            "BUFFER_API_KEY is unset — add it to stream-recorder/.env",
        );
        let has_plan = (planned, "No plan yet — press Build Plan");
        // The blog needs the YouTube URL, not the render: the article is built
        // around an embed, and there is no embed before the upload.
        let uploaded = !crate::publish::load(session).is_empty();
        let has_thumbnail = crate::card::assets::ready(&session.root).is_ok();
        // Reads the same config the pane does, so the button and the dropdown
        // never disagree. A file read, like the thumbnail check above it.
        let has_author =
            crate::blog::chosen_author(&crate::config::load(), &crate::blog::library::load())
                .is_set();

        Stages {
            titles: gate(false, &[recorded]),
            render: gate(busy.render, &[recorded]),
            posts: gate(false, &[recorded]),
            // Same prerequisite as posts, and for the reason this module opens
            // with: gate on what the code actually reads. Notes are written from
            // the transcript, so they are available the moment a chapter closes —
            // the render only adds timestamps, and a run without them still
            // types fine.
            substack: gate(busy.substack, &[recorded]),
            distribute: gate(
                busy.distribute,
                &[
                    (rendered, "Nothing rendered yet — run Render first"),
                    bucket,
                ],
            ),
            plan: gate(
                busy.schedule,
                &[
                    (linked, "No public URLs yet — use Socials → Upload media first"),
                    (posted, "No posts yet — generate them in Socials"),
                    buffer,
                ],
            ),
            approve: gate(busy.schedule, &[has_plan]),
            queue: gate(busy.schedule, &[has_plan, buffer]),
            publish: gate(
                busy.publish,
                &[
                    (longform, "No longform rendered yet — run Render first"),
                    (has_thumbnail, "Generate artwork on Thumbnails first"),
                    (crate::publish::metadata::load(session).validate().is_ok(), "Save a valid title and description on the YouTube tab"),
                ],
            ),
            blog: gate(
                busy.blog,
                &[
                    (uploaded, "Not on YouTube yet — upload it on the YouTube tab first"),
                    (has_thumbnail, "Generate artwork on Thumbnails first"),
                    (
                        env_set("STRAPI_API_URL") && env_set("STRAPI_API_TOKEN"),
                        "STRAPI_API_URL / STRAPI_API_TOKEN unset — add them to stream-recorder/.env",
                    ),
                    // The byline is a gate rather than a warning because the page
                    // is permanent and the field feeds the Person structured data
                    // the /education detail page emits. It used to be one line of
                    // log, which is how the live post ended up with no author.
                    (
                        has_author,
                        "No author chosen — pick one on the Blog tab",
                    ),
                ],
            ),
        }
    }
}

/// Busy wins over missing: a running job is the more useful thing to say, and
/// the files it is about to write are exactly the ones reported absent.
fn gate(busy: bool, needs: &[(bool, &str)]) -> Gate {
    if busy {
        return Gate::Busy;
    }
    match needs.iter().find(|(met, _)| !met) {
        Some((_, reason)) => Gate::Missing((*reason).to_string()),
        None => Gate::Ready,
    }
}

/// The same files [`crate::distribute`] collects, asked the same way: a longform
/// cut, or any vertical chapter.
fn has_render(render_dir: &Path) -> bool {
    if render_dir.join(LONGFORM).is_file() {
        return true;
    }
    (1..=99).any(|n| {
        render_dir
            .join(format!("vertical/chapter-{n:02}.mp4"))
            .is_file()
    })
}

fn env_set(key: &str) -> bool {
    std::env::var(key)
        .map(|value| !value.trim().is_empty())
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn temp(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "stream-recorder-stage-{}-{tag}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn session(root: &Path) -> Session {
        Session {
            root: root.to_path_buf(),
            dir: root.join("drafts"),
            version: None,
        }
    }

    fn write(path: PathBuf) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, b"x").unwrap();
    }

    #[test]
    fn an_empty_project_blocks_every_stage() {
        let root = temp("empty");
        let stages = Stages::read(&session(&root), Busy::default());
        assert!(stages.titles.missing().unwrap().contains("No chapters"));
        assert!(stages.render.missing().unwrap().contains("No chapters"));
        assert!(stages.posts.missing().unwrap().contains("No chapters"));
        assert!(!stages.distribute.is_ready());
        assert!(!stages.plan.is_ready());
        assert!(!stages.queue.is_ready());
        let _ = std::fs::remove_dir_all(&root);
    }

    /// The bug that started this: Distribute invited a press with no renders on
    /// disk, then failed a thread later.
    #[test]
    fn distribute_waits_for_a_render() {
        let root = temp("distribute");
        write(root.join("drafts/chapter-01.mp4"));
        let stages = Stages::read(&session(&root), Busy::default());
        assert_eq!(stages.distribute.missing(), Some(NO_RENDER));
        write(root.join("render/vertical/chapter-01.mp4"));
        let stages = Stages::read(&session(&root), Busy::default());
        // The file is no longer what it is waiting for. Whether it is now ready
        // or held on S3_BUCKET depends on the environment the test runs in, and
        // asserting on that would make this pass or fail by shell.
        assert_ne!(stages.distribute.missing(), Some(NO_RENDER));
        let _ = std::fs::remove_dir_all(&root);
    }

    const NO_RENDER: &str = "Nothing rendered yet — run Render first";

    /// The YouTube upload wants the longform itself, so a project with only
    /// vertical chapters must not offer it.
    #[test]
    fn youtube_waits_for_the_longform_not_any_render() {
        let root = temp("publish");
        write(root.join("render/vertical/chapter-01.mp4"));
        let stages = Stages::read(&session(&root), Busy::default());
        assert!(stages.publish.missing().unwrap().contains("No longform"));
        write(root.join("render/horizontal/longform.mp4"));
        let stages = Stages::read(&session(&root), Busy::default());
        // YouTube precedes social posts but requires its complete artwork set.
        assert!(!root.join("posts/posts.json").exists());
        assert!(stages.publish.missing().unwrap().contains("artwork"));
        crate::card::assets::fixture(&root);
        assert!(Stages::read(&session(&root), Busy::default()).publish.is_ready());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn recording_a_chapter_opens_titles_render_and_posts() {
        let root = temp("recorded");
        write(root.join("drafts/chapter-01.mp4"));
        let stages = Stages::read(&session(&root), Busy::default());
        assert!(stages.titles.is_ready());
        assert!(stages.render.is_ready());
        assert!(stages.posts.is_ready());
        assert!(stages.substack.is_ready());
        let _ = std::fs::remove_dir_all(&root);
    }

    /// The Substack notes are written from the transcript, so they open with the
    /// first chapter and never wait on a render, an upload or a channel.
    #[test]
    fn substack_waits_only_for_something_to_have_been_said() {
        let root = temp("substack");
        let stages = Stages::read(&session(&root), Busy::default());
        assert!(stages.substack.missing().unwrap().contains("No chapters"));
        write(root.join("drafts/chapter-01.mp4"));
        assert!(Stages::read(&session(&root), Busy::default()).substack.is_ready());
        let busy = Busy { substack: true, ..Busy::default() };
        assert_eq!(Stages::read(&session(&root), busy).substack, Gate::Busy);
        let _ = std::fs::remove_dir_all(&root);
    }

    /// A running job blocks its own button without claiming anything is absent,
    /// so the worker's own progress line is left to speak.
    #[test]
    fn a_running_job_reports_busy_not_missing() {
        let root = temp("busy");
        write(root.join("drafts/chapter-01.mp4"));
        let busy = Busy {
            render: true,
            ..Busy::default()
        };
        let stages = Stages::read(&session(&root), busy);
        assert_eq!(stages.render, Gate::Busy);
        assert!(!stages.render.is_ready());
        assert_eq!(stages.render.missing(), None);
        // Only the stage that is running: the others are unaffected.
        assert!(stages.posts.is_ready());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn approve_and_queue_wait_for_a_saved_plan() {
        let root = temp("plan");
        let stages = Stages::read(&session(&root), Busy::default());
        assert_eq!(
            stages.approve.missing(),
            Some("No plan yet — press Build Plan")
        );
        write(root.join("schedule/schedule.json"));
        let stages = Stages::read(&session(&root), Busy::default());
        assert!(stages.approve.is_ready());
        let _ = std::fs::remove_dir_all(&root);
    }

    /// A versioned project asks about `{stage}/vN`, which is the whole reason
    /// the gates read through [`Session`] rather than off the root.
    #[test]
    fn the_gates_follow_the_open_version() {
        let root = temp("versioned");
        let session = Session {
            root: root.clone(),
            dir: root.join("drafts/v2"),
            version: Some(2),
        };
        write(root.join("drafts/v2/chapter-01.mp4"));
        write(root.join("render/horizontal/longform.mp4"));
        // v2's own render folder is empty, so the flat one must not count.
        let stages = Stages::read(&session, Busy::default());
        assert_eq!(stages.distribute.missing(), Some(NO_RENDER));
        write(root.join("render/v2/horizontal/longform.mp4"));
        let stages = Stages::read(&session, Busy::default());
        assert_ne!(stages.distribute.missing(), Some(NO_RENDER));
        let _ = std::fs::remove_dir_all(&root);
    }
}
