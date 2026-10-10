//! The Files tab: everything the uploads have put on S3, as a tree.
//!
//! One listing of the upload prefix — `S3_BUCKET` / `S3_PREFIX`, today
//! `saaga-dev-cdn/socials/` — folded into folders on `/`. Read-only: a file
//! row opens its public URL or copies it, and nothing here writes to the
//! bucket.
//!
//! Listed at launch, after every upload and on Refresh; never polled. The pane
//! is re-rendered whole like every other (see `ui::web`), so a fresh listing
//! closes whatever folders were opened by hand and opens the project on screen.

use std::cmp::Ordering;
use std::collections::BTreeMap;
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread;

use serde::Serialize;

use crate::distribute::{Listed, Listing};

pub enum FilesEvent {
    Listed(Listing),
    Failed(String),
}

/// The listing job and what it last brought back.
pub struct FilesState {
    tx: Sender<FilesEvent>,
    rx: Receiver<FilesEvent>,
    busy: bool,
    listing: Option<Listing>,
    listed_at: Option<i64>,
    /// The last refresh's failure. The tree from the one before stays on
    /// screen under it, which beats an empty pane for a dropped login.
    error: Option<String>,
}

impl FilesState {
    pub fn new() -> Self {
        let (tx, rx) = mpsc::channel();
        FilesState {
            tx,
            rx,
            busy: false,
            listing: None,
            listed_at: None,
            error: None,
        }
    }

    /// Starts a listing unless one is already running; `true` if it started.
    pub fn refresh(&mut self) -> bool {
        if self.busy {
            return false;
        }
        self.busy = true;
        self.error = None;
        spawn(self.tx.clone());
        true
    }

    /// Takes what the listing thread sent; `true` when the pane should be
    /// drawn again.
    pub fn drain(&mut self) -> bool {
        let mut changed = false;
        for event in self.rx.try_iter() {
            self.busy = false;
            changed = true;
            match event {
                FilesEvent::Listed(listing) => {
                    eprintln!(
                        "stream-recorder: {} object(s) under {}",
                        listing.objects.len(),
                        listing.location
                    );
                    self.listing = Some(listing);
                    self.listed_at = Some(chrono::Local::now().timestamp());
                    self.error = None;
                }
                FilesEvent::Failed(msg) => {
                    eprintln!("stream-recorder: files listing failed: {msg}");
                    self.error = Some(msg);
                }
            }
        }
        changed
    }

    /// The pane, with `project` — the open project's folder — expanded.
    pub fn pane(&self, project: &str) -> Pane {
        let mut pane = Pane {
            location: "the upload bucket".into(),
            busy: self.busy,
            error: self.error.clone(),
            listed_at: self.listed_at.map(when),
            summary: None,
            truncated: false,
            tree: Vec::new(),
        };
        if let Some(listing) = &self.listing {
            pane.location = listing.location.clone();
            pane.truncated = listing.truncated;
            pane.tree = tree(&listing.objects, project);
            let size: u64 = listing.objects.iter().map(|o| o.size).sum();
            let files = listing.objects.iter().filter(|o| !is_marker(o)).count();
            let projects = pane.tree.iter().filter(|n| n.folder).count();
            pane.summary = Some(format!(
                "{files} file(s) · {} · {projects} project(s)",
                human_size(size)
            ));
        }
        pane
    }
}

impl Default for FilesState {
    fn default() -> Self {
        Self::new()
    }
}

fn spawn(tx: Sender<FilesEvent>) {
    // Kept back from the closure so a thread that never starts still reports —
    // otherwise the pane says Refreshing for good.
    let unstarted = tx.clone();
    if let Err(err) = thread::Builder::new()
        .name("files-s3".into())
        .spawn(move || {
            let event = match crate::distribute::list_uploads() {
                Ok(listing) => FilesEvent::Listed(listing),
                Err(err) => FilesEvent::Failed(format!("{err:#}")),
            };
            let _ = tx.send(event);
        })
    {
        let _ = unstarted.send(FilesEvent::Failed(format!(
            "Could not start the S3 listing: {err}"
        )));
    }
}

