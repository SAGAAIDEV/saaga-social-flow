//! Optional portrait video: the vertical longform, as the page's mobile player.
//!
//! Two sources, in order. The YouTube Short, when the vertical cut went up with
//! the longform — see [`crate::publish::short`] — which the page embeds the way
//! it embeds the landscape video and which costs the CMS nothing to hold.
//! Failing that, the file itself, uploaded into the media library: what this did
//! before the Short existed. Its designed poster is supplied by the artwork set
//! either way.
//!
//! And a third outcome, which is the common one for a real take: **no portrait
//! player at all**. The vertical longform is the same chapters as the landscape
//! one, so past three minutes it is not a Short — see
//! [`crate::publish::SHORT_MAX_SECONDS`] — and a ten-minute portrait render is
//! over a gigabyte, past what one request to the CMS can carry. That used to be
//! a hard failure *after* the thumbnail and the figures were already in the
//! media library: the post the button was pressed for never went up, over a
//! player the page renders fine without. The cut is optional everywhere else,
//! so it is optional here: decided for free before anything is uploaded, said
//! on the status line, and carried onto the ledger row as a warning.
use std::path::PathBuf;

use super::{payload, strapi};
use crate::session::Session;

/// What the publish will do about the portrait cut, decided off the network.
#[derive(Debug, Clone, PartialEq)]
pub(super) enum Plan {
    /// No portrait player. The reason, when there was a file that is not going
    /// up; `None` when the project simply has no vertical cut.
    Skip(Option<String>),
    /// Embed the Short already on YouTube.
    Short(payload::VerticalCut),
    /// Upload this file into the CMS's media library.
    Upload(PathBuf),
}

impl Plan {
    /// Why nothing is going up, when that is worth saying.
    pub(super) fn reason(&self) -> Option<&str> {
        match self {
            Plan::Skip(reason) => reason.as_deref(),
            _ => None,
        }
    }

    /// The cut as the body can carry it before any upload has happened: the
    /// Short, or nothing yet.
    pub(super) fn embedded(&self) -> Option<payload::VerticalCut> {
        match self {
            Plan::Short(cut) => Some(cut.clone()),
            _ => None,
        }
    }
}

/// What the upload left behind: the cut for `videoVertical`, and a note for
/// the ledger row when the cut was meant to go up and did not.
#[derive(Debug, Clone, PartialEq, Default)]
pub(super) struct Outcome {
    pub cut: Option<payload::VerticalCut>,
    pub note: Option<String>,
}

/// The cut as a Short, when one has gone up. No network: a ledger read.
pub(super) fn from_youtube(session: &Session) -> Option<payload::VerticalCut> {
    crate::publish::short(session).map(|short| payload::VerticalCut {
        url: short.url,
        video_id: Some(short.video_id),
    })
}

/// Decides, for free, what the publish will do about the portrait cut.
///
/// `targets` are the Render boxes: a vertical longform whose box is off is not
/// uploaded even when an earlier render left the file behind. A Short already
/// on YouTube is embedded regardless — it went up when the box was on, and the
/// page loses nothing by pointing at it. A file past the CMS upload cap is a
/// skip with the sizes in it, rather than a failure after the other uploads.
pub(super) fn plan(session: &Session, targets: crate::config::RenderTargets) -> Plan {
    if let Some(short) = from_youtube(session) {
        return Plan::Short(short);
    }
    let video = session.render_dir().join("vertical/longform.mp4");
    if !video.is_file() {
        return Plan::Skip(None);
    }
    if !targets.vertical {
        return Plan::Skip(Some(
            "the vertical longform is switched off above the Render button — publishing without \
             the portrait player"
                .to_string(),
        ));
    }
    if let Some(why) = strapi::video_upload_block(&video) {
        return Plan::Skip(Some(format!(
            "{why} — publishing without the portrait player"
        )));
    }
    Plan::Upload(video)
}

