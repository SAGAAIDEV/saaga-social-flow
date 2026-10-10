//! YouTube copy belongs to the upload, independently of downstream social posts.
//!
//! The copy is approved on the YouTube tab before it can go up, and an
//! approved copy is locked: nothing rewrites it until someone un-approves it.
use crate::session::Session;
use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

const FILE: &str = "youtube-metadata.json";

/// Where the YouTube tab's Approve is recorded.
pub const APPROVAL: &str = "youtube-approval.json";

/// What a change to approved copy is refused with, wherever it comes from:
/// the YouTube tab's Save, Write, or the Video details tab.
const LOCKED: &str =
    "The title and description are approved — press Un-approve on the YouTube tab to change them";
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Metadata {
    pub title: String,
    #[serde(default)]
    pub description: String,
}

pub fn load(session: &Session) -> Metadata {
    saved(session).unwrap_or_else(|| Metadata {
        title: session.title(),
        description: String::new(),
    })
}

/// The copy someone gave this video — Write's, an edit on the YouTube tab, or
/// an older project's prepared post — and `None` where [`load`] would stand
/// the project's name in for a title nobody wrote.
pub fn saved(session: &Session) -> Option<Metadata> {
    std::fs::read_to_string(session.root.join(FILE))
        .ok()
        .and_then(|text| serde_json::from_str(&text).ok())
        .or_else(|| legacy(session))
}

// Existing projects retain their previously prepared YouTube copy.
fn legacy(session: &Session) -> Option<Metadata> {
    let manifest = crate::posts::load_manifest(&session.posts_dir()).ok()?;
    let post = manifest
        .items
        .iter()
        .find(|item| item.video_id == "longform")?
        .posts
        .iter()
        .find(|post| post.platform == "youtube" || post.platform == "youtube_shorts")?;
    Some(Metadata {
        title: post
            .title
            .clone()
            .filter(|title| !title.trim().is_empty())
            .unwrap_or_else(|| session.title()),
        description: crate::schedule::copy::render_text(&post.content, &post.tags),
    })
}

impl Metadata {
    /// What an approval is tied to: both fields, byte for byte.
    pub fn fingerprint(&self) -> String {
        let mut hasher = Sha256::new();
        hasher.update(self.title.as_bytes());
        hasher.update([0]);
        hasher.update(self.description.as_bytes());
        hasher
            .finalize()
            .iter()
            .take(8)
            .map(|b| format!("{b:02x}"))
            .collect()
    }

    pub fn validate(&self) -> Result<()> {
        if self.title.trim().is_empty() || self.title.chars().count() > 100 {
            bail!("YouTube title must contain 1–100 characters");
        }
        if self.description.chars().count() > 5000 {
            bail!("YouTube description must be at most 5,000 characters");
        }
        Ok(())
    }
}

/// Saves the copy, unless approved copy is there and this would change it.
pub fn save(session: &Session, metadata: &Metadata) -> Result<()> {
    metadata.validate()?;
    ensure_unlocked(session, metadata)?;
    write(session, metadata)
}

/// Refuses `metadata` when it would replace approved copy. Called before
/// anything else is written, so a refused change leaves nothing half-saved.
pub fn ensure_unlocked(session: &Session, metadata: &Metadata) -> Result<()> {
    match approved(session) {
        Some(current) if current != *metadata => bail!(LOCKED),
        _ => Ok(()),
    }
}

/// Someone read this title and description and said they can go up.
///
/// Tied to the copy's fingerprint rather than kept as a flag, so it lapses on
/// its own if the file changes underneath it. Like the thumbnail's
/// (`card::assets::Approval`), it is its own file beside the copy.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Approval {
    pub copy: String,
    pub approved_at: String,
}

impl Approval {
    /// When, local time to the minute; the migration's note as it is.
    pub fn when(&self) -> String {
        chrono::DateTime::parse_from_rfc3339(&self.approved_at)
            .map(|t| {
                t.with_timezone(&chrono::Local)
                    .format("%Y-%m-%d %H:%M")
                    .to_string()
            })
            .unwrap_or_else(|_| self.approved_at.clone())
    }
}

pub fn approval(session: &Session) -> Option<Approval> {
    serde_json::from_str(&std::fs::read_to_string(session.root.join(APPROVAL)).ok()?).ok()
}

/// The saved copy, when it is exactly what was approved — the only copy an
/// upload sends.
pub fn approved(session: &Session) -> Option<Metadata> {
    let copy = saved(session)?;
    match approval(session) {
        Some(approval) => (approval.copy == copy.fingerprint()).then_some(copy),
        // Videos uploaded before approvals existed: their copy went up with
        // them, so it is recorded as approved the first time it is read — a
        // migration, once, tied to that copy.
        None if session.root.join(super::UPLOADS_JSONL).is_file() => {
            record(session, &copy, "before approvals existed".into())
                .ok()
                .map(|_| copy)
        }
        None => None,
    }
}

/// Saves `metadata` and approves it, replacing any earlier approval.
pub fn approve(session: &Session, metadata: &Metadata) -> Result<Approval> {
    metadata.validate()?;
    write(session, metadata)?;
    record(session, metadata, crate::schedule::ledger::now_rfc3339())
}