/// What `files.html` draws.
#[derive(Debug, Serialize)]
pub struct Pane {
    /// `s3://<bucket>/<prefix>/`.
    pub location: String,
    pub busy: bool,
    pub error: Option<String>,
    /// When the tree on screen was listed, local time.
    pub listed_at: Option<String>,
    pub summary: Option<String>,
    pub truncated: bool,
    pub tree: Vec<Node>,
}

impl Pane {
    /// Before the first listing lands.
    pub fn empty() -> Pane {
        FilesState::new().pane("")
    }
}

/// A folder, or a file with the URL it is served at.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Node {
    pub name: String,
    pub folder: bool,
    /// Drawn expanded.
    pub open: bool,
    /// For a folder, the files anywhere beneath it.
    pub files: usize,
    pub size: String,
    /// For a folder, the newest file beneath it.
    pub modified: String,
    pub url: Option<String>,
    pub children: Vec<Node>,
}

/// A folder as it is gathered, before it is sorted and summed.
#[derive(Default)]
struct Dir<'a> {
    dirs: BTreeMap<&'a str, Dir<'a>>,
    files: Vec<(&'a str, &'a Listed)>,
}

/// Folds `objects` into folders on `/`, with `project`'s folder and its
/// newest version open — the upload lands at `<project>/v<N>/`.
pub fn tree(objects: &[Listed], project: &str) -> Vec<Node> {
    let mut root = Dir::default();
    for object in objects {
        let mut segments: Vec<&str> = object.path.split('/').collect();
        // A key ending in `/` is a folder marker: it makes the folder, and is
        // not a file of its own.
        let name = if is_marker(object) {
            None
        } else {
            segments.pop()
        };
        let mut dir = &mut root;
        for segment in segments.into_iter().filter(|s| !s.is_empty()) {
            dir = dir.dirs.entry(segment).or_default();
        }
        if let Some(name) = name {
            dir.files.push((name, object));
        }
    }
    let (mut nodes, _) = build(root);
    let only_one = nodes.iter().filter(|n| n.folder).count() == 1;
    for node in nodes.iter_mut().filter(|n| n.folder) {
        if only_one || node.name == project {
            node.open = true;
            open_newest_version(node);
        }
    }
    nodes
}

/// The folder's nodes — folders first, then files, each in natural order —
/// and its totals.
fn build(dir: Dir<'_>) -> (Vec<Node>, Totals) {
    let mut totals = Totals::default();
    let mut folders: Vec<Node> = dir
        .dirs
        .into_iter()
        .map(|(name, child)| {
            let (children, sums) = build(child);
            totals.add(&sums);
            Node {
                name: name.to_string(),
                folder: true,
                open: false,
                files: sums.files,
                size: human_size(sums.size),
                modified: sums.newest.map(when).unwrap_or_default(),
                url: None,
                children,
            }
        })
        .collect();
    folders.sort_by(|a, b| natural_cmp(&a.name, &b.name));
    let mut files: Vec<(&str, &Listed)> = dir.files;
    files.sort_by(|a, b| natural_cmp(a.0, b.0));
    let files = files.into_iter().map(|(name, object)| {
        totals.add(&Totals {
            files: 1,
            size: object.size,
            newest: object.modified,
        });
        Node {
            name: name.to_string(),
            folder: false,
            open: false,
            files: 0,
            size: human_size(object.size),
            modified: object.modified.map(when).unwrap_or_default(),
            url: Some(object.url.clone()),
            children: Vec::new(),
        }
    });
    folders.extend(files);
    (folders, totals)
}

#[derive(Default)]
struct Totals {
    files: usize,
    size: u64,
    newest: Option<i64>,
}

impl Totals {
    fn add(&mut self, other: &Totals) {
        self.files += other.files;
        self.size += other.size;
        self.newest = self.newest.max(other.newest);
    }
}

/// Opens the `v<N>` folder with the highest N, which is the version an upload
/// just wrote unless an older one was re-uploaded.
fn open_newest_version(project: &mut Node) {
    let newest = project
        .children
        .iter_mut()
        .filter(|n| n.folder)
        .filter_map(|n| {
            let version: u32 = n.name.strip_prefix('v')?.parse().ok()?;
            Some((version, n))
        })
        .max_by_key(|(version, _)| *version);
    if let Some((_, node)) = newest {
        node.open = true;
    }
}

