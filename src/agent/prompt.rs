//! Which system prompt a generative step actually ran with — and which version
//! of it.
//!
//! A project overrides any builtin preamble by dropping
//! `{root}/prompts/{prompt_id}.txt` beside its media, and a *standing* override
//! lives in the prompt library at `~/.stream-recorder/prompts/` — see
//! [`library_dir`]. That much is just a file read. The rest of this module
//! exists because a reflection loop that rewrites prompts has to answer *"did
//! that change help?"*, and it cannot if the only record is "some overlay was in
//! effect".
//!
//! So every resolution carries a version: the builtin is v0, each applied overlay
//! is v1, v2… recorded in `{root}/prompts/versions.jsonl`, and a file edited by
//! hand outside that ledger resolves as *unversioned* rather than being mistaken
//! for the version it replaced. The content hash is always present, so
//! attribution never fails even when the version label is unknown.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

pub const NOTES: &str = "notes.slide_deck";
pub const TITLES: &str = "titles.chapter_cards";
pub const POSTS: &str = "posts.social";
pub const SUBSTACK: &str = "substack.notes";
pub const BLOG: &str = "blog.article";
pub const REFLECT: &str = "reflect.prompts";
/// Figure captions. Its own prompt rather than a section of [`BLOG`]: there is
/// one overlay file per id, so tuning how a caption reads would otherwise
/// retune the whole article voice — see [`crate::figure::blurb`].
pub const FIGURE: &str = "blog.figure";

/// The environment override for [`library_dir`], mirroring
/// `THUMBNAIL_REFERENCES` on the style library.
pub const LIBRARY_ENV: &str = "STREAM_RECORDER_PROMPTS";

pub const PROMPTS_DIR: &str = "prompts";
pub const VERSIONS_JSONL: &str = "versions.jsonl";
/// Where a replaced overlay is archived. Part of the apply path below.
#[allow(dead_code)]
pub const HISTORY_DIR: &str = "history";

/// One applied overlay, in the order it was applied.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct VersionRow {
    pub prompt_id: String,
    /// 1-based. v0 is the builtin and is never written here.
    pub version: u32,
    pub hash: String,
    pub applied_at: String,
    /// "reflect" | "hand" — who proposed it.
    pub source: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

/// The preamble a step ran with, and what it was.
#[derive(Debug, Clone, PartialEq)]
pub struct Resolved {
    pub text: String,
    /// `Some(0)` builtin, `Some(n)` a recorded overlay, `None` an overlay edited
    /// outside the ledger — knowable only by its hash.
    pub version: Option<u32>,
    pub hash: String,
}

/// The write half of versioning — apply, archive, and the labels a report needs.
///
/// Nothing calls these yet: they exist for the Reflect stage, which proposes an
/// overlay and needs to record it as a version with the replaced text recoverable.
/// Kept together rather than deferred so the read side resolving a version has a
/// writer that can actually produce one.
impl Resolved {
    #[allow(dead_code)]
    pub fn is_builtin(&self) -> bool {
        self.version == Some(0)
    }

    /// How to name this version in a report: "v2", or "unversioned (a1b2…)".
    #[allow(dead_code)]
    pub fn label(&self) -> String {
        match self.version {
            Some(n) => format!("v{n}"),
            None => format!("unversioned ({})", &self.hash[..self.hash.len().min(8)]),
        }
    }
}

/// The compiled-in preamble for a prompt id.
///
/// The single place that maps an id to its default text, so a diff can show what
/// a proposed rewrite would replace even when nothing has overridden it yet.
pub fn builtin(prompt_id: &str) -> Option<&'static str> {
    match prompt_id {
        NOTES => Some(super::notes::SYSTEM),
        TITLES => Some(super::titles::SYSTEM),
        POSTS => Some(crate::posts::generate::SYSTEM_PROMPT),
        SUBSTACK => Some(crate::substack::generate::SYSTEM_PROMPT),
        BLOG => Some(crate::blog::generate::SYSTEM_PROMPT),
        FIGURE => Some(crate::figure::blurb::SYSTEM_PROMPT),
        _ => None,
    }
}

/// The standing prompt library, shared by every project.
///
/// `{project}/prompts/` is per-project, and [`crate::session::Session::create`]
/// mints a fresh timestamped folder for every recording — so a preamble tuned
/// there is thrown away the moment the next video starts. A prompt is a house
/// style, and a house style that resets every video is not one.
///
/// `~/.stream-recorder/prompts/`, overridable by [`LIBRARY_ENV`], for the reason
/// [`crate::thumbnail::references`] gives about the picture library: the channel's
/// voice is the point, and a per-project copy of it drifts.
pub fn library_dir() -> Option<PathBuf> {
    if let Some(dir) = std::env::var_os(LIBRARY_ENV) {
        return Some(PathBuf::from(dir));
    }
    let home = std::env::var_os("HOME")?;
    Some(PathBuf::from(home).join(".stream-recorder").join(PROMPTS_DIR))
}

