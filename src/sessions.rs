//! The catalogue of recording projects on disk.
//!
//! [`crate::session::Session`] is the take you are working in *now*;
//! this module is the set of them: everything under
//! `~/.stream-recorder/sessions/`, newest first, so the UI can offer a project
//! picker instead of silently minting a fresh timestamp on every launch.
//!
//! A folder keeps its timestamp name forever — it is the stable identity that
//! paths, the S3 keys and `schedule.jsonl` are all written against. A friendly
//! name lives beside it in `session.json` and is presentation only, so renaming
//! a project can never orphan its files.

use std::path::{Path, PathBuf};
use std::time::SystemTime;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

pub const SESSION_JSON: &str = "session.json";

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct SessionMeta {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
}

/// One recording project as the picker sees it.
#[derive(Debug, Clone, PartialEq)]
pub struct SessionEntry {
    pub root: PathBuf,
    /// The folder name, e.g. "2026-08-15_02-19-20" — the durable identity.
    pub folder: String,
    pub name: Option<String>,
    pub files: usize,
    pub bytes: u64,
    pub modified: SystemTime,
}

impl SessionEntry {
    /// What to call this project: the given name, else its folder timestamp.
    pub fn title(&self) -> &str {
        self.name
            .as_deref()
            .map(str::trim)
            .filter(|name| !name.is_empty())
            .unwrap_or(&self.folder)
    }

    /// A dropdown row. An unnamed project is parenthesised so a named list does
    /// not read as though the timestamps were chosen.
    pub fn label(&self) -> String {
        let title = match &self.name {
            Some(name) if !name.trim().is_empty() => name.trim().to_string(),
            _ => format!("({})", self.folder),
        };
        format!("{title} — {} files, {}", self.files, human_bytes(self.bytes))
    }

    pub fn is_empty(&self) -> bool {
        self.files == 0
    }
}

pub fn sessions_root() -> Result<PathBuf> {
    let home = std::env::var_os("HOME").context("HOME not set")?;
    Ok(PathBuf::from(home)
        .join(".stream-recorder")
        .join("sessions"))
}

/// Every project with files in it, newest first.
///
/// Empty folders are omitted deliberately: a launch that recorded nothing leaves
/// one behind, and listing them buries the real work. They are left on disk
/// rather than deleted — this module never removes anything.
pub fn list() -> Vec<SessionEntry> {
    let Ok(root) = sessions_root() else {
        return Vec::new();
    };
    let Ok(entries) = std::fs::read_dir(&root) else {
        return Vec::new();
    };
    let mut out: Vec<SessionEntry> = entries
        .filter_map(|entry| entry.ok())
        .filter(|entry| entry.path().is_dir())
        .filter_map(|entry| describe(&entry.path()))
        .filter(|entry| !entry.is_empty())
        .collect();
    out.sort_by(|a, b| b.modified.cmp(&a.modified).then(b.folder.cmp(&a.folder)));
    out
}

/// What the picker should show: [`list`], plus `open` itself even when it is
/// still empty.
///
/// A project you just made has no files yet, so [`list`] omits it — leaving the
/// picker showing some other project's name while the window title and the name
/// field both say otherwise. Every caller that populates the picker and every
/// caller that resolves a picked row must use this same list, or the row indices
/// the UI sends back would address the wrong project.
pub fn list_including(open: &Path) -> Vec<SessionEntry> {
    merge_open(list(), describe(open))
}

/// Splices the open project into the listing, keeping it newest-first. A project
/// already listed — the usual case, once it holds a recording — is left alone so
/// it cannot appear twice.
fn merge_open(mut listed: Vec<SessionEntry>, open: Option<SessionEntry>) -> Vec<SessionEntry> {
    let Some(open) = open else {
        return listed;
    };
    if listed.iter().any(|entry| entry.root == open.root) {
        return listed;
    }
    let at = listed
        .iter()
        .position(|other| open.modified > other.modified)
        .unwrap_or(listed.len());
    listed.insert(at, open);
    listed
}

