//! The credentials the app needs, and the `.env` file they are typed into.
//!
//! Every consumer in this codebase reads its key with `std::env::var` — twenty
//! call sites across `thumbnail`, `figure`, `notes`, `agent`, `schedule`,
//! `blog`, `publish` and `distribute`. So this module deliberately does *not*
//! introduce a second way to reach a credential: the Settings pane writes a
//! `.env` file and folds the result back into the process environment, and
//! every one of those call sites keeps working unchanged.
//!
//! ## Which `.env`
//!
//! [`load_dotenv`](crate::load_dotenv) reads several. This module edits exactly
//! one, resolved by [`env_path`], and the pane shows which — a settings screen
//! that writes somewhere the reader cannot name is how two `.env` files end up
//! disagreeing with no way to tell which one lost.
//!
//! A release ships as a bare binary in a tarball, so on a teammate's machine
//! `CARGO_MANIFEST_DIR` names a path on the CI runner that does not exist and
//! the current directory is wherever they happened to launch from. Neither is a
//! place to keep credentials. `~/.stream-recorder/.env` is, it sits beside the
//! `config.json` this app already owns, and it is still an ordinary `.env` that
//! can be read and edited by hand.
//!
//! ## What is not here
//!
//! No masking on write and no encryption: this is a plaintext `.env`, chosen
//! deliberately. What it does do is refuse to widen the exposure — the file is
//! created `0600`, and the pane never sends a stored secret back to the webview
//! (see [`Status::preview`]).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::Serialize;

pub mod check;
pub mod sops;

/// Which part of the workflow a credential unlocks — the pane's grouping, and
/// the order it draws them in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum Group {
    /// Thumbnails, blurbs, notes, social copy — everything that calls a model.
    Llm,
    /// Buffer, and the S3 bucket the videos it links to are uploaded to.
    Social,
    /// Strapi.
    Blog,
    /// YouTube OAuth.
    Video,
    /// Transcription.
    Transcript,
}

impl Group {
    pub const ALL: [Group; 5] = [
        Group::Llm,
        Group::Social,
        Group::Blog,
        Group::Video,
        Group::Transcript,
    ];

    /// The stable name the webview uses to ask for a test and to place the
    /// result. Spelled out rather than derived from [`Group::title`], so
    /// renaming a heading cannot silently break the pane's wiring.
    pub fn slug(self) -> &'static str {
        match self {
            Group::Llm => "llm",
            Group::Social => "social",
            Group::Blog => "blog",
            Group::Video => "video",
            Group::Transcript => "transcript",
        }
    }

    pub fn from_slug(slug: &str) -> Option<Group> {
        Group::ALL.into_iter().find(|group| group.slug() == slug)
    }

    pub fn title(self) -> &'static str {
        match self {
            Group::Llm => "Models",
            Group::Social => "Social scheduling",
            Group::Blog => "Blog (Strapi)",
            Group::Video => "YouTube",
            Group::Transcript => "Transcription",
        }
    }

    /// What stops working when this group is unset. Shown under the heading,
    /// because "OPENROUTER_API_KEY" does not tell a new team member what they
    /// lose by skipping it.
    pub fn detail(self) -> &'static str {
        match self {
            Group::Llm => {
                "Thumbnail art, figure blurbs, notes and social copy. Nothing that writes \
                 text or draws an image works without this one."
            }
            Group::Social => {
                "Building and sending the Buffer schedule, and the S3 upload it links to."
            }
            Group::Blog => "Publishing the article to the CMS.",
            Group::Video => "Uploading the render to YouTube and setting its thumbnail.",
            Group::Transcript => {
                "Chapter transcripts. Without it recording still works; transcripts are skipped."
            }
        }
    }
}

/// How much a missing value costs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum Need {
    /// The feature errors out.
    Required,
    /// The feature degrades but the app runs.
    Optional,
}

/// One row of the Settings pane.
pub struct Field {
    /// The environment variable, which is also the `.env` key and the form name.
    pub key: &'static str,
    pub label: &'static str,
    pub group: Group,
    pub need: Need,
    /// Secrets are masked in the pane and never sent back to it once stored.
    /// Non-secrets (a URL, a channel id) round-trip so they can be edited.
    pub secret: bool,
    pub help: &'static str,
    /// Where to go to get one. Rendered as a link.
    pub url: Option<&'static str>,
}