/// Where a standing override for `prompt_id` lives, whether or not it is there
/// yet — "the file to edit" is worth naming before it exists.
pub fn library_overlay(prompt_id: &str) -> Option<PathBuf> {
    Some(library_dir()?.join(format!("{prompt_id}.txt")))
}

/// The library overlay for `prompt_id`, seeded from the builtin if absent.
///
/// Seeding freezes the builtin: later improvements to the shipped default stop
/// reaching anyone holding a copy, which is the failure `config::RETIRED` exists
/// to undo. So this is never called on a generation path — only behind a button
/// that says it is about to happen.
pub fn ensure_library_overlay(prompt_id: &str, builtin: &str) -> Result<PathBuf> {
    let path = library_overlay(prompt_id)
        .context("no HOME, so there is nowhere to keep a prompt library")?;
    seed_overlay(&path, builtin)?;
    Ok(path)
}

/// Writes `builtin` to `path` unless something is already there.
///
/// Split out so the "never clobber" rule is testable without setting a process
/// environment variable — which every other test in this file reads through
/// [`library_dir`], and which would race them.
fn seed_overlay(path: &Path, builtin: &str) -> Result<()> {
    if path.is_file() {
        return Ok(());
    }
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    }
    std::fs::write(path, format!("{}\n", builtin.trim()))
        .with_context(|| format!("writing {}", path.display()))
}

/// What `prompt_id` would run with right now — overlay if there is one, builtin
/// otherwise. `None` for an id this program does not have.
pub fn live(prompt_id: &str, root: Option<&Path>) -> Option<Resolved> {
    builtin(prompt_id).map(|text| resolve(prompt_id, text, root))
}

/// [`live`] with the library directory passed in — the same seam as
/// [`resolve_in`], for the same reason, one level up.
///
/// A pane that showed "v0 (builtin)" could only be tested by a developer who
/// had never saved a house prompt, because `live` reaches for `$HOME` and finds
/// the real library. Panes take the directory so their tests can hand over an
/// empty one.
pub(crate) fn live_in(
    prompt_id: &str,
    root: Option<&Path>,
    library: Option<&Path>,
) -> Option<Resolved> {
    builtin(prompt_id).map(|text| resolve_in(prompt_id, text, root, library))
}

/// Resolves the preamble for `prompt_id`: project overlay, then the standing
/// library, then the builtin.
///
/// The project wins because it is the narrower, more deliberate statement — and
/// because it is what Reflect writes, so an approved rewrite must not be
/// outranked by a house prompt someone set months ago.
pub fn resolve(prompt_id: &str, builtin: &str, root: Option<&Path>) -> Resolved {
    resolve_in(prompt_id, builtin, root, library_dir().as_deref())
}

/// [`resolve`] with the library directory passed in rather than read from the
/// environment.
///
/// Tests call this. `resolve` reaching for `$HOME` means a real
/// `~/.stream-recorder/prompts/posts.social.txt` on a developer's machine would
/// otherwise decide the result of a unit test — which is both a flake and,
/// worse, a green suite that proves nothing about precedence.
pub(crate) fn resolve_in(
    prompt_id: &str,
    builtin: &str,
    root: Option<&Path>,
    library: Option<&Path>,
) -> Resolved {
    let sources = [
        ("project", root.map(prompts_dir)),
        ("library", library.map(Path::to_path_buf)),
    ];
    for (source, dir) in sources {
        let Some(path) = dir.map(|dir| dir.join(format!("{prompt_id}.txt"))) else {
            continue;
        };
        let Some(text) = read_overlay(&path) else {
            continue;
        };
        let hash = hash_of(&text);
        let version = overlay_version(prompt_id, &hash, root, library);
        eprintln!(
            "stream-recorder: {prompt_id} using {source} overlay {} → {}",
            version
                .map(|n| format!("v{n}"))
                .unwrap_or_else(|| "unversioned".into()),
            path.display()
        );
        return Resolved {
            text,
            version,
            hash,
        };
    }
    builtin_version(builtin)
}

