//! The global style library: reference images every project's thumbnails imitate.
//!
//! Global rather than per-project on purpose — a channel's look is the point, and
//! a per-project folder would drift. `~/.stream-recorder/references/`, overridable
//! by config.
//!
//! Images are **downscaled on ingest**. A reference is sent to the image model on
//! every generation, so a 12 MP photo dropped in once costs money on every run
//! forever. No image model needs more than about a thousand pixels of edge to take
//! a style from a picture.
//!
//! Which references are *active* is part of a thumbnail's identity: the models cap
//! how many they accept, and a set of fifteen is rarely the set you want for one
//! video. [`Library::active_hash`] is what makes changing the selection invalidate
//! the candidates it produced.

use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};

/// Anything larger than this refuses to cross the bridge — a mis-drop of a video
/// file should say so rather than stall.
pub const MAX_BYTES: usize = 10 * 1024 * 1024;

/// Long edge kept on ingest.
pub const MAX_EDGE: u32 = 1024;

/// How many references may be sent at once. Seedream 5 Lite accepts 14; staying
/// under the smallest cap keeps every configured model usable.
pub const MAX_ACTIVE: usize = 12;

pub const SELECTION_JSON: &str = "selection.json";

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
struct Selection {
    /// File names, not paths — the library can move without breaking the choice.
    #[serde(default)]
    active: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Reference {
    pub name: String,
    pub path: PathBuf,
    pub bytes: u64,
    pub active: bool,
}

#[derive(Debug, Clone, PartialEq, Default, Serialize)]
pub struct Library {
    pub root: PathBuf,
    pub items: Vec<Reference>,
}

impl Library {
    pub fn active(&self) -> impl Iterator<Item = &Reference> {
        self.items.iter().filter(|item| item.active)
    }

    /// Identity of the active set, order-independent.
    ///
    /// Reordering the same pictures is not a different style, so the hash sorts
    /// first — otherwise a rename would needlessly invalidate every candidate.
    pub fn active_hash(&self) -> String {
        let mut names: Vec<&str> = self.active().map(|item| item.name.as_str()).collect();
        names.sort_unstable();
        crate::agent::prompt::hash_of(&names.join("\n"))
    }
}

/// Where the library lives. `THUMBNAIL_REFERENCES` overrides the default.
pub fn library_root() -> Result<PathBuf> {
    if let Some(dir) = std::env::var_os("THUMBNAIL_REFERENCES") {
        return Ok(PathBuf::from(dir));
    }
    let home = std::env::var_os("HOME").context("HOME not set")?;
    Ok(PathBuf::from(home)
        .join(".stream-recorder")
        .join("references"))
}

/// Reads the library and which of it is switched on.
pub fn load(root: &Path) -> Library {
    let selection = read_selection(root);
    let Ok(entries) = std::fs::read_dir(root) else {
        return Library {
            root: root.to_path_buf(),
            items: Vec::new(),
        };
    };
    let mut items: Vec<Reference> = entries
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.path())
        .filter(|path| is_image(path))
        .filter_map(|path| {
            let name = path.file_name()?.to_string_lossy().into_owned();
            let bytes = path.metadata().ok()?.len();
            Some(Reference {
                active: selection.active.contains(&name),
                name,
                path,
                bytes,
            })
        })
        .collect();
    items.sort_by(|a, b| a.name.cmp(&b.name));
    Library {
        root: root.to_path_buf(),
        items,
    }
}

/// Switches a reference on or off, refusing to exceed [`MAX_ACTIVE`].
pub fn set_active(root: &Path, name: &str, active: bool) -> Result<()> {
    let mut selection = read_selection(root);
    selection.active.retain(|item| item != name);
    if active {
        if selection.active.len() >= MAX_ACTIVE {
            bail!("at most {MAX_ACTIVE} reference(s) can be active — switch one off first");
        }
        selection.active.push(name.to_string());
    }
    write_selection(root, &selection)
}