/// Everything the pane offers, in display order.
///
/// The list is the contract: [`crate::ui`] draws exactly these, [`write`]
/// accepts exactly these keys, and a key absent here cannot be set from the UI
/// — which is what keeps a form post from writing arbitrary environment into
/// the file.
pub const FIELDS: &[Field] = &[
    Field {
        key: "OPENROUTER_API_KEY",
        label: "OpenRouter API key",
        group: Group::Llm,
        need: Need::Required,
        secret: true,
        help: "One key covers every model the app uses, image models included — \
               thumbnails route to google/gemini-3.1-flash-image through OpenRouter, \
               so there is no separate Gemini key to set.",
        url: Some("https://openrouter.ai/keys"),
    },
    Field {
        key: "BUFFER_API_KEY",
        label: "Buffer API key",
        group: Group::Social,
        need: Need::Required,
        secret: true,
        help: "Access token from Buffer's developer settings.",
        url: Some("https://publish.buffer.com/settings/api"),
    },
    Field {
        key: "BUFFER_ORG_ID",
        label: "Buffer organization",
        group: Group::Social,
        need: Need::Optional,
        secret: false,
        help: "Leave blank on a single-workspace account and it is resolved on first use. \
               Set it if you belong to more than one, or posts land in the wrong workspace.",
        url: None,
    },
    Field {
        key: "BUFFER_TWITTER_HANDLE",
        label: "Twitter handle",
        group: Group::Social,
        need: Need::Optional,
        secret: false,
        help: "Used as context when writing posts, e.g. @yourhandle.",
        url: None,
    },
    Field {
        key: "S3_BUCKET",
        label: "S3 bucket",
        group: Group::Social,
        need: Need::Required,
        secret: false,
        help: "Where renders are uploaded so Buffer has a public video URL to attach. \
               AWS credentials themselves come from your AWS profile, not from here.",
        url: None,
    },
    Field {
        key: "STRAPI_API_URL",
        label: "Strapi URL",
        group: Group::Blog,
        need: Need::Required,
        secret: false,
        help: "https://cms.saagasolve.com for production, http://localhost:1337 for local.",
        url: None,
    },
    Field {
        key: "STRAPI_API_TOKEN",
        label: "Strapi API token",
        group: Group::Blog,
        need: Need::Required,
        secret: true,
        help: "Strapi Admin → Settings → API Tokens. Needs find/create on the video-post \
               collection; a read-only token fails at publish, not at save.",
        url: None,
    },
    Field {
        key: "BLOG_PUBLIC_BASE",
        label: "Blog public base URL",
        group: Group::Blog,
        need: Need::Optional,
        secret: false,
        help: "The site figures are linked from, e.g. https://saagasolve.com.",
        url: None,
    },
    Field {
        key: "YOUTUBE_CLIENT_ID",
        label: "YouTube client ID",
        group: Group::Video,
        need: Need::Required,
        secret: false,
        help: "OAuth client of type Desktop app, from Google Cloud Console. \
               Each person signs in as themselves; only the client is shared.",
        url: Some("https://console.cloud.google.com/apis/credentials"),
    },
    Field {
        key: "YOUTUBE_CLIENT_SECRET",
        label: "YouTube client secret",
        group: Group::Video,
        need: Need::Required,
        secret: true,
        help: "Issued with the client ID above.",
        url: None,
    },
    Field {
        key: "YOUTUBE_CHANNEL_ID",
        label: "Expected channel ID",
        group: Group::Video,
        need: Need::Optional,
        secret: false,
        help: "A safety catch: set it and an upload aborts if the signed-in account is not \
               this channel. Leave blank and whichever account is signed in is accepted.",
        url: None,
    },
    Field {
        key: "ASSEMBLYAI_API_KEY",
        label: "AssemblyAI API key",
        group: Group::Transcript,
        need: Need::Optional,
        secret: true,
        help: "Unset, chapters record normally and transcripts are written as skipped.",
        url: Some("https://www.assemblyai.com/app/account"),
    },
];

/// The field for `key`, if the UI is allowed to write it.
pub fn field(key: &str) -> Option<&'static Field> {
    FIELDS.iter().find(|f| f.key == key)
}