/// A trimmed, non-empty overlay, or nothing.
///
/// An emptied file means "go back to the default" — a preamble of no words is
/// not a preamble, and running with one would silently change every generation
/// rather than restoring it.
fn read_overlay(path: &Path) -> Option<String> {
    let text = std::fs::read_to_string(path).ok()?;
    let trimmed = text.trim();
    (!trimmed.is_empty()).then(|| trimmed.to_string())
}

/// The recorded version of an overlay's exact text, from either ledger.
///
/// An overlay whose hash is in neither was edited by hand. Reporting it as the
/// version it replaced would attribute work to a prompt that never ran, so it
/// stays unversioned — the hash is always there, so attribution never fails,
/// only the label does.
fn overlay_version(
    prompt_id: &str,
    hash: &str,
    root: Option<&Path>,
    library: Option<&Path>,
) -> Option<u32> {
    // The project keeps its ledger under `prompts/`; the library *is* that
    // directory, so its ledger sits directly inside it.
    let project = root.map(load_versions).unwrap_or_default();
    let shared = library
        .map(|dir| read_versions(&dir.join(VERSIONS_JSONL)))
        .unwrap_or_default();
    project
        .iter()
        .chain(shared.iter())
        .filter(|row| row.prompt_id == prompt_id && row.hash == hash)
        .map(|row| row.version)
        .max()
}

fn builtin_version(builtin: &str) -> Resolved {
    Resolved {
        text: builtin.to_string(),
        version: Some(0),
        hash: hash_of(builtin),
    }
}

pub fn prompts_dir(root: &Path) -> PathBuf {
    root.join(PROMPTS_DIR)
}

/// FNV-1a/64, fixed by specification so a hash written today still matches after
/// a toolchain upgrade. std's `DefaultHasher` is explicitly unspecified across
/// releases and must never key anything persisted.
pub fn hash_of(text: &str) -> String {
    hash_of_bytes(text.as_bytes())
}

/// The same hash over arbitrary bytes — image stills and reference sets are
/// content-addressed with it, so a regenerated thumbnail from the same inputs is
/// recognisably the same work.
pub fn hash_of_bytes(bytes: &[u8]) -> String {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    format!("{hash:016x}")
}

pub fn load_versions(root: &Path) -> Vec<VersionRow> {
    read_versions(&prompts_dir(root).join(VERSIONS_JSONL))
}

fn read_versions(path: &Path) -> Vec<VersionRow> {
    let Ok(text) = std::fs::read_to_string(path) else {
        return Vec::new();
    };
    text.lines()
        .filter(|line| !line.trim().is_empty())
        .filter_map(|line| serde_json::from_str(line).ok())
        .collect()
}

/// The version an overlay would become if applied now.
#[allow(dead_code)]
pub fn next_version(rows: &[VersionRow], prompt_id: &str) -> u32 {
    rows.iter()
        .filter(|row| row.prompt_id == prompt_id)
        .map(|row| row.version)
        .max()
        .unwrap_or(0)
        + 1
}

/// Writes an overlay, archives whatever it replaced, and records the new version.
///
/// The archive is not optional: a prompt is the program's behaviour, so reverting
/// must never depend on the model having been right.
#[allow(dead_code)]
pub fn apply(
    root: &Path,
    prompt_id: &str,
    text: &str,
    source: &str,
    note: Option<&str>,
    applied_at: &str,
) -> Result<VersionRow> {
    let dir = prompts_dir(root);
    let history = dir.join(HISTORY_DIR);
    std::fs::create_dir_all(&history)
        .with_context(|| format!("creating {}", history.display()))?;

    let live = dir.join(format!("{prompt_id}.txt"));
    if let Ok(previous) = std::fs::read_to_string(&live) {
        let archived = history.join(format!("{prompt_id}.{applied_at}.txt"));
        std::fs::write(&archived, previous)
            .with_context(|| format!("archiving {}", archived.display()))?;
    }

    let text = text.trim();
    std::fs::write(&live, format!("{text}\n"))
        .with_context(|| format!("writing {}", live.display()))?;

    let row = VersionRow {
        prompt_id: prompt_id.to_string(),
        version: next_version(&load_versions(root), prompt_id),
        hash: hash_of(text),
        applied_at: applied_at.to_string(),
        source: source.to_string(),
        note: note.map(str::to_string),
    };
    append_version(root, &row)?;
    Ok(row)
}