/// The most recently touched non-empty project, if there is one.
pub fn latest() -> Option<SessionEntry> {
    list().into_iter().next()
}

pub fn describe(root: &Path) -> Option<SessionEntry> {
    let folder = root.file_name()?.to_string_lossy().into_owned();
    let measured = measure(root);
    // The newest file inside, not the folder's own mtime. A directory's mtime
    // only tracks its *direct* children, and every recording write lands in
    // `drafts/` — so the root's timestamp does not move at all during a take, and
    // "resume the most recent project" was really resuming whichever one last had
    // a top-level folder created in it. An empty project has no files to ask, so
    // it falls back to the folder.
    let modified = measured.newest.unwrap_or_else(|| {
        std::fs::metadata(root)
            .and_then(|meta| meta.modified())
            .unwrap_or(SystemTime::UNIX_EPOCH)
    });
    Some(SessionEntry {
        root: root.to_path_buf(),
        folder,
        name: load_name(root),
        files: measured.files,
        bytes: measured.bytes,
        modified,
    })
}

pub fn load_name(root: &Path) -> Option<String> {
    let text = std::fs::read_to_string(root.join(SESSION_JSON)).ok()?;
    let meta: SessionMeta = serde_json::from_str(&text).ok()?;
    meta.name
        .map(|name| name.trim().to_string())
        .filter(|name| !name.is_empty())
}

/// Names a project. An empty name clears it back to the folder timestamp.
pub fn save_name(root: &Path, name: &str) -> Result<()> {
    std::fs::create_dir_all(root).with_context(|| format!("creating {}", root.display()))?;
    let name = name.trim();
    let meta = SessionMeta {
        name: (!name.is_empty()).then(|| name.to_string()),
    };
    let path = root.join(SESSION_JSON);
    std::fs::write(
        &path,
        serde_json::to_string_pretty(&meta).context("serializing session meta")? + "\n",
    )
    .with_context(|| format!("writing {}", path.display()))
}

/// What one walk of a project folder found.
#[derive(Debug, Default, Clone, PartialEq)]
struct Measured {
    files: usize,
    bytes: u64,
    /// When the project was last actually written to, from the newest file in it.
    newest: Option<SystemTime>,
}

/// Counts files and bytes, ignoring anything unreadable, and notes the newest
/// write. `session.json` itself is not counted, so naming an empty project does
/// not make it look used — but it *does* count toward `newest`, because naming a
/// project is working on it.
fn measure(root: &Path) -> Measured {
    let mut out = Measured::default();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.filter_map(|entry| entry.ok()) {
            let path = entry.path();
            let Ok(meta) = entry.metadata() else { continue };
            if meta.is_dir() {
                stack.push(path);
                continue;
            }
            if !meta.is_file() {
                continue;
            }
            let name = path.file_name().unwrap_or_default().to_string_lossy();
            if name == ".DS_Store" {
                continue;
            }
            if let Ok(at) = meta.modified() {
                out.newest = Some(out.newest.map_or(at, |seen| seen.max(at)));
            }
            if name == SESSION_JSON {
                continue;
            }
            out.files += 1;
            out.bytes += meta.len();
        }
    }
    out
}

/// Deletes session folders holding no files whatsoever, except `keep`.
///
/// [`Session::create`](crate::session::Session::create) makes the folder when the
/// button is pressed, so every New Project that did not go on to record leaves
/// one behind — sixteen of them accumulated here in two days. They are invisible
/// in the picker, which is exactly why nobody ever cleans them up.
///
/// "No files whatsoever" is stricter than [`SessionEntry::is_empty`], which
/// ignores `session.json`: a folder that has been *named* is one someone meant to
/// keep, however little is in it yet. Returns how many went.
pub fn sweep_empty(keep: &Path) -> usize {
    match sessions_root() {
        Ok(root) => sweep_in(&root, keep),
        Err(_) => 0,
    }
}