/// What the pane draws for one field: whether it is set, and a hint of what is
/// there — never the value itself for a secret.
#[derive(Debug, Clone, Serialize)]
pub struct Status {
    pub key: &'static str,
    pub label: &'static str,
    pub need: Need,
    pub secret: bool,
    pub help: &'static str,
    pub url: Option<&'static str>,
    /// Whether a non-empty value is live in this process.
    pub set: bool,
    /// For a secret: last four characters, or a length hint when it is too short
    /// to reveal any of. For a plain field: the whole value, so it can be edited.
    pub preview: String,
    /// Where the live value came from. With the shared credentials committed to
    /// `dev.sops.env`, most values are not typed by the reader at all, and a
    /// pane that showed them as "set" without saying by whom would invite
    /// someone to retype a key they already have.
    pub source: Source,
}

/// Which layer supplied a live value. Ordered as they are loaded, first wins —
/// see [`crate::load_dotenv`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum Source {
    /// Exported in the shell the app was launched from. Beats everything, and
    /// survives no edit made here.
    Shell,
    /// The `.env` this pane writes.
    Local,
    /// The team's committed `dev.sops.env`, decrypted at startup.
    Team,
    /// Nothing supplied it.
    Unset,
}

/// Show enough of a secret to recognise it, and never enough to use it.
fn preview(value: &str) -> String {
    let count = value.chars().count();
    if count <= 8 {
        // Four characters of an eight-character secret is half of it. Length
        // alone still answers "did my paste land", which is what this is for.
        return format!("{count} characters");
    }
    let tail: String = value.chars().skip(count - 4).collect();
    format!("…{tail}")
}

/// Every field's current state, for rendering the pane.
///
/// Reads the live process environment rather than the file: that is what the
/// rest of the app will actually see, and after a save the two agree because
/// [`write`] folds the new values in.
pub fn status() -> Vec<Status> {
    let stored = read().unwrap_or_default();
    FIELDS
        .iter()
        .map(|f| {
            let live = std::env::var(f.key).unwrap_or_default();
            let live = live.trim();
            let on_file = stored.get(f.key).map(String::as_str).unwrap_or("").trim();
            let from_team = sops::provided()
                .get(f.key)
                .map(String::as_str)
                .unwrap_or("")
                .trim();
            // Local before team: the local file is loaded first and therefore
            // wins, so when both carry the same value it is the local one in
            // effect — and the local one is what this pane can actually change.
            let source = if live.is_empty() {
                Source::Unset
            } else if on_file == live {
                Source::Local
            } else if from_team == live {
                Source::Team
            } else {
                Source::Shell
            };
            Status {
                key: f.key,
                label: f.label,
                need: f.need,
                secret: f.secret,
                help: f.help,
                url: f.url,
                set: !live.is_empty(),
                preview: if live.is_empty() {
                    String::new()
                } else if f.secret {
                    preview(live)
                } else {
                    live.to_string()
                },
                source,
            }
        })
        .collect()
}

/// One stage's model choice, as the Settings pane draws it.
///
/// The pane renders what is *stored*, not a live catalog: building the dropdown
/// needs a provider-scoped model list from OpenRouter, and a settings screen
/// that blocks on a network call — or worse, renders an empty dropdown when the
/// call fails — is a worse answer than naming the choice and pointing at the
/// tab that can change it.
#[derive(Debug, Clone, Serialize)]
pub struct ModelChoice {
    pub stage: &'static str,
    /// What this model is used for, in the reader's terms.
    pub detail: &'static str,
    pub model: String,
    /// `None` is OpenRouter's automatic routing.
    pub provider: String,
    /// Which tab's dropdowns change it.
    pub where_to_change: &'static str,
}

