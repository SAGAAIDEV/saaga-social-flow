//! One complete, immutable artwork set, activated only after every render succeeds.
use super::Card;
use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

pub const MANIFEST: &str = "thumbnails/artwork.json";
/// Where a set's own renders live, one directory per set id.
const SETS_DIR: &str = "thumbnails/sets";
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    Horizontal,
    Vertical,
    Og,
}
impl Kind {
    pub const ALL: [Self; 3] = [Self::Horizontal, Self::Vertical, Self::Og];
    pub fn name(self) -> &'static str {
        match self {
            Self::Horizontal => "horizontal",
            Self::Vertical => "vertical",
            Self::Og => "og",
        }
    }
    /// The one place each destination's size is written down on this side.
    pub fn size(self) -> (u32, u32) {
        match self {
            Self::Horizontal => (1280, 720),
            Self::Vertical => (720, 1280),
            Self::Og => (1200, 630),
        }
    }
    /// The artboard it composes on. The OG image is the horizontal card at
    /// another resolution rather than a second design, so it has none of its own.
    pub fn format(self) -> crate::thumbnail::format::Format {
        match self {
            Self::Vertical => crate::thumbnail::format::Format::Vertical,
            _ => crate::thumbnail::format::Format::Horizontal,
        }
    }
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Asset {
    pub kind: Kind,
    pub file: String,
    pub width: u32,
    pub height: u32,
    pub hash: String,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Set {
    pub id: String,
    pub title: String,
    pub source_hash: String,
    pub card_hash: String,
    pub assets: Vec<Asset>,
}
impl Set {
    /// Freeze upload inputs outside the render cache, which later generation prunes.
    pub fn snapshot(&self, root: &Path, parent: &Path) -> Result<PathBuf> {
        let dir = parent.join(&self.id);
        std::fs::create_dir_all(&dir)?;
        for kind in Kind::ALL {
            std::fs::copy(
                self.path(root, kind)?,
                dir.join(format!("{}.jpg", kind.name())),
            )?;
        }
        Ok(dir)
    }
    pub fn path(&self, root: &Path, kind: Kind) -> Result<PathBuf> {
        let asset = self
            .assets
            .iter()
            .find(|asset| asset.kind == kind)
            .context("artwork format is missing")?;
        // The dimensions were measured off the file by `accept`, so this catches
        // a manifest edited by hand rather than a render that came out wrong.
        if (asset.width, asset.height) != kind.size() {
            bail!("incorrect artwork dimensions");
        }
        let relative = Path::new(&asset.file);
        if relative.is_absolute()
            || relative
                .components()
                .any(|c| !matches!(c, std::path::Component::Normal(_)))
        {
            bail!("invalid artwork path");
        }
        let path = root.join(relative);
        let bytes = std::fs::read(&path).with_context(|| format!("reading {}", path.display()))?;
        if crate::agent::prompt::hash_of_bytes(&bytes) != asset.hash {
            bail!("artwork was changed — generate the set again");
        }
        Ok(path)
    }
}
pub fn load(root: &Path) -> Result<Set> {
    let set: Set = serde_json::from_str(
        &std::fs::read_to_string(root.join(MANIFEST)).context("no artwork set drawn yet")?,
    )?;
    for kind in Kind::ALL {
        set.path(root, kind)?;
    }
    Ok(set)
}
/// Whether an already-loaded set still matches what it was drawn from.
///
/// Split from [`ready`] so a caller holding a set — the Thumbnail pane, which
/// rebuilds on every action — can check it without reading and hashing all three
/// pictures a second time.
pub fn current(root: &Path, set: &Set) -> Result<()> {
    let design = super::load(root);
    if set.card_hash != crate::agent::prompt::hash_of(&design.fingerprint()) {
        bail!("Design changed since the artwork was drawn — redraw it before publishing");
    }
    let photo = super::photo(root).context("The artwork photo is missing")?;
    if set.source_hash != crate::agent::prompt::hash_of_bytes(&std::fs::read(photo)?) {
        bail!("Photo changed since the artwork was drawn — redraw it before publishing");
    }
    Ok(())
}
pub fn ready(root: &Path) -> Result<Set> {
    let set = load(root)?;
    current(root, &set)?;
    Ok(set)
}
/// A picture that is going to be published, by way of [`approved`].
///
/// Every path out of the app — the YouTube upload and Replace thumbnail, the
/// blog, the S3 copy for Buffer — reads the set through here, so nothing goes
/// public that has not been looked at on the Thumbnail tab.
pub fn selected(root: &Path, kind: Kind) -> Result<PathBuf> {
    approved(root)?.path(root, kind)
}

/// Where the Thumbnail tab's Approve is recorded.
pub const APPROVAL: &str = "thumbnails/approval.json";

/// Someone looked at one artwork set and said it can be published.
///
/// Tied to the set's id rather than kept as a flag on the project, so it lapses
/// on its own: a redraw mints a new id, and a new photo or a design edit
/// already fails [`current`]. Nothing has to remember to clear it. Its own file
/// rather than a field in the manifest so that [`Job::commit`] — which replaces
/// the manifest wholesale — never has to know about it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Approval {
    pub set_id: String,
    pub approved_at: String,
}

