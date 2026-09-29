//! The team template in the window, and the YouTube details pushed live.
//!
//! The template — funnel links, the YouTube description footer — is the
//! team's, in S3 (see [`crate::team`]). It is read at launch and on Reload, on
//! a thread, since S3 is a network call and the window must not wait on it.
//! The links box on Socials → Generate posts and the footer box on the YouTube
//! tab are two editors of the one file: each save sends the whole template,
//! with the other half as it was last read, conditional on the file not having
//! changed since. A teammate's save in between comes back as a conflict, and
//! the box keeps what was typed so nothing is lost while reloading.
//!
//! Saving the video details on the YouTube tab also changes them on YouTube
//! when the video is already up — see [`App::push_youtube_details`].

use std::sync::mpsc::{self, Receiver, TryRecvError};

use super::App;
use crate::publish::metadata::Metadata;
use crate::team::{self, Loaded, Saved, Source, TeamTemplate};

pub(super) enum TeamEvent {
    Loaded(Loaded),
    Saved(Result<Saved, String>),
    /// The YouTube details update finished: the video id, and how it went.
    YoutubeDetails(String, Result<(), String>),
}

#[derive(Default)]
pub(super) struct TeamState {
    /// The template as last read or saved; `None` until the first read lands.
    loaded: Option<Loaded>,
    /// The one team job in flight: a load or a save.
    job: Option<Receiver<TeamEvent>>,
    /// The YouTube details update in flight, separate so a template save and a
    /// details save do not wait on each other.
    youtube: Option<Receiver<TeamEvent>>,
}

/// What the YouTube tab's footer section shows.
#[derive(serde::Serialize)]
pub(super) struct FooterView {
    pub footer: String,
    pub status: String,
    pub loaded: bool,
}

/// "3 links, saved by andrew on 2026-09-29", or why the copy is local.
fn describe(loaded: &Loaded) -> String {
    let template = &loaded.template;
    let count = match template.links.len() {
        0 => "No team links yet".to_string(),
        1 => "1 team link".to_string(),
        n => format!("{n} team links"),
    };
    let by = match (template.updated_by.as_str(), template.updated_at.get(..10)) {
        ("", _) | (_, None) => String::new(),
        (who, Some(day)) => format!(", saved by {who} on {day}"),
    };
    match &loaded.source {
        Source::Team => format!("{count}{by}."),
        Source::Cached(why) => format!(
            "{count} from this Mac's copy{by} — S3 could not be read ({why}). Reload to try \
             again; saving needs the team file."
        ),
        Source::Empty => format!("No team template yet, and S3 could not be read. {count}."),
    }
}

impl App {
    /// Read the team template on a thread; [`App::drain_team`] takes it.
    pub(super) fn load_team_template(&mut self) {
        if self.team.job.is_some() {
            return;
        }
        let (tx, rx) = mpsc::channel();
        match std::thread::Builder::new()
            .name("team-template".into())
            .spawn(move || {
                let _ = tx.send(TeamEvent::Loaded(team::load()));
            }) {
            Ok(_) => self.team.job = Some(rx),
            Err(err) => self.set_team_status(&format!("Could not read the team template: {err}")),
        }
    }

    /// Save for team on the links box.
    pub(super) fn save_team_links(&mut self, text: &str) {
        let Some(base) = self.team_base() else {
            return;
        };
        let links = match team::parse(text) {
            Ok(links) => links,
            Err(errors) => {
                self.set_team_status(&format!("Not saved — {}.", errors.join("; ")));
                return;
            }
        };
        if let Err(err) = team::ensure_not_emptied(&links, base.template.links.len()) {
            self.set_team_status(&format!("Not saved — {err:#}."));
            return;
        }
        let template = TeamTemplate {
            links,
            ..base.template.clone()
        };
        self.start_team_save(template, base.etag.clone());
    }

