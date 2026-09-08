//! Where a recording's files land: always a self-contained project directory
//! under `~/.stream-recorder/sessions/{id}/`.
//!
//! Nothing here reads the screencast pipeline's Drive-side project convention.
//! A project owns every artifact it produces — chapters, notes, cut, render,
//! titles, posts, the distribute links and `schedule.jsonl` — inside its own
//! folder, so the whole loop can run without another tool having laid the
//! ground first.

use std::path::PathBuf;
use std::time::SystemTime;

use anyhow::{Context, Result};

/// One take's output location, and the version it is on.
///
/// `{root}/drafts/vN` holds the recording, `{root}/notes` the deck, and
/// `{root}/edit/vN` the cut. `dir` is the draft folder so existing writers
/// keep writing chapter files in the right place. `version` is `None` for a
/// project whose takes sit flat in `drafts/`, which is how a new one starts.
#[derive(Debug, Clone)]
pub struct Session {
    pub root: PathBuf,
    pub dir: PathBuf,
    pub version: Option<u32>,
}

impl Session {
    pub fn open() -> Result<Session> {
        // Resume rather than mint. A fresh timestamp on every launch orphans the
        // previous take — its chapters, notes, renders and, worse, its
        // `schedule.jsonl`, whose absence would let an already-queued post be sent
        // to Buffer a second time. New projects are made deliberately, by
        // [`Session::create`].
        match crate::sessions::latest() {
            Some(previous) => {
                println!(
                    "stream-recorder: resuming project {} → {}",
                    previous.title(),
                    previous.root.display()
                );
                Session::open_root(previous.root)
            }
            None => Session::create(),
        }
    }

    /// Opens a specific project folder, at whichever version it was left on.
    ///
    /// Resuming the version is the whole job. Every stage lives under
    /// `{stage}/vN` once a project has been bumped, so opening it flat would
    /// point Cut, Render, Titles, Posts, Distribute and Schedule at paths that
    /// do not exist — the work is all still on disk, and none of it shows.
    pub fn open_root(root: PathBuf) -> Result<Session> {
        let version = resumable_version(&root);
        let dir = match version {
            Some(v) => ensure_dir(root.join("drafts").join(format!("v{v}")))?,
            None => ensure_dir(root.join("drafts"))?,
        };
        Ok(Session { root, dir, version })
    }

    /// Starts a new project in a fresh timestamped folder, at v1.
    ///
    /// Versioned from its first take, deliberately. When the first take was
    /// unversioned, `v1` meant the *second* take, take one had no number anyone
    /// could say out loud, and — because the version picker lists `vN` folders —
    /// it had no row in that picker either. There was no way back to it once a
    /// version existed. Projects made before this still open flat; see
    /// [`resumable_version`].
    pub fn create() -> Result<Session> {
        let root = session_dir()?;
        println!("stream-recorder: new project → {}", root.display());
        let dir = ensure_dir(root.join("drafts").join("v1"))?;
        Ok(Session {
            root,
            dir,
            version: Some(1),
        })
    }

    /// This project's given name, or `None` while it is still just a timestamp.
    pub fn name(&self) -> Option<String> {
        crate::sessions::load_name(&self.root)
    }

    /// What to call this project in the UI and in the window title: its given
    /// name, else the folder timestamp that is always there to fall back on.
    pub fn title(&self) -> String {
        self.name().unwrap_or_else(|| self.folder())
    }