/// Where the set on disk stands, for the Thumbnail tab and the recording
/// page's pipeline strip.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Review {
    /// No set drawn yet.
    None,
    /// On disk, but drawn from a photo or a design that has since changed.
    Stale,
    /// Current, and waiting to be looked at.
    Drafted,
    /// Current and approved: what every upload will use.
    Approved,
}

pub fn review(root: &Path) -> Review {
    let Ok(set) = load(root) else {
        return Review::None;
    };
    if current(root, &set).is_err() {
        return Review::Stale;
    }
    match is_approved(root, &set) {
        true => Review::Approved,
        false => Review::Drafted,
    }
}

/// The set, when it is current and approved — the only set anything publishes.
pub fn approved(root: &Path) -> Result<Set> {
    let set = ready(root)?;
    if !is_approved(root, &set) {
        bail!("The thumbnail has not been approved — review it on the Thumbnail tab");
    }
    Ok(set)
}

/// Approves the current set. Refuses a stale or missing one: the button that
/// calls this is off in those cases, but a queued click can still arrive.
pub fn approve(root: &Path) -> Result<Approval> {
    let set = ready(root)?;
    let approval = Approval {
        set_id: set.id,
        approved_at: crate::schedule::ledger::now_rfc3339(),
    };
    let path = root.join(APPROVAL);
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, serde_json::to_vec_pretty(&approval)?)?;
    std::fs::rename(&tmp, &path).context("saving the thumbnail approval")?;
    Ok(approval)
}

pub fn approval(root: &Path) -> Option<Approval> {
    serde_json::from_str(&std::fs::read_to_string(root.join(APPROVAL)).ok()?).ok()
}

fn is_approved(root: &Path, set: &Set) -> bool {
    match approval(root) {
        Some(approval) => approval.set_id == set.id,
        // Projects published before approval existed: their picture has already
        // gone public, and without this the blog gate of every one of them would
        // shut on the first launch after the change. The first Approve on such a
        // project writes the file, and from then on the record is what counts.
        None => root.join(crate::publish::UPLOADS_JSONL).is_file(),
    }
}