    /// Save for team on the YouTube tab's footer box.
    pub(super) fn save_team_footer(&mut self, footer: &str) {
        let Some(base) = self.team_base() else {
            return;
        };
        let template = TeamTemplate {
            youtube_footer: footer.trim().to_string(),
            ..base.template.clone()
        };
        self.start_team_save(template, base.etag.clone());
    }

    /// The template a save starts from, or `None` — said on the line — when
    /// there is nothing safe to save over yet.
    fn team_base(&self) -> Option<Loaded> {
        if self.team.job.is_some() {
            self.set_team_status("The team template is still loading or saving — a moment.");
            return None;
        }
        let Some(loaded) = self.team.loaded.clone() else {
            self.set_team_status("The team template has not loaded yet — press Reload.");
            return None;
        };
        // A local copy has no ETag to be conditional on; saving it could
        // replace a newer team file with an old one.
        if !matches!(loaded.source, Source::Team) {
            self.set_team_status(
                "Not saved — the team file could not be read, so this is this Mac's copy. \
                 Reload once S3 can be reached (aws sso login), then save.",
            );
            return None;
        }
        Some(loaded)
    }

    fn start_team_save(&mut self, template: TeamTemplate, etag: Option<String>) {
        let (tx, rx) = mpsc::channel();
        match std::thread::Builder::new()
            .name("team-template-save".into())
            .spawn(move || {
                let result = team::save(template, etag.as_deref()).map_err(|e| format!("{e:#}"));
                let _ = tx.send(TeamEvent::Saved(result));
            }) {
            Ok(_) => {
                self.team.job = Some(rx);
                self.set_team_status("Saving the team template…");
            }
            Err(err) => self.set_team_status(&format!("Could not save: {err}")),
        }
    }

    /// Saving the video details, when the longform is already on YouTube,
    /// changes its title and description there too. On a thread: it is two
    /// calls to YouTube. The Short keeps its own copy and is left alone.
    pub(super) fn push_youtube_details(&mut self, metadata: &Metadata) {
        let Some(upload) = crate::publish::longform(&self.session) else {
            return;
        };
        if self.team.youtube.is_some() {
            self.set_publish_line(
                "Still updating YouTube with the last save — save again once it lands.",
            );
            return;
        }
        let (tx, rx) = mpsc::channel();
        let (title, description) = (metadata.title.clone(), metadata.description.clone());
        let video_id = upload.video_id.clone();
        match std::thread::Builder::new()
            .name("youtube-details".into())
            .spawn(move || {
                let result = crate::publish::youtube::access_token()
                    .and_then(|token| {
                        crate::publish::youtube::update_details(
                            &token,
                            &video_id,
                            &title,
                            &description,
                        )
                    })
                    .map_err(|e| format!("{e:#}"));
                let _ = tx.send(TeamEvent::YoutubeDetails(video_id, result));
            }) {
            Ok(_) => {
                self.team.youtube = Some(rx);
                self.set_publish_line("Video details saved — updating them on YouTube…");
            }
            Err(err) => {
                self.set_publish_line(&format!("Saved here, but YouTube was not updated: {err}"))
            }
        }
    }

    pub(super) fn drain_team(&mut self) {
        for slot in [0, 1] {
            let rx = if slot == 0 {
                &self.team.job
            } else {
                &self.team.youtube
            };
            let Some(rx) = rx.as_ref() else {
                continue;
            };
            let event = match rx.try_recv() {
                Ok(event) => Some(event),
                Err(TryRecvError::Empty) => continue,
                Err(TryRecvError::Disconnected) => None,
            };
            if slot == 0 {
                self.team.job = None;
            } else {
                self.team.youtube = None;
            }
            match event {
                Some(event) => self.team_event(event),
                None => self.set_team_status("The team template job stopped without saying why."),
            }
        }
    }