/// Checks a dropped image and downscales it, touching no files.
///
/// The expensive half — a Core Image decode, transform and re-encode of up to
/// [`MAX_BYTES`] — and the half with no shared state, so it is what runs off the
/// main thread. See [`crate::thumbnail::spawn_prepare_reference`].
pub fn prepare(name: &str, bytes: &[u8]) -> Result<Vec<u8>> {
    if bytes.len() > MAX_BYTES {
        bail!(
            "{name} is {:.1} MB — references are capped at {} MB",
            bytes.len() as f64 / 1_048_576.0,
            MAX_BYTES / 1_048_576
        );
    }
    if bytes.is_empty() {
        bail!("{name} is empty");
    }
    // Shrink once here rather than pay for the full-size original on every
    // generation. A file Core Image cannot read is stored as-is: refusing it
    // outright would reject formats the image models may still accept.
    Ok(
        match crate::thumbnail::still::shrink(bytes, f64::from(MAX_EDGE)) {
            Ok(smaller) if smaller.len() < bytes.len() => smaller,
            _ => bytes.to_vec(),
        },
    )
}

/// Writes prepared bytes into the library under a name that will not collide.
pub fn store(root: &Path, name: &str, bytes: &[u8]) -> Result<PathBuf> {
    std::fs::create_dir_all(root).with_context(|| format!("creating {}", root.display()))?;
    let path = root.join(unique_name(root, name));
    std::fs::write(&path, bytes).with_context(|| format!("writing {}", path.display()))?;
    Ok(path)
}

pub fn remove(root: &Path, name: &str) -> Result<()> {
    let path = root.join(name);
    std::fs::remove_file(&path).with_context(|| format!("removing {}", path.display()))?;
    let mut selection = read_selection(root);
    selection.active.retain(|item| item != name);
    write_selection(root, &selection)
}

/// A name not already taken, so two files called `ref.jpg` both survive.
fn unique_name(root: &Path, name: &str) -> String {
    let name = sanitise(name);
    if !root.join(&name).exists() {
        return name;
    }
    let (stem, ext) = match name.rsplit_once('.') {
        Some((stem, ext)) => (stem.to_string(), format!(".{ext}")),
        None => (name.clone(), String::new()),
    };
    (2..)
        .map(|n| format!("{stem}-{n}{ext}"))
        .find(|candidate| !root.join(candidate).exists())
        .unwrap_or(name)
}

/// A dropped file names itself, so the name is untrusted: keep it to a plain
/// file name and never let it escape the library directory.
fn sanitise(name: &str) -> String {
    let base = name.rsplit(['/', '\\']).next().unwrap_or(name);
    let cleaned: String = base
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || "-_. ".contains(c) {
                c
            } else {
                '-'
            }
        })
        .collect();
    let cleaned = cleaned.trim_matches(['.', ' ']).to_string();
    if cleaned.is_empty() {
        "reference.jpg".to_string()
    } else {
        cleaned
    }
}

fn is_image(path: &Path) -> bool {
    path.extension()
        .and_then(|ext| ext.to_str())
        .map(|ext| {
            matches!(
                ext.to_ascii_lowercase().as_str(),
                "jpg" | "jpeg" | "png" | "webp"
            )
        })
        .unwrap_or(false)
}

fn read_selection(root: &Path) -> Selection {
    std::fs::read_to_string(root.join(SELECTION_JSON))
        .ok()
        .and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or_default()
}