fn is_marker(object: &Listed) -> bool {
    object.path.ends_with('/')
}

/// `v2` before `v10`, and `chapter-2` before `chapter-10`: runs of digits
/// compare as numbers, everything else as text.
fn natural_cmp(a: &str, b: &str) -> Ordering {
    let (mut a, mut b) = (a, b);
    loop {
        match (a.is_empty(), b.is_empty()) {
            (true, true) => return Ordering::Equal,
            (true, false) => return Ordering::Less,
            (false, true) => return Ordering::Greater,
            _ => {}
        }
        let (a_run, a_rest) = split_run(a);
        let (b_run, b_rest) = split_run(b);
        let a_digits = a_run.starts_with(|c: char| c.is_ascii_digit());
        let b_digits = b_run.starts_with(|c: char| c.is_ascii_digit());
        let order = if a_digits && b_digits {
            let (a_num, b_num) = (a_run.trim_start_matches('0'), b_run.trim_start_matches('0'));
            a_num
                .len()
                .cmp(&b_num.len())
                .then_with(|| a_num.cmp(b_num))
                .then_with(|| a_run.len().cmp(&b_run.len()))
        } else {
            a_run.to_lowercase().cmp(&b_run.to_lowercase())
        };
        if order != Ordering::Equal {
            return order;
        }
        (a, b) = (a_rest, b_rest);
    }
}

/// The leading run of digits, or of anything else, and what follows it.
fn split_run(text: &str) -> (&str, &str) {
    let digits = text.starts_with(|c: char| c.is_ascii_digit());
    let end = text
        .find(|c: char| c.is_ascii_digit() != digits)
        .unwrap_or(text.len());
    text.split_at(end)
}

/// Decimal units, as Finder counts them.
fn human_size(bytes: u64) -> String {
    const UNITS: [&str; 4] = ["KB", "MB", "GB", "TB"];
    if bytes < 1000 {
        return format!("{bytes} B");
    }
    let mut value = bytes as f64;
    let mut unit = "B";
    for next in UNITS {
        if value < 1000.0 {
            break;
        }
        value /= 1000.0;
        unit = next;
    }
    format!("{value:.1} {unit}")
}

/// Local time, to the minute.
fn when(secs: i64) -> String {
    chrono::DateTime::from_timestamp(secs, 0)
        .map(|t| {
            t.with_timezone(&chrono::Local)
                .format("%Y-%m-%d %H:%M")
                .to_string()
        })
        .unwrap_or_default()
}