    fn team_event(&mut self, event: TeamEvent) {
        match event {
            TeamEvent::Loaded(loaded) => {
                if let Some(live) = self.live.as_ref() {
                    live.control_target
                        .set_team_links_text(&team::to_lines(&loaded.template.links));
                }
                let line = describe(&loaded);
                self.team.loaded = Some(loaded);
                self.set_team_status(&line);
            }
            TeamEvent::Saved(Ok(Saved::Saved(loaded))) => {
                if let Some(live) = self.live.as_ref() {
                    live.control_target
                        .set_team_links_text(&team::to_lines(&loaded.template.links));
                }
                let line = format!("Saved for the team. {}", describe(&loaded));
                self.team.loaded = Some(loaded);
                self.set_team_status(&line);
            }
            // The box is left as typed: that is the change to make again after
            // reloading, and overwriting it here would lose it.
            TeamEvent::Saved(Ok(Saved::Conflict)) => self.set_team_status(
                "Not saved — a teammate changed the team template since this Mac read it. Copy \
                 your change, press Reload, then make it again.",
            ),
            TeamEvent::Saved(Err(err)) => self.set_team_status(&format!("Not saved: {err}")),
            TeamEvent::YoutubeDetails(id, Ok(())) => self.set_publish_line(&format!(
                "Video details saved, and updated on YouTube (https://youtu.be/{id})."
            )),
            TeamEvent::YoutubeDetails(_, Err(err)) => self.set_publish_line(&format!(
                "Video details saved here, but YouTube was not updated: {err}"
            )),
        }
        // The footer box is on the YouTube page, which is drawn whole.
        self.update_publish_summary();
    }

    /// What the YouTube page's footer section shows.
    pub(super) fn footer_view(&self) -> FooterView {
        match self.team.loaded.as_ref() {
            Some(loaded) => FooterView {
                footer: loaded.template.youtube_footer.clone(),
                status: describe(loaded),
                loaded: matches!(loaded.source, Source::Team),
            },
            None => FooterView {
                footer: String::new(),
                status: "Loading the team template…".into(),
                loaded: false,
            },
        }
    }

    /// The team template as last read, for the copy the render writes.
    pub(super) fn team_template(&self) -> TeamTemplate {
        self.team
            .loaded
            .as_ref()
            .map(|loaded| loaded.template.clone())
            .unwrap_or_default()
    }

    /// The team line is the posts status: that is where the links box is. The
    /// footer box's own page shows the same line when it is next drawn.
    fn set_team_status(&self, text: &str) {
        if let Some(live) = self.live.as_ref() {
            live.control_target
                .set_posts_status(&format!("Team template: {text}"));
        }
    }

    fn set_publish_line(&self, text: &str) {
        if let Some(live) = self.live.as_ref() {
            live.control_target.set_publish_status(text);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::team::Link;

    fn loaded(source: Source, links: usize) -> Loaded {
        Loaded {
            template: TeamTemplate {
                links: (0..links)
                    .map(|i| Link {
                        label: format!("L{i}"),
                        url: format!("https://saaga.dev/{i}"),
                        note: String::new(),
                    })
                    .collect(),
                youtube_footer: String::new(),
                updated_by: "andrew".into(),
                updated_at: "2026-09-29T10:00:00-07:00".into(),
            },
            etag: Some("\"abc\"".into()),
            source,
        }
    }

    #[test]
    fn the_line_says_how_many_links_who_saved_them_and_where_they_came_from() {
        assert_eq!(
            describe(&loaded(Source::Team, 3)),
            "3 team links, saved by andrew on 2026-09-29."
        );
        assert_eq!(
            describe(&loaded(Source::Team, 1)),
            "1 team link, saved by andrew on 2026-09-29."
        );
        let cached = describe(&loaded(Source::Cached("expired login".into()), 2));
        assert!(cached.contains("this Mac's copy"), "{cached}");
        assert!(cached.contains("expired login"), "{cached}");
        let mut none = loaded(Source::Team, 0);
        none.template.updated_by.clear();
        assert_eq!(describe(&none), "No team links yet.");
    }
}