    /// The durable identity every path, S3 key and schedule row is written
    /// against — the timestamped folder name, never the given name.
    pub fn folder(&self) -> String {
        self.root
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| "session".into())
    }

    pub fn set_name(&self, name: &str) -> Result<()> {
        crate::sessions::save_name(&self.root, name)
    }

    pub fn edit_dir(&self) -> PathBuf {
        self.staged("edit")
    }

    pub fn compose_dir(&self) -> PathBuf {
        self.staged("compose")
    }

    pub fn render_dir(&self) -> PathBuf {
        self.staged("render")
    }

    pub fn posts_dir(&self) -> PathBuf {
        self.staged("posts")
    }

    /// Where the Substack notes land. Version-scoped like every other stage: a
    /// re-recorded take is a different essay, not an edit of the last one's.
    pub fn substack_dir(&self) -> PathBuf {
        self.staged("substack")
    }

    /// Where the blog article draft lands, beside the ledger row it produced.
    /// Version-scoped for the same reason as the rest: a re-recorded take is a
    /// different article, not an edit of the last one's.
    pub fn blog_dir(&self) -> PathBuf {
        self.staged("blog")
    }

    pub fn titles_dir(&self) -> PathBuf {
        self.staged("titles")
    }

    pub fn reflect_dir(&self) -> PathBuf {
        self.staged("reflect")
    }

    pub fn distribute_dir(&self) -> PathBuf {
        self.staged("distribute")
    }

    pub fn schedule_dir(&self) -> PathBuf {
        self.staged("schedule")
    }

    fn staged(&self, name: &str) -> PathBuf {
        match self.version {
            Some(v) => self.root.join(name).join(format!("v{v}")),
            None => self.root.join(name),
        }
    }

    pub fn notes_dir(&self) -> Result<PathBuf> {
        ensure_dir(self.root.join("notes"))
    }

    pub fn list_versions(&self) -> Vec<VersionInfo> {
        list_versions(&self.root)
    }

    pub fn open_version(&self, version: u32) -> Result<Session> {
        let dir = ensure_dir(self.root.join("drafts").join(format!("v{version}")))?;
        println!(
            "stream-recorder: switched to v{version} → {}",
            dir.display()
        );
        Ok(Session {
            root: self.root.clone(),
            dir,
            version: Some(version),
        })
    }

    /// Finish this take and open the next version *of this same project*.
    ///
    /// A version is a folder inside the project, so this never mints a new
    /// project — that is [`Session::create`]'s job, behind its own button.
    pub fn next_version(&self) -> Result<Session> {
        let next = next_version_number(self.version, latest_version(&self.root));
        let dir = ensure_dir(self.root.join("drafts").join(format!("v{next}")))?;
        println!("stream-recorder: new version v{next} → {}", dir.display());
        Ok(Session {
            root: self.root.clone(),
            dir,
            version: Some(next),
        })
    }
}

/// One past whichever is further along: the version being recorded, or the
/// highest one already on disk. Taking the max of both matters when you are
/// sitting on v2 while v5 exists — bumping to v3 would overwrite real work.
fn next_version_number(current: Option<u32>, latest: Option<u32>) -> u32 {
    current.into_iter().chain(latest).max().unwrap_or(0) + 1
}

pub fn output_dir() -> Result<PathBuf> {
    Ok(Session::open()?.dir)
}