/// Unlocks the copy for editing. Upload waits until it is approved again.
pub fn unapprove(session: &Session) -> Result<()> {
    match std::fs::remove_file(session.root.join(APPROVAL)) {
        Err(err) if err.kind() != std::io::ErrorKind::NotFound => {
            Err(err).context("removing the YouTube approval")
        }
        _ => Ok(()),
    }
}

fn record(session: &Session, metadata: &Metadata, approved_at: String) -> Result<Approval> {
    let approval = Approval {
        copy: metadata.fingerprint(),
        approved_at,
    };
    let path = session.root.join(APPROVAL);
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, serde_json::to_vec_pretty(&approval)?)?;
    std::fs::rename(&tmp, &path).context("saving the YouTube approval")?;
    Ok(approval)
}

fn write(session: &Session, metadata: &Metadata) -> Result<()> {
    std::fs::create_dir_all(&session.root)?;
    std::fs::write(
        session.root.join(FILE),
        serde_json::to_string_pretty(metadata)?,
    )
    .context("saving YouTube title and description")
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn youtube_copy_can_be_saved_without_social_posts() {
        let root = std::env::temp_dir().join(format!("youtube-metadata-{}", std::process::id()));
        let session = Session {
            dir: root.join("drafts"),
            root: root.clone(),
            version: None,
        };
        let metadata = Metadata {
            title: "A video".into(),
            description: "About the video".into(),
        };
        save(&session, &metadata).unwrap();
        assert_eq!(load(&session), metadata);
        assert!(!session.posts_dir().exists());
        std::fs::remove_dir_all(root).unwrap();
    }
    fn session(name: &str) -> (std::path::PathBuf, Session) {
        let root =
            std::env::temp_dir().join(format!("youtube-approval-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let session = Session {
            dir: root.join("drafts"),
            root: root.clone(),
            version: None,
        };
        (root, session)
    }

    fn copy(title: &str, description: &str) -> Metadata {
        Metadata {
            title: title.into(),
            description: description.into(),
        }
    }

    #[test]
    fn approved_copy_is_locked_until_it_is_unapproved() {
        let (root, session) = session("lock");
        save(&session, &copy("Draft", "First pass")).unwrap();
        assert_eq!(approved(&session), None, "saved is not approved");

        let approval = approve(&session, &copy("Final", "The one")).unwrap();
        assert_eq!(approval.copy, copy("Final", "The one").fingerprint());
        assert_eq!(approved(&session), Some(copy("Final", "The one")));
        // Approving saves what was approved.
        assert_eq!(load(&session), copy("Final", "The one"));

        // Any change is refused, and nothing is written.
        let err = save(&session, &copy("Final", "The one, edited")).unwrap_err();
        assert!(format!("{err}").contains("Un-approve"), "{err}");
        assert_eq!(load(&session), copy("Final", "The one"));
        // Saving the same copy again is no change.
        save(&session, &copy("Final", "The one")).unwrap();
        assert!(approved(&session).is_some());

        unapprove(&session).unwrap();
        assert_eq!(approved(&session), None);
        save(&session, &copy("Final", "The one, edited")).unwrap();
        // Unapproving twice is not an error.
        unapprove(&session).unwrap();
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn an_approval_lapses_when_the_file_changes_underneath_it() {
        let (root, session) = session("lapse");
        approve(&session, &copy("Title", "Words")).unwrap();
        std::fs::write(
            root.join(FILE),
            serde_json::to_string(&copy("Title", "Other words")).unwrap(),
        )
        .unwrap();
        assert_eq!(approved(&session), None);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn invalid_copy_cannot_be_approved() {
        let (root, session) = session("invalid");
        assert!(approve(&session, &copy(" ", "")).is_err());
        assert!(approval(&session).is_none());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn copy_that_already_went_up_counts_as_approved_once() {
        let (root, session) = session("migrated");
        save(&session, &copy("Live", "Already on YouTube")).unwrap();
        std::fs::write(root.join(crate::publish::UPLOADS_JSONL), "{}\n").unwrap();
        assert_eq!(approved(&session), Some(copy("Live", "Already on YouTube")));
        let recorded = approval(&session).unwrap();
        assert_eq!(recorded.approved_at, "before approvals existed");
        assert_eq!(recorded.when(), "before approvals existed");
        // Tied to that copy: a later change does not inherit it.
        std::fs::write(
            root.join(FILE),
            serde_json::to_string(&copy("Live", "Changed since")).unwrap(),
        )
        .unwrap();
        assert_eq!(approved(&session), None);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn the_fingerprint_tells_title_from_description() {
        assert_ne!(copy("ab", "c").fingerprint(), copy("a", "bc").fingerprint());
        assert_eq!(copy("a", "b").fingerprint().len(), 16);
    }

    #[test]
    fn invalid_metadata_is_refused_before_upload() {
        assert!(Metadata {
            title: " ".into(),
            description: String::new()
        }
        .validate()
        .is_err());
        assert!(Metadata {
            title: "x".repeat(101),
            description: String::new()
        }
        .validate()
        .is_err());
        assert!(Metadata {
            title: "Fine".into(),
            description: "x".repeat(5001)
        }
        .validate()
        .is_err());
    }
}