/// Carries out the plan. Never fails: the portrait player is optional, and by
/// the time this runs the thumbnail has already gone up through the same
/// client, so a refusal here is about this file and not about the CMS. The
/// page renders one player at every width without it.
pub(super) fn upload(client: &strapi::Strapi, plan: Plan, status: impl Fn(String)) -> Outcome {
    match plan {
        Plan::Short(cut) => {
            status("Embedding the vertical cut from YouTube…".into());
            Outcome {
                cut: Some(cut),
                note: None,
            }
        }
        Plan::Skip(reason) => {
            if let Some(why) = &reason {
                eprintln!("stream-recorder: {why}");
            }
            Outcome {
                cut: None,
                note: reason,
            }
        }
        Plan::Upload(video) => {
            status("Uploading the vertical cut…".into());
            match client.upload_video(&video) {
                Ok(uploaded) => Outcome {
                    cut: Some(payload::VerticalCut {
                        url: uploaded.url,
                        video_id: None,
                    }),
                    note: None,
                },
                Err(err) => {
                    let why = format!(
                        "the vertical cut did not upload ({err:#}) — published without the \
                         portrait player"
                    );
                    eprintln!("stream-recorder: {why}");
                    status(format!("{why}."));
                    Outcome {
                        cut: None,
                        note: Some(why),
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn session(tag: &str) -> Session {
        let root = std::env::temp_dir().join(format!("blog-vertical-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        Session {
            root: root.clone(),
            dir: root.join("drafts"),
            version: None,
        }
    }

    fn on() -> crate::config::RenderTargets {
        crate::config::RenderTargets {
            horizontal: true,
            vertical: true,
            shorts: true,
        }
    }

    /// No portrait cut is the common case and it has to cost nothing: no
    /// upload, no artwork lookup, no error, no note. The post then renders one
    /// player at every width, exactly as it did before this module existed.
    #[test]
    fn a_project_with_no_vertical_cut_uploads_nothing() {
        let session = session("none");
        let plan = plan(&session, on());
        assert_eq!(plan, Plan::Skip(None));
        assert!(from_youtube(&session).is_none());
        // Reaching the network at all is the failure here: a horizontal-only
        // project is a normal project, not a degraded one.
        let got = upload(&strapi::Strapi::for_test(), plan, |_| {});
        assert_eq!(got, Outcome::default());
        let _ = std::fs::remove_dir_all(&session.root);
    }

    /// A Short in the ledger is the vertical video, and the CMS is never sent
    /// the file: the client here points nowhere, so an upload would fail.
    #[test]
    fn a_short_on_youtube_is_embedded_rather_than_uploaded() {
        let session = session("short");
        std::fs::write(
            session.root.join(crate::publish::UPLOADS_JSONL),
            concat!(
                r#"{"video_id":"long1","url":"https://www.youtube.com/watch?v=long1","title":"T","#,
                r#""source_hash":"a","uploaded_at":"2026-09-08T12:00:00Z","orientation":"horizontal"}"#,
                "\n",
                r#"{"video_id":"short1","url":"https://www.youtube.com/shorts/short1","title":"T","#,
                r#""source_hash":"b","uploaded_at":"2026-09-08T12:05:00Z","orientation":"vertical"}"#,
                "\n",
            ),
        )
        .unwrap();
        // Even with a file on disk: the Short went up, and it is the cut.
        std::fs::create_dir_all(session.render_dir().join("vertical")).unwrap();
        std::fs::write(session.render_dir().join("vertical/longform.mp4"), b"v").unwrap();
        let plan = plan(&session, on());
        let embedded = plan
            .embedded()
            .expect("the short is known before any upload");
        assert_eq!(embedded.video_id.as_deref(), Some("short1"));
        let got = upload(&strapi::Strapi::for_test(), plan, |_| {})
            .cut
            .expect("the short is the vertical video");
        assert_eq!(got.video_id.as_deref(), Some("short1"));
        assert_eq!(got.url, "https://www.youtube.com/shorts/short1");
        let _ = std::fs::remove_dir_all(&session.root);
    }

    /// A vertical longform on disk from before the box was unticked stays on
    /// disk and off the wire, and the row says so.
    #[test]
    fn a_switched_off_vertical_longform_is_not_uploaded() {
        let session = session("off");
        std::fs::create_dir_all(session.render_dir().join("vertical")).unwrap();
        std::fs::write(session.render_dir().join("vertical/longform.mp4"), b"v").unwrap();
        let off = crate::config::RenderTargets {
            vertical: false,
            ..on()
        };
        let plan = plan(&session, off);
        assert!(plan.reason().unwrap().contains("switched off"), "{plan:?}");
        let got = upload(&strapi::Strapi::for_test(), plan, |_| {});
        assert!(got.cut.is_none());
        assert!(got.note.unwrap().contains("switched off"));
        let _ = std::fs::remove_dir_all(&session.root);
    }

    /// The case that was failing the whole publish: a ten-minute portrait render
    /// is over a gigabyte, and the CMS upload was refusing it after the
    /// thumbnail and figures were already up. Now it is decided before any
    /// upload, names both sizes, and the post goes up without it.
    #[test]
    fn an_oversize_vertical_longform_is_skipped_before_anything_is_uploaded() {
        let session = session("oversize");
        std::fs::create_dir_all(session.render_dir().join("vertical")).unwrap();
        // Sparse: the length without the bytes.
        let file =
            std::fs::File::create(session.render_dir().join("vertical/longform.mp4")).unwrap();
        file.set_len(1260 * 1_048_576).unwrap();
        drop(file);
        let plan = plan(&session, on());
        let why = plan.reason().expect("skipped").to_string();
        assert!(why.contains("1260 MB"), "{why}");
        assert!(why.contains("512 MB"), "{why}");
        assert!(why.contains("without the portrait player"), "{why}");
        assert!(plan.embedded().is_none());
        let got = upload(&strapi::Strapi::for_test(), plan, |_| {});
        assert!(got.cut.is_none());
        assert_eq!(got.note.as_deref(), Some(why.as_str()));
        let _ = std::fs::remove_dir_all(&session.root);
    }

    /// A cut that fits is uploaded — and when the upload itself fails, the
    /// post still goes up: the client here points nowhere, and that is a note
    /// on the row rather than the end of the publish.
    #[test]
    fn a_failed_vertical_upload_is_a_note_rather_than_a_failure() {
        let session = session("refused");
        std::fs::create_dir_all(session.render_dir().join("vertical")).unwrap();
        let video = session.render_dir().join("vertical/longform.mp4");
        std::fs::write(&video, b"v").unwrap();
        let plan = plan(&session, on());
        assert_eq!(plan, Plan::Upload(video));
        let lines = std::cell::RefCell::new(Vec::new());
        let got = upload(&strapi::Strapi::for_test(), plan, |line| {
            lines.borrow_mut().push(line)
        });
        assert!(got.cut.is_none());
        assert!(got.note.unwrap().contains("did not upload"));
        assert!(lines
            .borrow()
            .iter()
            .any(|line| line.contains("Uploading the vertical cut")));
        let _ = std::fs::remove_dir_all(&session.root);
    }
}