fn ensure_dir(dir: PathBuf) -> Result<PathBuf> {
    std::fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;
    Ok(dir)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VersionInfo {
    pub n: u32,
    pub has_draft: bool,
    pub has_edit: bool,
    pub has_render: bool,
    pub has_titles: bool,
    pub has_posts: bool,
    pub has_distribute: bool,
}

impl VersionInfo {
    /// True when the folder exists but nothing ever landed in it — a bump that
    /// was made and then not used.
    pub fn is_empty(&self) -> bool {
        !(self.has_draft
            || self.has_edit
            || self.has_render
            || self.has_titles
            || self.has_posts
            || self.has_distribute)
    }

    pub fn label(&self) -> String {
        let mut bits = Vec::new();
        if self.has_draft {
            bits.push("draft");
        }
        if self.has_edit {
            bits.push("cut");
        }
        if self.has_render {
            bits.push("render");
        }
        if self.has_titles {
            bits.push("titles");
        }
        if self.has_posts {
            bits.push("posts");
        }
        if self.has_distribute {
            bits.push("distribute");
        }
        if bits.is_empty() {
            format!("v{}", self.n)
        } else {
            format!("v{} · {}", self.n, bits.join(", "))
        }
    }
}

fn list_versions(root: &std::path::Path) -> Vec<VersionInfo> {
    let mut nums = std::collections::BTreeSet::new();
    for stage in ["drafts", "edit", "render", "posts", "titles", "distribute"] {
        let Some(entries) = std::fs::read_dir(root.join(stage)).ok() else {
            continue;
        };
        for entry in entries.flatten() {
            let name = entry.file_name();
            let Some(name) = name.to_str() else {
                continue;
            };
            if let Some(n) = name.strip_prefix('v').and_then(|s| s.parse::<u32>().ok()) {
                nums.insert(n);
            }
        }
    }
    nums.into_iter()
        .map(|n| VersionInfo {
            n,
            has_draft: stage_has(root, "drafts", n),
            has_edit: stage_has(root, "edit", n),
            has_render: stage_has(root, "render", n),
            has_titles: root
                .join("titles")
                .join(format!("v{n}"))
                .join("titles.json")
                .exists(),
            has_posts: root
                .join("posts")
                .join(format!("v{n}"))
                .join("posts.json")
                .exists(),
            has_distribute: root
                .join("distribute")
                .join(format!("v{n}"))
                .join("links.json")
                .exists(),
        })
        .collect()
}

fn stage_has(root: &std::path::Path, stage: &str, n: u32) -> bool {
    let dir = root.join(stage).join(format!("v{n}"));
    dir.read_dir()
        .map(|mut it| it.next().is_some())
        .unwrap_or(false)
}

fn latest_version(root: &std::path::Path) -> Option<u32> {
    list_versions(root).into_iter().map(|v| v.n).max()
}

/// The version to reopen, in three cases.
///
/// The newest version holding work, when there is one. Otherwise a flat project
/// — takes loose in `drafts/`, from before [`Session::create`] versioned them —
/// which must open flat even when a stray empty `v1` sits beside those files,
/// or resuming would hide every one of them behind a version that has none.
/// Otherwise the newest version there is, which is how a project created but not
/// yet recorded into keeps the version it was made with instead of falling back
/// to a flat folder it will never use.
fn resumable_version(root: &std::path::Path) -> Option<u32> {
    let versions = list_versions(root);
    let worked_in = versions
        .iter()
        .filter(|version| !version.is_empty())
        .map(|version| version.n)
        .max();
    if worked_in.is_some() {
        return worked_in;
    }
    if flat_take_exists(root) {
        return None;
    }
    versions.iter().map(|version| version.n).max()
}

/// Whether `drafts/` holds a take of its own, ignoring the `vN` folders inside it.
fn flat_take_exists(root: &std::path::Path) -> bool {
    std::fs::read_dir(root.join("drafts"))
        .map(|entries| entries.flatten().any(|entry| entry.path().is_file()))
        .unwrap_or(false)
}

fn session_dir() -> Result<PathBuf> {
    use std::time::UNIX_EPOCH;

    let home = std::env::var_os("HOME").context("HOME not set")?;

    // Format as YYYY-MM-DD_HH-MM-SS for readability.
    // Note: this uses UTC internally; a full solution would use the local timezone,
    // but that would require an external crate. This is sufficient for timestamping.
    let now = SystemTime::now();
    let duration = now
        .duration_since(UNIX_EPOCH)
        .context("system clock before 1970")?;
    let total_secs = duration.as_secs();
    let secs_per_day = 86400;
    let days_since_epoch = total_secs / secs_per_day;
    let secs_today = total_secs % secs_per_day;

    // Simple year/month/day calculation from days since 1970-01-01.
    // This is approximate but good enough for directory names.
    let (year, month, day) = days_to_ymd(days_since_epoch);
    let (hour, min, sec) = secs_to_hms(secs_today);

    let id = format!(
        "{:04}-{:02}-{:02}_{:02}-{:02}-{:02}",
        year, month, day, hour, min, sec
    );
    let dir = PathBuf::from(home)
        .join(".stream-recorder")
        .join("sessions")
        .join(id);
    std::fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;
    Ok(dir)
}

/// Convert days since 1970-01-01 to (year, month, day) in UTC.
/// Approximate calculation, good enough for timestamps.
fn days_to_ymd(mut days: u64) -> (u32, u32, u32) {
    let mut year = 1970;
    loop {
        let days_in_year = if is_leap_year(year) { 366 } else { 365 };
        if days < days_in_year as u64 {
            break;
        }
        days -= days_in_year as u64;
        year += 1;
    }
    let days_in_month = [
        31,
        if is_leap_year(year) { 29 } else { 28 },
        31,
        30,
        31,
        30,
        31,
        31,
        30,
        31,
        30,
        31,
    ];
    let mut month = 1;
    for &dim in &days_in_month {
        if days < dim as u64 {
            break;
        }
        days -= dim as u64;
        month += 1;
    }
    (year, month, (days + 1) as u32)
}

fn is_leap_year(year: u32) -> bool {
    (year.is_multiple_of(4) && !year.is_multiple_of(100)) || year.is_multiple_of(400)
}

fn secs_to_hms(secs: u64) -> (u32, u32, u32) {
    let hour = (secs / 3600) as u32;
    let min = ((secs % 3600) / 60) as u32;
    let sec = (secs % 60) as u32;
    (hour, min, sec)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_stage_of_a_versioned_take_lands_under_the_same_project() {
        let session = Session {
            root: PathBuf::from("/tmp/rec"),
            dir: PathBuf::from("/tmp/rec/drafts/v3"),
            version: Some(3),
        };
        assert_eq!(session.edit_dir(), PathBuf::from("/tmp/rec/edit/v3"));
        assert_eq!(session.compose_dir(), PathBuf::from("/tmp/rec/compose/v3"));
        assert_eq!(session.render_dir(), PathBuf::from("/tmp/rec/render/v3"));
        assert_eq!(session.posts_dir(), PathBuf::from("/tmp/rec/posts/v3"));
        assert_eq!(
            session.substack_dir(),
            PathBuf::from("/tmp/rec/substack/v3")
        );
        assert_eq!(
            session.distribute_dir(),
            PathBuf::from("/tmp/rec/distribute/v3")
        );
        assert_eq!(
            session.schedule_dir(),
            PathBuf::from("/tmp/rec/schedule/v3")
        );
        assert_eq!(session.version, Some(3));
    }

    /// A new version is a folder inside the project, so the New Version button
    /// can never quietly do what the New Project button is for.
    #[test]
    fn a_new_version_stays_inside_the_same_project() {
        let root = std::env::temp_dir().join(format!(
            "stream-recorder-next-version-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        let session = Session::open_root(root.clone()).unwrap();
        let next = session.next_version().unwrap();
        assert_eq!(next.root, root);
        assert_eq!(next.version, Some(1));
        assert_eq!(next.dir, root.join("drafts/v1"));
        let after = next.next_version().unwrap();
        assert_eq!(after.root, root);
        assert_eq!(after.version, Some(2));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_bump_never_lands_on_a_version_that_already_exists() {
        // Sitting on v2 with v5 already recorded: v3 would overwrite v3's work.
        assert_eq!(next_version_number(Some(2), Some(5)), 6);
        assert_eq!(next_version_number(Some(5), Some(2)), 6);
        assert_eq!(next_version_number(Some(3), Some(3)), 4);
        // An unversioned project's takes sit flat in drafts/; the first bump is v1.
        assert_eq!(next_version_number(None, None), 1);
        assert_eq!(next_version_number(None, Some(4)), 5);
    }

    /// The regression this guards: a project bumped to v1 opened flat, so every
    /// tab — Cut, Render, Titles, Posts, Distribute — read `{root}/{stage}` and
    /// found nothing, while all of it sat under `{stage}/v1`.
    #[test]
    fn reopening_a_bumped_project_lands_back_on_its_version() {
        let root =
            std::env::temp_dir().join(format!("stream-recorder-resume-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("drafts/v1")).unwrap();
        std::fs::write(root.join("drafts/v1/chapter-01.mp4"), b"x").unwrap();
        std::fs::create_dir_all(root.join("posts/v1")).unwrap();
        std::fs::write(root.join("posts/v1/posts.json"), b"{}").unwrap();
        let session = Session::open_root(root.clone()).unwrap();
        assert_eq!(session.version, Some(1));
        assert_eq!(session.dir, root.join("drafts/v1"));
        assert_eq!(session.posts_dir(), root.join("posts/v1"));
        assert_eq!(session.distribute_dir(), root.join("distribute/v1"));
        let _ = std::fs::remove_dir_all(&root);
    }

    /// The mirror image, and why [`resumable_version`] skips empty folders: a
    /// flat project with a leftover empty `drafts/v1` must still open flat, or
    /// resuming would hide the files it does have.
    #[test]
    fn a_flat_project_with_an_unused_bump_still_opens_flat() {
        let root = std::env::temp_dir().join(format!(
            "stream-recorder-resume-flat-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("drafts/v1")).unwrap();
        std::fs::write(root.join("drafts/chapter-01.mp4"), b"x").unwrap();
        std::fs::create_dir_all(root.join("distribute")).unwrap();
        std::fs::write(root.join("distribute/links.json"), b"{}").unwrap();
        let session = Session::open_root(root.clone()).unwrap();
        assert_eq!(session.version, None);
        assert_eq!(session.dir, root.join("drafts"));
        assert_eq!(session.distribute_dir(), root.join("distribute"));
        let _ = std::fs::remove_dir_all(&root);
    }

    /// A project is versioned from its first take, so take one is v1 and shows up
    /// in the version picker like every other take.
    #[test]
    fn a_new_project_starts_at_v1() {
        let root =
            std::env::temp_dir().join(format!("stream-recorder-resume-new-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("drafts/v1")).unwrap();
        let session = Session::open_root(root.clone()).unwrap();
        assert_eq!(session.version, Some(1));
        assert_eq!(session.dir, root.join("drafts/v1"));
        // And the first bump off it is v2, not a second v1.
        assert_eq!(session.next_version().unwrap().version, Some(2));
        let _ = std::fs::remove_dir_all(&root);
    }

    /// Nothing on disk at all still opens flat: `create` is what makes a project
    /// versioned, and it is the only thing that should.
    #[test]
    fn an_empty_folder_with_no_versions_opens_flat() {
        let root = std::env::temp_dir().join(format!(
            "stream-recorder-resume-bare-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        let session = Session::open_root(root.clone()).unwrap();
        assert_eq!(session.version, None);
        assert_eq!(session.dir, root.join("drafts"));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn create_makes_a_versioned_project() {
        let session = Session::create().unwrap();
        assert_eq!(session.version, Some(1));
        assert!(session.dir.ends_with("drafts/v1"));
        // Reopening it finds v1 again rather than dropping to flat, even though
        // nothing has been recorded into it yet.
        let reopened = Session::open_root(session.root.clone()).unwrap();
        assert_eq!(reopened.version, Some(1));
        let _ = std::fs::remove_dir_all(&session.root);
    }

    #[test]
    fn list_versions_reads_stage_folders() {
        let root =
            std::env::temp_dir().join(format!("stream-recorder-versions-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("drafts/v1")).unwrap();
        std::fs::write(root.join("drafts/v1/chapter-01.mp4"), b"x").unwrap();
        std::fs::create_dir_all(root.join("edit/v1/chapter-01")).unwrap();
        std::fs::write(root.join("edit/v1/chapter-01/edits.json"), b"[]").unwrap();
        std::fs::create_dir_all(root.join("render/v2/horizontal")).unwrap();
        std::fs::write(root.join("render/v2/horizontal/longform.mp4"), b"x").unwrap();
        let found = list_versions(&root);
        assert_eq!(found.len(), 2);
        assert!(found[0].has_draft && found[0].has_edit);
        assert!(found[1].has_render);
        assert_eq!(latest_version(&root), Some(2));
        let _ = std::fs::remove_dir_all(&root);
    }
}