pub struct Job {
    pub root: PathBuf,
    pub card: Card,
    pub photo: PathBuf,
    pub set: Set,
    pub next: usize,
}
impl Job {
    pub fn new(root: &Path, card: Card) -> Result<Self> {
        super::ready(root, &card)?;
        let photo = super::photo(root)
            .context("Capture your photo first, then generate the artwork set")?;
        let bytes = std::fs::read(&photo)?;
        let source_hash = crate::agent::prompt::hash_of_bytes(&bytes);
        let card_hash = crate::agent::prompt::hash_of(&card.fingerprint());
        let id = crate::agent::prompt::hash_of(&format!(
            "artwork-v1\n{card_hash}\n{source_hash}\n{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)?
                .as_nanos()
        ));
        let dir = root.join(SETS_DIR).join(&id);
        std::fs::create_dir_all(&dir)?;
        // Freeze the source so recapturing mid-job cannot mix different portraits.
        let photo = dir.join("source.jpg");
        std::fs::write(&photo, bytes)?;
        Ok(Self {
            root: root.to_owned(),
            photo,
            set: Set {
                id,
                title: card.title.clone(),
                source_hash,
                card_hash,
                assets: vec![],
            },
            card,
            next: 0,
        })
    }
    pub fn kind(&self) -> Kind {
        Kind::ALL[self.next]
    }
    pub fn page(&self) -> PathBuf {
        self.root
            .join(SETS_DIR)
            .join(&self.set.id)
            .join("card.html")
    }
    /// The design this kind is drawn from: the saved one, on its own artboard.
    pub fn design(&self) -> Card {
        Card {
            format: self.kind().format(),
            ..self.card.clone()
        }
    }
    pub fn accept(&mut self, jpeg: &[u8]) -> Result<bool> {
        if jpeg.is_empty() {
            bail!("renderer returned an empty image");
        }
        let kind = self.kind();
        // Measured off the file rather than restated from the request. Recording
        // `kind.size()` unread would make the manifest's dimensions a copy of
        // what was asked for, and every check of them true by construction.
        let (width, height) = crate::thumbnail::still::jpeg_dimensions(jpeg)
            .context("renderer returned an image with no readable JPEG dimensions")?;
        if (width, height) != kind.size() {
            let (want_w, want_h) = kind.size();
            bail!(
                "the {} artwork drew at {width}x{height}, not {want_w}x{want_h}",
                kind.name()
            );
        }
        let file = format!("{SETS_DIR}/{}/{}.jpg", self.set.id, kind.name());
        std::fs::write(self.root.join(&file), jpeg)?;
        self.set.assets.push(Asset {
            kind,
            file,
            width,
            height,
            hash: crate::agent::prompt::hash_of_bytes(jpeg),
        });
        self.next += 1;
        Ok(self.next == Kind::ALL.len())
    }
    pub fn commit(&self) -> Result<()> {
        for kind in Kind::ALL {
            self.set.path(&self.root, kind)?;
        }
        let tmp = self
            .root
            .join("thumbnails")
            .join(format!("artwork-{}.tmp", self.set.id));
        std::fs::write(&tmp, serde_json::to_vec_pretty(&self.set)?)?;
        std::fs::rename(tmp, self.root.join(MANIFEST)).context("activating artwork set")?;
        self.prune();
        Ok(())
    }
    /// Every set but the one now live.
    ///
    /// Each press mints a fresh id, so without this a project keeps a full set of
    /// renders plus a frozen source per press, on a Drive-backed folder, forever.
    /// After the rename and best-effort on purpose: a directory that will not
    /// delete must not fail a commit that has already succeeded, and nothing is
    /// removed until it is no longer the set anything reads.
    fn prune(&self) {
        let Ok(entries) = std::fs::read_dir(self.root.join(SETS_DIR)) else {
            return;
        };
        for entry in entries.flatten() {
            if entry.file_name().to_string_lossy() == self.set.id {
                continue;
            }
            if let Err(err) = std::fs::remove_dir_all(entry.path()) {
                eprintln!(
                    "stream-recorder: could not remove {}: {err}",
                    entry.path().display()
                );
            }
        }
    }
}

#[cfg(test)]
fn fixture_jpeg(kind: Kind) -> Vec<u8> {
    // Frame header only: these tests exercise persistence, not image decoding.
    let (w, h) = kind.size();
    vec![
        0xff,
        0xd8,
        0xff,
        0xc0,
        0,
        11,
        8,
        (h >> 8) as u8,
        h as u8,
        (w >> 8) as u8,
        w as u8,
        1,
        1,
        0x11,
        0,
        0xff,
        0xd9,
    ]
}