fn sweep_in(root: &Path, keep: &Path) -> usize {
    let Ok(entries) = std::fs::read_dir(root) else {
        return 0;
    };
    let mut swept = 0;
    for entry in entries.filter_map(|entry| entry.ok()) {
        let path = entry.path();
        if !path.is_dir() || path == keep {
            continue;
        }
        let found = measure(&path);
        if found.newest.is_some() {
            continue;
        }
        match std::fs::remove_dir_all(&path) {
            Ok(()) => swept += 1,
            Err(err) => eprintln!(
                "stream-recorder: could not remove empty project {}: {err}",
                path.display()
            ),
        }
    }
    swept
}

pub fn human_bytes(bytes: u64) -> String {
    const KB: f64 = 1024.0;
    let bytes = bytes as f64;
    if bytes < KB {
        return format!("{bytes:.0} B");
    }
    const UNITS: [&str; 4] = ["KB", "MB", "GB", "TB"];
    let mut value = bytes / KB;
    let mut unit = 0;
    while value >= KB && unit + 1 < UNITS.len() {
        value /= KB;
        unit += 1;
    }
    format!("{value:.1} {}", UNITS[unit])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "stream-recorder-sessions-{}-{tag}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn entry(name: Option<&str>, folder: &str, files: usize, bytes: u64) -> SessionEntry {
        SessionEntry {
            root: PathBuf::from("/tmp").join(folder),
            folder: folder.into(),
            name: name.map(str::to_string),
            files,
            bytes,
            modified: SystemTime::UNIX_EPOCH,
        }
    }

    #[test]
    fn a_named_project_shows_its_name() {
        let named = entry(Some("Rust error handling"), "2026-08-15_02-19-20", 149, 731_000_000);
        assert_eq!(named.title(), "Rust error handling");
        assert_eq!(named.label(), "Rust error handling — 149 files, 697.1 MB");
    }

    /// An unnamed project must not look like someone chose that timestamp.
    #[test]
    fn an_unnamed_project_falls_back_to_its_folder_in_parentheses() {
        let bare = entry(None, "2026-08-15_02-11-21", 18, 1_024);
        assert_eq!(bare.title(), "2026-08-15_02-11-21");
        assert!(bare.label().starts_with("(2026-08-15_02-11-21) —"));
    }

    #[test]
    fn a_blank_name_is_treated_as_unnamed() {
        let blank = entry(Some("   "), "2026-08-15_02-11-21", 3, 10);
        assert_eq!(blank.title(), "2026-08-15_02-11-21");
        assert!(blank.label().starts_with('('));
    }

    #[test]
    fn a_name_round_trips_through_session_json() {
        let dir = temp("name");
        assert_eq!(load_name(&dir), None);
        save_name(&dir, "  Buffer queue walkthrough  ").unwrap();
        assert_eq!(load_name(&dir).as_deref(), Some("Buffer queue walkthrough"));
        // Clearing it goes back to unnamed rather than storing an empty string.
        save_name(&dir, "").unwrap();
        assert_eq!(load_name(&dir), None);
        let _ = std::fs::remove_dir_all(&dir);
    }

    fn aged(folder: &str, secs: u64) -> SessionEntry {
        SessionEntry {
            modified: SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(secs),
            ..entry(None, folder, 1, 1)
        }
    }

    /// The point of the whole function: a project made seconds ago holds no files,
    /// so `list` drops it — and the picker would show someone else's name.
    #[test]
    fn a_brand_new_empty_project_still_reaches_the_picker() {
        let listed = vec![aged("older", 100)];
        let open = SessionEntry {
            files: 0,
            bytes: 0,
            ..aged("newest", 500)
        };
        let merged = merge_open(listed, Some(open));
        assert_eq!(merged.len(), 2);
        assert_eq!(merged[0].folder, "newest");
    }

    #[test]
    fn an_already_listed_project_is_not_duplicated() {
        let listed = vec![aged("a", 200), aged("b", 100)];
        let merged = merge_open(listed.clone(), Some(listed[1].clone()));
        assert_eq!(merged, listed);
    }

    #[test]
    fn the_open_project_lands_in_newest_first_order() {
        let listed = vec![aged("a", 300), aged("b", 100)];
        let merged = merge_open(listed, Some(aged("middle", 200)));
        let order: Vec<&str> = merged.iter().map(|e| e.folder.as_str()).collect();
        assert_eq!(order, ["a", "middle", "b"]);
    }

    #[test]
    fn measuring_ignores_the_meta_file_so_naming_does_not_fake_usage() {
        let dir = temp("measure");
        save_name(&dir, "Just a name").unwrap();
        let named = measure(&dir);
        assert_eq!((named.files, named.bytes), (0, 0));
        // Uncounted, but it still marks the project as touched — naming one is
        // working on it, and a named folder must never be swept.
        assert!(named.newest.is_some());
        assert!(describe(&dir).unwrap().is_empty());
        std::fs::write(dir.join("chapter-01.mp4"), b"0123456789").unwrap();
        let used = measure(&dir);
        assert_eq!((used.files, used.bytes), (1, 10));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn measuring_walks_nested_directories() {
        let dir = temp("nested");
        std::fs::create_dir_all(dir.join("drafts/v1")).unwrap();
        std::fs::write(dir.join("drafts/v1/a.mp4"), b"aaaa").unwrap();
        std::fs::write(dir.join("drafts/v1/b.mp4"), b"bb").unwrap();
        std::fs::write(dir.join("notes.json"), b"n").unwrap();
        let found = measure(&dir);
        assert_eq!((found.files, found.bytes), (3, 7));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The bug this closes: a directory's mtime only tracks its direct children,
    /// so a project written to only inside `drafts/` looked untouched since the
    /// day its folders were made, and "resume the latest" resumed the wrong one.
    #[test]
    fn a_project_is_dated_by_its_newest_file_not_its_folder() {
        let dir = temp("dated");
        std::fs::create_dir_all(dir.join("drafts/v1")).unwrap();
        let folder_made = std::fs::metadata(&dir).unwrap().modified().unwrap();
        std::thread::sleep(std::time::Duration::from_millis(1100));
        std::fs::write(dir.join("drafts/v1/chapter-01.mp4"), b"take").unwrap();

        let entry = describe(&dir).unwrap();
        assert!(
            entry.modified > folder_made,
            "the take is newer than the folder that has not changed since it was made"
        );
        assert_eq!(
            std::fs::metadata(&dir).unwrap().modified().unwrap(),
            folder_made,
            "writing inside drafts/ really does leave the root untouched"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// New Project makes the folder on the press, so one that never recorded is
    /// litter. Sixteen had piled up in two days, invisible in the picker.
    #[test]
    fn sweeping_takes_the_folders_nothing_was_ever_written_to() {
        let root = temp("sweep");
        let abandoned = root.join("2026-08-14_03-00-44");
        let recorded = root.join("2026-08-15_18-19-05");
        let named = root.join("2026-08-15_20-00-00");
        let open = root.join("2026-08-16_09-00-00");
        for dir in [&abandoned, &recorded, &named, &open] {
            std::fs::create_dir_all(dir.join("drafts/v1")).unwrap();
        }
        std::fs::write(recorded.join("drafts/v1/chapter-01.mp4"), b"take").unwrap();
        // Named but empty: someone meant to keep this one.
        save_name(&named, "Tomorrow's video").unwrap();

        assert_eq!(sweep_in(&root, &open), 1);
        assert!(!abandoned.exists(), "the abandoned folder is gone");
        assert!(recorded.exists(), "a folder with a take survives");
        assert!(named.exists(), "naming a project keeps it");
        assert!(open.exists(), "the open project is never swept");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn bytes_read_in_the_unit_a_human_would_use() {
        assert_eq!(human_bytes(512), "512 B");
        assert_eq!(human_bytes(2_048), "2.0 KB");
        assert_eq!(human_bytes(5_242_880), "5.0 MB");
        assert_eq!(human_bytes(2_147_483_648), "2.0 GB");
        assert_eq!(human_bytes(731_000_000), "697.1 MB");
        assert_eq!(human_bytes(0), "0 B");
        // The largest unit holds rather than overflowing into nonsense.
        assert_eq!(human_bytes(5 * 1024_u64.pow(4)), "5.0 TB");
    }
}