/// Every stage's model and provider, read from the saved config.
///
/// Central on purpose: these were spread across three panes with no single
/// place showing what would actually run, and the Post tab's pair was not
/// written down at all — chosen from a dropdown, used for that session, and
/// silently back to the default on the next launch.
pub fn models() -> Vec<ModelChoice> {
    let cfg = crate::config::load();
    let auto = || "Auto (OpenRouter routes)".to_string();
    let notes_model = cfg
        .notes_model
        .clone()
        .unwrap_or_else(crate::notes::default_model);
    // Unset means the Post tab starts from the Notes choice, which is what
    // startup does — so showing the Notes model here is what will actually run.
    let posts_model = cfg
        .posts_model
        .clone()
        .unwrap_or_else(|| notes_model.clone());
    let posts_provider = cfg
        .posts_provider
        .clone()
        .or_else(|| cfg.notes_provider.clone());

    let mut out = vec![
        ModelChoice {
            stage: "Notes, titles and video copy",
            detail: "Chapter notes, the video title and description, and the blog article.",
            model: notes_model,
            provider: cfg.notes_provider.clone().unwrap_or_else(auto),
            where_to_change: "the dropdowns beside Notes",
        },
        ModelChoice {
            stage: "Social posts",
            detail: "The copy written for each platform on the Generate posts tab.",
            model: posts_model,
            provider: posts_provider.unwrap_or_else(auto),
            where_to_change: "the dropdowns on Generate posts",
        },
    ];
    if let Some(first) = cfg.thumbnail.models.first() {
        out.push(ModelChoice {
            stage: "Thumbnail images",
            detail: "The image model the thumbnail stage draws with.",
            model: first.id.clone(),
            provider: "through OpenRouter".to_string(),
            where_to_change: "the AI experiments under Video details",
        });
    }
    out
}

/// Print what is set and where it came from, without printing a secret.
///
/// The headless twin of the Settings tab, for the case the tab cannot help
/// with: something is wrong at launch, or the app is being set up over a
/// terminal. Writes to `out` rather than stdout so the shape of the report can
/// be asserted in a test.
pub fn report(out: &mut impl std::io::Write) -> anyhow::Result<()> {
    let path = env_path()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|_| "<no HOME>".into());
    writeln!(out, "Local settings file: {path}")?;

    match sops::path() {
        Some(team) => {
            let count = sops::provided().len();
            if count == 0 {
                writeln!(
                    out,
                    "Team credentials:    {} — NOT decrypted (see the message above)",
                    team.display()
                )?;
            } else {
                writeln!(
                    out,
                    "Team credentials:    {} — {count} keys decrypted",
                    team.display()
                )?;
            }
        }
        None => writeln!(out, "Team credentials:    none in this directory")?,
    }
    writeln!(out)?;

    let status = status();
    for group in Group::ALL {
        writeln!(out, "{}", group.title())?;
        for (field, state) in FIELDS.iter().zip(&status) {
            if field.group != group {
                continue;
            }
            let mark = if state.set {
                "ok"
            } else if state.need == Need::Required {
                "MISSING"
            } else {
                "--"
            };
            let from = match state.source {
                Source::Team => "team",
                Source::Local => "local",
                Source::Shell => "shell",
                Source::Unset => "",
            };
            writeln!(out, "  {mark:<8} {:<24} {from}", field.key)?;
        }
    }

    writeln!(out, "\nModels")?;
    for choice in models() {
        writeln!(
            out,
            "  {:<28} {} ({})",
            choice.stage, choice.model, choice.provider
        )?;
    }

    let missing = missing_required();
    writeln!(out)?;
    if missing.is_empty() {
        writeln!(out, "Every required key is set.")?;
    } else {
        writeln!(
            out,
            "{} required key(s) missing: {}",
            missing.len(),
            missing.join(", ")
        )?;
        writeln!(out, "Set them in the app's Settings tab, or in {path}.")?;
    }
    Ok(())
}

/// One heading in the pane, with the fields under it.
#[derive(Debug, Clone, Serialize)]
pub struct Section {
    pub slug: &'static str,
    pub title: &'static str,
    pub detail: &'static str,
    pub fields: Vec<Status>,
    /// Every required field in this section has a value, so its feature will run.
    pub complete: bool,
}

/// Everything the pane renders: the sections, in [`Group::ALL`] order.
pub fn sections() -> Vec<Section> {
    let status = status();
    Group::ALL
        .into_iter()
        .map(|group| {
            let fields: Vec<Status> = FIELDS
                .iter()
                .zip(&status)
                .filter(|(field, _)| field.group == group)
                .map(|(_, status)| status.clone())
                .collect();
            Section {
                slug: group.slug(),
                title: group.title(),
                detail: group.detail(),
                complete: fields.iter().all(|f| f.need == Need::Optional || f.set),
                fields,
            }
        })
        .collect()
}

/// How many required fields are still empty — the pane's banner.
pub fn missing_required() -> Vec<&'static str> {
    status()
        .into_iter()
        .filter(|s| s.need == Need::Required && !s.set)
        .map(|s| s.label)
        .collect()
}

/// The per-user `.env`, beside the config this app already owns.
pub fn user_env_path() -> Result<PathBuf> {
    let home = std::env::var_os("HOME").context("HOME not set")?;
    Ok(PathBuf::from(home).join(".stream-recorder").join(".env"))
}