#[cfg(test)]
pub(crate) fn fixture(root: &Path) -> Set {
    std::fs::create_dir_all(root).unwrap();
    crate::thumbnail::still::write_bytes(root, b"photo fixture").unwrap();
    let design = Card {
        title: "A title".into(),
        ..Card::default()
    };
    super::save(root, &design).unwrap();
    let mut job = Job::new(root, design).unwrap();
    for kind in Kind::ALL {
        job.accept(&fixture_jpeg(kind)).unwrap();
    }
    job.commit().unwrap();
    job.set
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!("artwork-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        root
    }

    #[test]
    fn a_drawn_set_waits_for_approval_and_only_then_is_selected() {
        let root = scratch("approve");
        assert_eq!(review(&root), Review::None);
        fixture(&root);
        assert_eq!(review(&root), Review::Drafted);
        assert!(ready(&root).is_ok());
        let err = selected(&root, Kind::Horizontal).unwrap_err();
        assert!(err.to_string().contains("Thumbnail tab"), "{err}");

        let approval = approve(&root).unwrap();
        assert_eq!(approval.set_id, load(&root).unwrap().id);
        assert_eq!(review(&root), Review::Approved);
        assert!(selected(&root, Kind::Horizontal).is_ok());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn an_approval_lapses_on_a_redraw_a_new_photo_and_a_design_edit() {
        let root = scratch("lapse");
        fixture(&root);
        approve(&root).unwrap();
        // A redraw: same photo and design, new set id.
        fixture(&root);
        assert_eq!(review(&root), Review::Drafted);
        assert!(approved(&root).is_err());

        approve(&root).unwrap();
        crate::thumbnail::still::write_bytes(&root, b"another photo").unwrap();
        assert_eq!(review(&root), Review::Stale);
        assert!(approved(&root).is_err());
        assert!(approve(&root).is_err(), "a stale set cannot be approved");

        fixture(&root);
        approve(&root).unwrap();
        let mut design = super::super::load(&root);
        design.title = "Edited".into();
        super::super::save(&root, &design).unwrap();
        assert_eq!(review(&root), Review::Stale);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn a_project_already_on_youtube_counts_as_approved_until_it_is_approved_once() {
        let root = scratch("backfill");
        fixture(&root);
        std::fs::write(root.join(crate::publish::UPLOADS_JSONL), "{}\n").unwrap();
        assert_eq!(review(&root), Review::Approved);
        // Once there is a record, the record decides.
        approve(&root).unwrap();
        fixture(&root);
        assert_eq!(review(&root), Review::Drafted);
        std::fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn incomplete_or_tampered_artwork_cannot_replace_the_previous_set() {
        let root = std::env::temp_dir().join(format!("artwork-atomic-{}", std::process::id()));
        let prior = fixture(&root);
        let mut job = Job::new(
            &root,
            Card {
                title: "Replacement".into(),
                ..Card::default()
            },
        )
        .unwrap();
        assert!(job.accept(b"not an image").is_err());
        assert_eq!(job.next, 0);
        job.accept(&fixture_jpeg(Kind::Horizontal)).unwrap();
        assert!(job.commit().is_err());
        assert_eq!(load(&root).unwrap().id, prior.id);
        job.accept(&fixture_jpeg(Kind::Vertical)).unwrap();
        job.accept(&fixture_jpeg(Kind::Og)).unwrap();
        job.commit().unwrap();
        assert_eq!(load(&root).unwrap().id, job.set.id);
        assert!(ready(&root).is_err(), "new set must match the saved design");
        super::super::save(&root, &job.card).unwrap();
        approve(&root).unwrap();
        let path = selected(&root, Kind::Og).unwrap();
        std::fs::write(path, b"changed").unwrap();
        assert!(load(&root).is_err());
        std::fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn destination_sizes_are_distinct_and_the_source_is_frozen() {
        assert_eq!(Kind::Horizontal.size(), (1280, 720));
        assert_eq!(Kind::Vertical.size(), (720, 1280));
        assert_eq!(Kind::Og.size(), (1200, 630));
        let root = std::env::temp_dir().join(format!("artwork-source-{}", std::process::id()));
        let source = crate::thumbnail::still::write_bytes(&root, b"original").unwrap();
        let job = Job::new(
            &root,
            Card {
                title: "Title".into(),
                ..Card::default()
            },
        )
        .unwrap();
        std::fs::write(source, b"recaptured").unwrap();
        assert_eq!(std::fs::read(job.photo).unwrap(), b"original");
        std::fs::remove_dir_all(root).unwrap();
    }

    /// The OG image is the horizontal card at another resolution, so it draws
    /// from the same artboard. Only the portrait cut composes differently.
    #[test]
    fn only_the_portrait_cut_has_an_artboard_of_its_own() {
        use crate::thumbnail::format::Format;
        assert_eq!(Kind::Horizontal.format(), Format::Horizontal);
        assert_eq!(Kind::Og.format(), Format::Horizontal);
        assert_eq!(Kind::Vertical.format(), Format::Vertical);

        let root = std::env::temp_dir().join(format!("artwork-artboards-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        crate::thumbnail::still::write_bytes(&root, b"photo").unwrap();
        // A portrait project: the picker must not reach the OG image.
        let card = Card {
            title: "Title".into(),
            format: Format::Vertical,
            ..Card::default()
        };
        super::super::save(&root, &card).unwrap();
        let mut job = Job::new(&root, card).unwrap();
        for kind in Kind::ALL {
            assert_eq!(job.design().format, kind.format(), "{}", kind.name());
            job.accept(&fixture_jpeg(kind)).unwrap();
        }
        job.commit().unwrap();
        // And the picker is not part of the set's identity, so flipping it back
        // must not retire artwork that is still exactly right.
        super::super::save(
            &root,
            &Card {
                title: "Title".into(),
                ..Card::default()
            },
        )
        .unwrap();
        assert!(
            ready(&root).is_ok(),
            "the format picker retired a valid set"
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    /// A set is a set: the moment one is live, the ones it replaced are gone.
    #[test]
    fn committing_a_set_takes_the_ones_it_replaced_with_it() {
        let root = std::env::temp_dir().join(format!("artwork-prune-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let first = fixture(&root);
        let snapshot = first.snapshot(&root, &root.join("upload-artwork")).unwrap();
        // Abandoned halfway: a failed run leaves a directory behind too.
        let mut abandoned = Job::new(
            &root,
            Card {
                title: "Abandoned".into(),
                ..Card::default()
            },
        )
        .unwrap();
        abandoned.accept(&fixture_jpeg(Kind::Horizontal)).unwrap();

        let design = Card {
            title: "Second".into(),
            ..Card::default()
        };
        super::super::save(&root, &design).unwrap();
        let mut job = Job::new(&root, design).unwrap();
        for kind in Kind::ALL {
            job.accept(&fixture_jpeg(kind)).unwrap();
        }
        assert_eq!(std::fs::read_dir(root.join(SETS_DIR)).unwrap().count(), 3);
        job.commit().unwrap();

        let left: Vec<String> = std::fs::read_dir(root.join(SETS_DIR))
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(left, vec![job.set.id.clone()]);
        assert!(!root.join(SETS_DIR).join(&first.id).exists());
        for kind in Kind::ALL {
            assert_eq!(
                std::fs::read(snapshot.join(format!("{}.jpg", kind.name()))).unwrap(),
                fixture_jpeg(kind)
            );
        }
        assert!(ready(&root).is_ok(), "the live set survived the sweep");
        std::fs::remove_dir_all(root).unwrap();
    }
}