fn write_selection(root: &Path, selection: &Selection) -> Result<()> {
    std::fs::create_dir_all(root).with_context(|| format!("creating {}", root.display()))?;
    let path = root.join(SELECTION_JSON);
    std::fs::write(
        &path,
        serde_json::to_string_pretty(selection).context("serializing selection")? + "\n",
    )
    .with_context(|| format!("writing {}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp(tag: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("stream-recorder-refs-{}-{tag}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn images_are_listed_and_other_files_ignored() {
        let dir = temp("list");
        std::fs::write(dir.join("a.jpg"), b"x").unwrap();
        std::fs::write(dir.join("b.PNG"), b"x").unwrap();
        std::fs::write(dir.join("notes.txt"), b"x").unwrap();
        let library = load(&dir);
        let names: Vec<&str> = library.items.iter().map(|r| r.name.as_str()).collect();
        assert_eq!(names, vec!["a.jpg", "b.PNG"]);
        assert!(
            library.active().next().is_none(),
            "nothing is on by default"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_selection_survives_a_reload() {
        let dir = temp("select");
        std::fs::write(dir.join("a.jpg"), b"x").unwrap();
        set_active(&dir, "a.jpg", true).unwrap();
        assert_eq!(load(&dir).active().count(), 1);
        set_active(&dir, "a.jpg", false).unwrap();
        assert_eq!(load(&dir).active().count(), 0);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Every configured model has to accept the set, so the cap is the smallest.
    #[test]
    fn the_active_set_is_capped() {
        let dir = temp("cap");
        for n in 0..MAX_ACTIVE + 1 {
            std::fs::write(dir.join(format!("r{n:02}.jpg")), b"x").unwrap();
        }
        for n in 0..MAX_ACTIVE {
            set_active(&dir, &format!("r{n:02}.jpg"), true).unwrap();
        }
        let err = set_active(&dir, &format!("r{MAX_ACTIVE:02}.jpg"), true).unwrap_err();
        assert!(err.to_string().contains("at most"));
        assert_eq!(load(&dir).active().count(), MAX_ACTIVE);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Reordering the same pictures is not a different style.
    #[test]
    fn the_active_hash_ignores_order_but_not_membership() {
        let dir = temp("hash");
        std::fs::write(dir.join("a.jpg"), b"x").unwrap();
        std::fs::write(dir.join("b.jpg"), b"x").unwrap();
        set_active(&dir, "b.jpg", true).unwrap();
        set_active(&dir, "a.jpg", true).unwrap();
        let one = load(&dir).active_hash();

        // Same two, switched on in the other order.
        set_active(&dir, "a.jpg", false).unwrap();
        set_active(&dir, "b.jpg", false).unwrap();
        set_active(&dir, "a.jpg", true).unwrap();
        set_active(&dir, "b.jpg", true).unwrap();
        assert_eq!(load(&dir).active_hash(), one);

        set_active(&dir, "b.jpg", false).unwrap();
        assert_ne!(load(&dir).active_hash(), one, "membership changed");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_second_file_of_the_same_name_does_not_clobber_the_first() {
        let dir = temp("collide");
        let first = store(&dir, "ref.jpg", b"one").unwrap();
        let second = store(&dir, "ref.jpg", b"two").unwrap();
        assert_ne!(first, second);
        assert_eq!(std::fs::read(&first).unwrap(), b"one");
        assert_eq!(second.file_name().unwrap(), "ref-2.jpg");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A dropped file names itself, so the name is untrusted input.
    #[test]
    fn a_hostile_name_cannot_escape_the_library() {
        assert_eq!(sanitise("../../../etc/passwd"), "passwd");
        assert_eq!(sanitise("/tmp/evil.jpg"), "evil.jpg");
        assert_eq!(sanitise("..."), "reference.jpg");
        assert_eq!(sanitise(""), "reference.jpg");
        assert!(!sanitise("a/b:c*.jpg").contains('/'));
    }

    #[test]
    fn oversized_and_empty_drops_are_refused_with_a_reason() {
        // Refused before anything touches the library, which is what lets this
        // half run on a worker thread.
        let huge = vec![0u8; MAX_BYTES + 1];
        let err = prepare("huge.jpg", &huge).unwrap_err();
        assert!(err.to_string().contains("capped at"));
        assert!(prepare("empty.jpg", b"")
            .unwrap_err()
            .to_string()
            .contains("empty"));
    }

    #[test]
    fn removing_a_reference_also_deactivates_it() {
        let dir = temp("remove");
        std::fs::write(dir.join("a.jpg"), b"x").unwrap();
        set_active(&dir, "a.jpg", true).unwrap();
        remove(&dir, "a.jpg").unwrap();
        let library = load(&dir);
        assert!(library.items.is_empty());
        assert_eq!(library.active_hash(), crate::agent::prompt::hash_of(""));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_missing_library_reads_as_empty_rather_than_failing() {
        let library = load(&PathBuf::from("/nonexistent/stream-recorder-refs"));
        assert!(library.items.is_empty());
        assert_eq!(library.active().count(), 0);
    }
}