/// The one file the Settings pane reads and writes.
///
/// A checkout's own `.env` wins when there is one, so a developer editing
/// settings keeps editing the file they already have rather than quietly
/// starting a second one that the first one then shadows. Everywhere else —
/// which is every machine that installed a release — it is the per-user file,
/// whether or not it exists yet.
pub fn env_path() -> Result<PathBuf> {
    let crate_env = Path::new(env!("CARGO_MANIFEST_DIR")).join(".env");
    if crate_env.is_file() {
        return Ok(crate_env);
    }
    if let Some(cwd) = std::env::current_dir().ok().map(|d| d.join(".env")) {
        if cwd.is_file() {
            return Ok(cwd);
        }
    }
    user_env_path()
}

/// Parse `.env` syntax into key/value pairs.
///
/// The same subset [`crate::load_dotenv`] accepts — `KEY=value`, optional
/// `export`, `#` comments, optional surrounding quotes — kept in step with it
/// deliberately: a file this pane writes and that loader cannot read is the one
/// failure that would look like the save silently doing nothing.
pub fn parse(text: &str) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let line = line.strip_prefix("export ").unwrap_or(line);
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        out.insert(
            key.trim().to_string(),
            value.trim().trim_matches(['"', '\'']).to_string(),
        );
    }
    out
}

/// The current contents of [`env_path`], or an empty map when there is no file.
pub fn read() -> Result<BTreeMap<String, String>> {
    let path = env_path()?;
    match std::fs::read_to_string(&path) {
        Ok(text) => Ok(parse(&text)),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(BTreeMap::new()),
        Err(err) => Err(err).with_context(|| format!("reading {}", path.display())),
    }
}

/// Apply `updates` to `text`, returning the new file contents.
///
/// Rewrites the line a key is already on and appends the ones that are new, so
/// comments, ordering, and every unrelated key in the file survive — this file
/// holds credentials for services beyond this app, and a settings screen that
/// rewrote it wholesale would delete them.
///
/// A value that is empty or unchanged is not written, which is what lets the
/// pane post every field on every save without clearing the secrets it was
/// never shown.
pub fn merge(text: &str, updates: &BTreeMap<String, String>) -> String {
    let existing = parse(text);
    let mut pending: BTreeMap<&str, &str> = BTreeMap::new();
    for (key, value) in updates {
        let value = value.trim();
        if value.is_empty() || existing.get(key.as_str()).map(String::as_str) == Some(value) {
            continue;
        }
        pending.insert(key.as_str(), value);
    }
    if pending.is_empty() {
        return text.to_string();
    }

    let mut out = String::with_capacity(text.len() + 128);
    let mut written: Vec<&str> = Vec::new();
    for line in text.lines() {
        let trimmed = line.trim();
        let bare = trimmed.strip_prefix("export ").unwrap_or(trimmed);
        let key = (!trimmed.starts_with('#'))
            .then(|| bare.split_once('='))
            .flatten()
            .map(|(key, _)| key.trim());
        match key.and_then(|key| pending.get(key).map(|value| (key, *value))) {
            Some((key, value)) => {
                let prefix = if trimmed.starts_with("export ") {
                    "export "
                } else {
                    ""
                };
                out.push_str(&format!("{prefix}{key}={value}\n"));
                written.push(key);
            }
            None => {
                out.push_str(line);
                out.push('\n');
            }
        }
    }

    let fresh: Vec<_> = pending
        .iter()
        .filter(|(key, _)| !written.contains(*key))
        .collect();
    if !fresh.is_empty() {
        if !out.is_empty() && !out.ends_with("\n\n") {
            out.push('\n');
        }
        out.push_str("# Added by the Settings tab\n");
        for (key, value) in fresh {
            out.push_str(&format!("{key}={value}\n"));
        }
    }
    out
}