#[allow(dead_code)]
fn append_version(root: &Path, row: &VersionRow) -> Result<()> {
    use std::io::Write;

    let path = prompts_dir(root).join(VERSIONS_JSONL);
    let line = serde_json::to_string(row).context("serializing prompt version")?;
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .with_context(|| format!("opening {}", path.display()))?;
    writeln!(file, "{line}").with_context(|| format!("appending to {}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "stream-recorder-prompt-{}-{tag}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join(PROMPTS_DIR)).unwrap();
        dir
    }

    /// Resolve with no library, so a real `~/.stream-recorder/prompts` on the
    /// machine running the tests cannot decide the answer. The library's own
    /// behaviour is pinned by the three tests at the end, against a temp dir.
    fn only_project(prompt_id: &str, builtin: &str, root: Option<&Path>) -> Resolved {
        resolve_in(prompt_id, builtin, root, None)
    }

    #[test]
    fn no_overlay_resolves_to_the_builtin_as_v0() {
        let got = only_project(TITLES, "builtin", None);
        assert_eq!(got.text, "builtin");
        assert!(got.is_builtin());
        assert_eq!(got.label(), "v0");
        assert_eq!(got.hash, hash_of("builtin"));
    }

    #[test]
    fn an_applied_overlay_wins_and_carries_its_version() {
        let dir = temp("applied");
        let row = apply(
            &dir,
            TITLES,
            " repaired ",
            "reflect",
            Some("why"),
            "2026-08-16T09:00:00Z",
        )
        .unwrap();
        assert_eq!(row.version, 1);
        let got = only_project(TITLES, "builtin", Some(&dir));
        assert_eq!(got.text, "repaired");
        assert_eq!(got.version, Some(1));
        assert_eq!(got.label(), "v1");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn versions_climb_per_prompt_and_archive_what_they_replace() {
        let dir = temp("climb");
        apply(&dir, POSTS, "first", "reflect", None, "2026-08-16T09:00:00Z").unwrap();
        let second =
            apply(&dir, POSTS, "second", "reflect", None, "2026-08-17T09:00:00Z").unwrap();
        assert_eq!(second.version, 2);
        // A different prompt starts its own count.
        let other = apply(&dir, TITLES, "t", "hand", None, "2026-08-17T09:00:00Z").unwrap();
        assert_eq!(other.version, 1);
        // The replaced text stays recoverable.
        let archived = dir
            .join(PROMPTS_DIR)
            .join(HISTORY_DIR)
            .join(format!("{POSTS}.2026-08-17T09:00:00Z.txt"));
        assert_eq!(std::fs::read_to_string(archived).unwrap().trim(), "first");
        assert_eq!(only_project(POSTS, "builtin", Some(&dir)).version, Some(2));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The point of hashing: a file edited outside the ledger must not be
    /// attributed to the version it overwrote.
    #[test]
    fn a_hand_edited_overlay_resolves_as_unversioned() {
        let dir = temp("handedit");
        apply(&dir, POSTS, "generated", "reflect", None, "2026-08-16T09:00:00Z").unwrap();
        assert_eq!(only_project(POSTS, "builtin", Some(&dir)).version, Some(1));

        std::fs::write(
            dir.join(PROMPTS_DIR).join(format!("{POSTS}.txt")),
            "tweaked by hand",
        )
        .unwrap();
        let got = only_project(POSTS, "builtin", Some(&dir));
        assert_eq!(got.version, None, "not v1 — v1 never said this");
        assert_eq!(got.text, "tweaked by hand");
        assert!(got.label().starts_with("unversioned ("));
        assert_eq!(got.hash, hash_of("tweaked by hand"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn an_empty_overlay_falls_back_to_the_builtin() {
        let dir = temp("empty");
        std::fs::write(dir.join(PROMPTS_DIR).join(format!("{TITLES}.txt")), "  \n ").unwrap();
        assert!(only_project(TITLES, "builtin", Some(&dir)).is_builtin());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Persisted keys cannot move when the toolchain does.
    #[test]
    fn the_hash_is_pinned_by_known_answers() {
        assert_eq!(hash_of(""), "cbf29ce484222325");
        assert_eq!(hash_of("a"), "af63dc4c8601ec8c");
        assert_eq!(hash_of("foobar"), "85944171f73967e8");
        assert_ne!(hash_of("prompt one"), hash_of("prompt two"));
    }

    #[test]
    fn next_version_counts_per_prompt() {
        let row = |prompt_id: &str, version: u32, hash: &str| VersionRow {
            prompt_id: prompt_id.into(),
            version,
            hash: hash.into(),
            applied_at: "t".into(),
            source: "reflect".into(),
            note: None,
        };
        let rows = vec![row(POSTS, 1, "x"), row(POSTS, 2, "y")];
        assert_eq!(next_version(&rows, POSTS), 3);
        assert_eq!(next_version(&rows, TITLES), 1);
        assert_eq!(next_version(&[], NOTES), 1);
    }

    /// A bare library directory, no project inside it.
    fn library(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "stream-recorder-prompt-lib-{}-{tag}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// The whole point of the library tier: a preamble tuned once outlives the
    /// project it was tuned in, because the next recording is a different folder.
    #[test]
    fn a_library_overlay_beats_the_builtin() {
        let lib = library("beats-builtin");
        std::fs::write(lib.join(format!("{SUBSTACK}.txt")), " house voice \n").unwrap();
        let project = temp("lib-vs-builtin");

        let got = resolve_in(SUBSTACK, "builtin", Some(&project), Some(&lib));
        assert_eq!(got.text, "house voice");
        assert_eq!(got.version, None, "hand-edited, so unversioned by hash");
        assert_eq!(got.hash, hash_of("house voice"));
        // And it reaches a project that has no prompts folder of its own at all.
        assert_eq!(
            resolve_in(SUBSTACK, "builtin", None, Some(&lib)).text,
            "house voice"
        );
        let _ = std::fs::remove_dir_all(&lib);
        let _ = std::fs::remove_dir_all(&project);
    }

    /// The project is the narrower statement, and it is what Reflect writes — an
    /// approved rewrite must not be outranked by a house prompt set months ago.
    #[test]
    fn a_project_overlay_still_beats_the_library() {
        let lib = library("project-wins");
        std::fs::write(lib.join(format!("{POSTS}.txt")), "house voice").unwrap();
        let project = temp("project-wins");
        apply(&project, POSTS, "this project only", "reflect", None, "2026-08-16T09:00:00Z")
            .unwrap();

        let got = resolve_in(POSTS, "builtin", Some(&project), Some(&lib));
        assert_eq!(got.text, "this project only");
        assert_eq!(got.version, Some(1), "and keeps its recorded version");
        let _ = std::fs::remove_dir_all(&lib);
        let _ = std::fs::remove_dir_all(&project);
    }

    /// The no-op case, which is what the three existing prompt ids see until
    /// someone deliberately creates a library file.
    #[test]
    fn an_absent_library_changes_nothing() {
        let lib = library("absent");
        let _ = std::fs::remove_dir_all(&lib);
        let project = temp("absent");
        assert!(resolve_in(TITLES, "builtin", Some(&project), Some(&lib)).is_builtin());
        assert!(resolve_in(TITLES, "builtin", None, Some(&lib)).is_builtin());
        let _ = std::fs::remove_dir_all(&project);
    }

    /// An emptied library file means "go back to the default". A preamble of no
    /// words is not a preamble.
    #[test]
    fn an_emptied_library_overlay_falls_back_to_the_builtin() {
        let lib = library("emptied");
        std::fs::write(lib.join(format!("{POSTS}.txt")), "  \n\t ").unwrap();
        assert!(resolve_in(POSTS, "builtin", None, Some(&lib)).is_builtin());
        let _ = std::fs::remove_dir_all(&lib);
    }

    /// A library overlay recorded in the library's own ledger reports its
    /// version, so a house prompt is as attributable as a project one.
    #[test]
    fn a_library_overlay_reads_its_version_from_the_library_ledger() {
        let lib = library("ledger");
        std::fs::write(lib.join(format!("{SUBSTACK}.txt")), "house voice").unwrap();
        let row = VersionRow {
            prompt_id: SUBSTACK.into(),
            version: 4,
            hash: hash_of("house voice"),
            applied_at: "2026-08-16T09:00:00Z".into(),
            source: "hand".into(),
            note: None,
        };
        std::fs::write(
            lib.join(VERSIONS_JSONL),
            serde_json::to_string(&row).unwrap() + "\n",
        )
        .unwrap();
        assert_eq!(
            resolve_in(SUBSTACK, "builtin", None, Some(&lib)).version,
            Some(4)
        );
        let _ = std::fs::remove_dir_all(&lib);
    }

    /// Seeding is a deliberate act — it freezes the builtin — so it happens once
    /// and never overwrites what is already there.
    #[test]
    fn seeding_the_library_writes_the_builtin_once() {
        let lib = library("seed");
        let path = lib.join("nested").join(format!("{SUBSTACK}.txt"));
        seed_overlay(&path, "  the builtin  ").unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "the builtin\n");

        std::fs::write(&path, "since edited").unwrap();
        seed_overlay(&path, "the builtin").unwrap();
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "since edited",
            "an existing prompt is never clobbered by the default"
        );
        // And a seeded file is what the resolver then picks up.
        assert_eq!(
            resolve_in(SUBSTACK, "builtin", None, path.parent()).text,
            "since edited"
        );
        let _ = std::fs::remove_dir_all(&lib);
    }
}