/// Whether a pane may hand `url` to `open`: web pages only. The URL comes from
/// a listing, but `open` would just as happily launch a `file://` app.
pub fn openable(url: &str) -> bool {
    url.starts_with("https://") && !url.contains(char::is_whitespace)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn object(path: &str, size: u64, modified: i64) -> Listed {
        Listed {
            path: path.into(),
            size,
            modified: Some(modified),
            url: format!("https://cdn.example.com/socials/{path}"),
        }
    }

    fn names(nodes: &[Node]) -> Vec<&str> {
        nodes.iter().map(|n| n.name.as_str()).collect()
    }

    fn find<'a>(nodes: &'a [Node], name: &str) -> &'a Node {
        nodes
            .iter()
            .find(|n| n.name == name)
            .unwrap_or_else(|| panic!("no {name} in {:?}", names(nodes)))
    }

    #[test]
    fn keys_fold_into_folders_with_files_beneath_them() {
        let tree = tree(
            &[
                object("p1/v1/landscape-aa.mp4", 2_000_000, 100),
                object("p1/v1/chapters/01/c-bb.mp4", 500_000, 300),
                object("p1/v1/transcript.txt", 1_000, 200),
                object("p2/v1/portrait-cc.mp4", 3_000, 50),
            ],
            "",
        );
        assert_eq!(names(&tree), ["p1", "p2"]);
        let p1 = find(&tree, "p1");
        assert!(p1.folder);
        assert_eq!(p1.files, 3);
        assert_eq!(p1.size, "2.5 MB");
        assert_eq!(p1.modified, when(300), "the newest file beneath it");
        let v1 = find(&p1.children, "v1");
        // Folders first, then files.
        assert_eq!(
            names(&v1.children),
            ["chapters", "landscape-aa.mp4", "transcript.txt"]
        );
        let video = find(&v1.children, "landscape-aa.mp4");
        assert!(!video.folder);
        assert_eq!(
            video.url.as_deref(),
            Some("https://cdn.example.com/socials/p1/v1/landscape-aa.mp4")
        );
        assert!(video.children.is_empty());
    }

    #[test]
    fn the_open_project_and_its_newest_version_start_expanded() {
        let objects = [
            object("old/v1/a.mp4", 1, 1),
            object("mine/v2/a.mp4", 1, 1),
            object("mine/v10/a.mp4", 1, 1),
            object("mine/v9/a.mp4", 1, 1),
        ];
        let tree = tree(&objects, "mine");
        assert!(!find(&tree, "old").open);
        let mine = find(&tree, "mine");
        assert!(mine.open);
        assert_eq!(names(&mine.children), ["v2", "v9", "v10"]);
        let open: Vec<_> = mine
            .children
            .iter()
            .filter(|n| n.open)
            .map(|n| n.name.as_str())
            .collect();
        assert_eq!(open, ["v10"], "numbers, not text: v10 is newer than v9");
    }

    #[test]
    fn a_lone_project_opens_whichever_project_is_on_screen() {
        let tree = tree(&[object("only/v1/a.mp4", 1, 1)], "something-else");
        assert!(find(&tree, "only").open);
        assert!(find(&find(&tree, "only").children, "v1").open);
    }

    #[test]
    fn a_folder_marker_makes_a_folder_and_no_file() {
        let tree = tree(&[object("empty/", 0, 1), object("p/v1/a.mp4", 10, 1)], "");
        let empty = find(&tree, "empty");
        assert!(empty.folder);
        assert_eq!(empty.files, 0);
        assert!(empty.children.is_empty());
    }

    #[test]
    fn digits_sort_as_numbers() {
        let mut names = vec!["chapter-10", "chapter-2", "Chapter-1", "v1", "v01", "b"];
        names.sort_by(|a, b| natural_cmp(a, b));
        assert_eq!(
            names,
            ["b", "Chapter-1", "chapter-2", "chapter-10", "v1", "v01"]
        );
    }

    #[test]
    fn sizes_read_like_finder() {
        assert_eq!(human_size(0), "0 B");
        assert_eq!(human_size(999), "999 B");
        assert_eq!(human_size(1_500), "1.5 KB");
        assert_eq!(human_size(52_400_000), "52.4 MB");
        assert_eq!(human_size(4_100_000_000), "4.1 GB");
    }

    #[test]
    fn only_web_pages_are_opened() {
        assert!(openable("https://cdn.saagasolve.dev/socials/p/v1/a.mp4"));
        assert!(!openable("http://cdn.saagasolve.dev/a.mp4"));
        assert!(!openable("file:///Applications/Calculator.app"));
        assert!(!openable("https://x.dev/a b"));
    }

    #[test]
    fn the_pane_counts_files_and_projects_and_keeps_the_tree_through_a_failure() {
        let mut state = FilesState::new();
        state
            .tx
            .send(FilesEvent::Listed(Listing {
                location: "s3://bucket/socials/".into(),
                objects: vec![
                    object("p1/v1/a.mp4", 1_000_000, 1),
                    object("p2/v1/b.mp4", 2_000_000, 1),
                    object("p2/", 0, 1),
                ],
                truncated: false,
            }))
            .unwrap();
        assert!(state.drain());
        let pane = state.pane("p1");
        assert_eq!(pane.location, "s3://bucket/socials/");
        assert_eq!(
            pane.summary.as_deref(),
            Some("2 file(s) · 3.0 MB · 2 project(s)")
        );
        assert!(pane.listed_at.is_some());

        state.tx.send(FilesEvent::Failed("expired".into())).unwrap();
        assert!(state.drain());
        let pane = state.pane("p1");
        assert_eq!(pane.error.as_deref(), Some("expired"));
        assert_eq!(names(&pane.tree), ["p1", "p2"]);
    }

    #[test]
    fn a_second_refresh_waits_for_the_first() {
        let mut state = FilesState::new();
        state.busy = true;
        assert!(!state.refresh());
        assert!(state.pane("").busy);
    }
}