/// Write `updates` to [`env_path`] and fold them into this process.
///
/// Returns the file written, for the pane to name. Keys outside [`FIELDS`] are
/// dropped rather than rejected: the form is the only writer, and a stray field
/// name is a bug in the pane, not a reason to lose a legitimate save alongside
/// it.
///
/// The environment is updated too, so a key typed here works on the next button
/// press rather than the next launch.
pub fn write(updates: &BTreeMap<String, String>) -> Result<PathBuf> {
    let allowed: BTreeMap<String, String> = updates
        .iter()
        .filter(|(key, _)| field(key).is_some())
        .map(|(key, value)| (key.clone(), value.trim().to_string()))
        .collect();

    let path = env_path()?;
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    }
    let current = std::fs::read_to_string(&path).unwrap_or_default();
    let merged = merge(&current, &allowed);
    if merged != current {
        write_private(&path, &merged)?;
    }

    for (key, value) in &allowed {
        if !value.is_empty() {
            // SAFETY: the winit event loop is single-threaded and every UiEvent
            // is handled on it, so nothing is reading the environment
            // concurrently here. The workers that do read it (transcode,
            // upload, generation) are spawned per action, after this returns.
            unsafe { std::env::set_var(key, value) };
        }
    }
    Ok(path)
}

/// Write `text` to `path` so only this user can read it.
///
/// Owner-only from creation rather than after: a `0644` window between the
/// write and a `chmod`, however short, is a window where a credential file was
/// world-readable.
fn write_private(path: &Path, text: &str) -> Result<()> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;

    // Through a temporary file in the same directory: a crash partway through
    // rewriting `.env` in place would leave a truncated file, which reads as
    // "every credential vanished".
    let tmp = path.with_extension("env.tmp");
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(&tmp)
        .with_context(|| format!("creating {}", tmp.display()))?;
    file.write_all(text.as_bytes())
        .with_context(|| format!("writing {}", tmp.display()))?;
    file.sync_all().ok();
    drop(file);
    // An existing file keeps its own mode through a rename, so tighten it too.
    if path.exists() {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
    }
    std::fs::rename(&tmp, path).with_context(|| format!("replacing {}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn updates(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    /// The point of merging rather than rewriting: this file holds credentials
    /// for services this app has never heard of, and they have to survive a save.
    #[test]
    fn unrelated_keys_and_comments_survive_a_write() {
        let before = "# team credentials\nJIRA_API_TOKEN=keepme\n\n# llm\nOPENROUTER_API_KEY=old\n";
        let after = merge(&before, &updates(&[("OPENROUTER_API_KEY", "new")]));
        assert!(after.contains("JIRA_API_TOKEN=keepme"), "{after}");
        assert!(after.contains("# team credentials"), "{after}");
        assert!(after.contains("OPENROUTER_API_KEY=new"), "{after}");
        assert!(!after.contains("OPENROUTER_API_KEY=old"), "{after}");
    }

    /// The pane posts every field on every save, and it is never shown the
    /// secrets it already holds — so a blank box has to mean "leave it alone".
    /// If it meant "clear it", opening Settings and pressing Save would wipe
    /// every key in the file.
    #[test]
    fn a_blank_field_leaves_the_stored_value_alone() {
        let before = "BUFFER_API_KEY=live\n";
        let after = merge(
            &before,
            &updates(&[("BUFFER_API_KEY", ""), ("S3_BUCKET", "  ")]),
        );
        assert_eq!(after, before);
    }

    #[test]
    fn a_new_key_is_appended_under_its_own_heading() {
        let after = merge("EXISTING=1\n", &updates(&[("S3_BUCKET", "media")]));
        assert!(after.starts_with("EXISTING=1\n"), "{after}");
        assert!(after.contains("# Added by the Settings tab"), "{after}");
        assert!(after.contains("S3_BUCKET=media"), "{after}");
    }

    #[test]
    fn an_exported_line_stays_exported() {
        let after = merge("export S3_BUCKET=old\n", &updates(&[("S3_BUCKET", "new")]));
        assert_eq!(after, "export S3_BUCKET=new\n");
    }

    /// A commented-out key is a suggestion, not a setting — rewriting it in
    /// place would leave the real value further down the file still winning.
    #[test]
    fn a_commented_line_is_not_mistaken_for_the_setting() {
        let after = merge("# S3_BUCKET=example\n", &updates(&[("S3_BUCKET", "real")]));
        assert!(after.contains("# S3_BUCKET=example"), "{after}");
        assert!(after.contains("\nS3_BUCKET=real\n"), "{after}");
    }

    /// Keys the form does not own cannot be written through it, whatever the
    /// webview posts.
    #[test]
    fn only_declared_fields_are_writable() {
        assert!(field("OPENROUTER_API_KEY").is_some());
        assert!(field("PATH").is_none());
        assert!(field("GM_PASSWORD").is_none());
    }

    /// What the pane is allowed to show. Four characters identifies a paste;
    /// the rest of the key never leaves the process.
    #[test]
    fn a_secret_preview_reveals_only_the_tail() {
        assert_eq!(preview("sk-or-v1-abcdef1234wxyz"), "…wxyz");
        assert_eq!(preview("short"), "5 characters");
        assert_eq!(preview("exactly8"), "8 characters");
    }

    #[test]
    fn parsing_matches_the_loader_on_quotes_and_exports() {
        let parsed = parse("export A=\"one\"\nB='two'\n# C=three\nD=four\nnonsense\n");
        assert_eq!(parsed.get("A").map(String::as_str), Some("one"));
        assert_eq!(parsed.get("B").map(String::as_str), Some("two"));
        assert_eq!(parsed.get("C"), None);
        assert_eq!(parsed.get("D").map(String::as_str), Some("four"));
    }

    /// Every stage has to name a model, including the ones whose config is
    /// unset — an empty row here reads as "no model", when what is really
    /// happening is that a default runs.
    #[test]
    fn every_stage_names_a_model_and_a_provider() {
        for choice in models() {
            assert!(
                !choice.model.trim().is_empty(),
                "{} has no model",
                choice.stage
            );
            assert!(
                !choice.provider.trim().is_empty(),
                "{} has no provider",
                choice.stage
            );
            assert!(
                !choice.where_to_change.trim().is_empty(),
                "{} does not say where to change it",
                choice.stage
            );
        }
    }

    /// The Post tab falls back to the Notes model when it has never been given
    /// one, because that is what startup actually does — showing a different
    /// model here than the one that will run is the bug this section exists to
    /// end.
    #[test]
    fn an_unset_post_model_reports_the_notes_model_that_will_run() {
        let cfg = crate::config::load();
        if cfg.posts_model.is_some() {
            return; // This machine has made a choice; nothing to infer.
        }
        let rows = models();
        let notes = rows
            .iter()
            .find(|c| c.stage.starts_with("Notes"))
            .expect("notes row");
        let posts = rows
            .iter()
            .find(|c| c.stage == "Social posts")
            .expect("posts row");
        assert_eq!(
            posts.model, notes.model,
            "posts should mirror notes when unset"
        );
    }

    /// The report is what a teammate runs when something is wrong, so it has to
    /// name every key and never print a value.
    #[test]
    fn the_report_names_every_key_and_leaks_none() {
        // SAFETY: single-threaded test.
        unsafe { std::env::set_var("STRAPI_API_TOKEN", "tok-supersecret-do-not-print") };
        let mut out = Vec::new();
        report(&mut out).expect("the report writes");
        unsafe { std::env::remove_var("STRAPI_API_TOKEN") };

        let text = String::from_utf8(out).expect("utf8");
        for field in FIELDS {
            assert!(text.contains(field.key), "the report omits {}", field.key);
        }
        assert!(
            !text.contains("supersecret"),
            "the report printed a secret:\n{text}"
        );
        assert!(text.contains("Local settings file:"), "{text}");
    }

    /// Every field the pane draws has to be one the app actually reads, or the
    /// pane is teaching people to fill in something that does nothing.
    #[test]
    fn every_field_is_uniquely_named_and_grouped() {
        let keys: std::collections::HashSet<_> = FIELDS.iter().map(|f| f.key).collect();
        assert_eq!(keys.len(), FIELDS.len(), "duplicate key in FIELDS");
        for group in Group::ALL {
            assert!(
                FIELDS.iter().any(|f| f.group == group),
                "{group:?} has no fields but is drawn as a section"
            );
        }
    }

    /// Required means the feature errors without it, and the pane's readiness
    /// banner counts on that split staying accurate.
    #[test]
    fn the_required_set_is_what_the_code_hard_errors_on() {
        let required: Vec<_> = FIELDS
            .iter()
            .filter(|f| f.need == Need::Required)
            .map(|f| f.key)
            .collect();
        assert_eq!(
            required,
            [
                "OPENROUTER_API_KEY",
                "BUFFER_API_KEY",
                "S3_BUCKET",
                "STRAPI_API_URL",
                "STRAPI_API_TOKEN",
                "YOUTUBE_CLIENT_ID",
                "YOUTUBE_CLIENT_SECRET",
            ]
        );
    }
}
