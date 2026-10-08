//! Your commits on GitHub, for the Speaking notes to summarise.
//!
//! The notes are prep for the take: what you shipped since the last video, as
//! talking points. This module finds a login, works out "since the last
//! video", and fetches the commits; [`crate::agent::notes`] writes the deck.
//!
//! ## The login
//!
//! Three sources, in order, the first that answers wins:
//!
//! 1. `GITHUB_TOKEN` — typed into Settings → GitHub, or saved there by a
//!    sign-in below.
//! 2. `gh auth token` — whoever already signed in to the GitHub CLI is signed
//!    in here too, with nothing to paste.
//! 3. Sign in with GitHub, the device-code flow: a short code to enter at
//!    github.com/login/device. It needs an OAuth app registered to the org with
//!    device flow enabled, whose client ID goes in `GITHUB_CLIENT_ID`; without
//!    one, [`Login::Unavailable`] says how to use the other two.
//!
//! ## Which commits
//!
//! GitHub's commit search across every repository the token can read, once
//! for `author:<you>` and once per email you commit as ([`author_emails`]):
//! GitHub only credits a commit to an account when its email is verified on
//! that account, and a repo that commits as a work address would otherwise be
//! missing entirely. Search covers default branches only, so work still on a
//! feature branch is not in it until it merges — said in the deck's status.

use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};

const API: &str = "https://api.github.com";
const HTTP_TIMEOUT: Duration = Duration::from_secs(30);
/// Three pages of search. Past that it is not a video's worth of work, and the
/// search API allows 30 requests a minute.
const MAX_COMMITS: usize = 300;
/// With no earlier project to measure from.
const DEFAULT_WINDOW: chrono::Duration = chrono::Duration::days(7);

/// A token, and where it came from, for the status line.
#[derive(Debug, Clone)]
pub struct Token {
    pub value: String,
    pub source: &'static str,
}

/// How to get a token when none is on hand.
pub enum Login {
    /// A client ID is set: the device flow can run.
    DeviceFlow { client_id: String },
    /// Nothing to sign in with; the message says what to do instead.
    Unavailable(String),
}

/// A saved token, or the `gh` CLI's. `None` means a sign-in is needed.
pub fn token() -> Option<Token> {
    let saved = std::env::var("GITHUB_TOKEN").unwrap_or_default();
    if !saved.trim().is_empty() {
        return Some(Token {
            value: saved.trim().to_string(),
            source: "Settings",
        });
    }
    let output = std::process::Command::new("gh")
        .args(["auth", "token"])
        .output()
        .ok()?;
    let value = String::from_utf8(output.stdout).ok()?.trim().to_string();
    (output.status.success() && !value.is_empty()).then_some(Token {
        value,
        source: "the gh CLI",
    })
}

pub fn login() -> Login {
    let client_id = std::env::var("GITHUB_CLIENT_ID").unwrap_or_default();
    if !client_id.trim().is_empty() {
        return Login::DeviceFlow {
            client_id: client_id.trim().to_string(),
        };
    }
    Login::Unavailable(
        "not signed in to GitHub — run `gh auth login` in Terminal, or paste a token \
         (repo scope) under Settings → GitHub, then press Summarize Commits again"
            .to_string(),
    )
}

/// The first half of the device flow: the code to show and where to enter it.
#[derive(Debug, Clone, Deserialize)]
pub struct DeviceCode {
    pub device_code: String,
    pub user_code: String,
    pub verification_uri: String,
    pub expires_in: u64,
    #[serde(default = "default_interval")]
    pub interval: u64,
}

fn default_interval() -> u64 {
    5
}

pub fn start_device_login(client_id: &str) -> Result<DeviceCode> {
    ureq::post("https://github.com/login/device/code")
        .timeout(HTTP_TIMEOUT)
        .set("Accept", "application/json")
        // `repo` because the search has to read private repositories' commits.
        // `user:email` so the account's own emails can widen the search.
        .send_form(&[("client_id", client_id), ("scope", "repo user:email")])
        .context("asking GitHub for a sign-in code")?
        .into_json()
        .context("reading GitHub's sign-in code")
}

/// The second half: wait for the code to be entered, then take the token.
pub fn finish_device_login(client_id: &str, code: &DeviceCode) -> Result<String> {
    let deadline = Instant::now() + Duration::from_secs(code.expires_in);
    let mut interval = Duration::from_secs(code.interval.max(1));
    loop {
        std::thread::sleep(interval);
        if Instant::now() > deadline {
            bail!("the GitHub sign-in code expired before it was entered");
        }
        let body: serde_json::Value = ureq::post("https://github.com/login/oauth/access_token")
            .timeout(HTTP_TIMEOUT)
            .set("Accept", "application/json")
            .send_form(&[
                ("client_id", client_id),
                ("device_code", &code.device_code),
                ("grant_type", "urn:ietf:params:oauth:grant-type:device_code"),
            ])
            .context("checking the GitHub sign-in")?
            .into_json()
            .context("reading the GitHub sign-in")?;
        if let Some(token) = body.get("access_token").and_then(|t| t.as_str()) {
            return Ok(token.to_string());
        }
        match body.get("error").and_then(|e| e.as_str()) {
            Some("authorization_pending") => {}
            // GitHub's own instruction: back off by five seconds.
            Some("slow_down") => interval += Duration::from_secs(5),
            Some("access_denied") => bail!("the GitHub sign-in was declined"),
            Some("expired_token") => bail!("the GitHub sign-in code expired"),
            Some(other) => bail!("GitHub sign-in failed: {other}"),
            None => bail!("GitHub answered the sign-in with neither a token nor an error"),
        }
    }
}

fn get(token: &str, url: &str) -> ureq::Request {
    ureq::get(url)
        .timeout(HTTP_TIMEOUT)
        .set("Authorization", &format!("Bearer {token}"))
        .set("Accept", "application/vnd.github+json")
        .set("X-GitHub-Api-Version", "2022-11-28")
        // Required: the API refuses requests without one.
        .set("User-Agent", "saaga-social-flow")
}

/// The signed-in account's login.
pub fn whoami(token: &str) -> Result<String> {
    #[derive(Deserialize)]
    struct User {
        login: String,
    }
    let user: User = match get(token, &format!("{API}/user")).call() {
        Ok(response) => response.into_json().context("reading the GitHub user")?,
        Err(ureq::Error::Status(401, _)) => {
            bail!("GitHub rejected the token (401) — sign in again or replace it in Settings")
        }
        Err(err) => return Err(err).context("reaching GitHub"),
    };
    Ok(user.login)
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Commit {
    pub repo: String,
    pub sha: String,
    /// The whole message: subject, then body.
    pub message: String,
    /// RFC 3339, as GitHub reports it.
    pub date: String,
    pub url: String,
}

impl Commit {
    pub fn subject(&self) -> &str {
        self.message.lines().next().unwrap_or("").trim()
    }
}

/// The emails your commits may carry: the account's own (when the token may
/// read them), your global git email, and `GITHUB_AUTHOR_EMAILS` for the ones
/// only some repositories use. Lower-cased, one of each.
pub fn author_emails(token: &str) -> Vec<String> {
    let mut emails: Vec<String> = Vec::new();
    if let Ok(response) = get(token, &format!("{API}/user/emails")).call() {
        if let Ok(serde_json::Value::Array(rows)) = response.into_json::<serde_json::Value>() {
            emails.extend(
                rows.iter()
                    .filter(|row| row.get("verified").and_then(|v| v.as_bool()) != Some(false))
                    .filter_map(|row| row.get("email")?.as_str().map(str::to_string)),
            );
        }
    }
    if let Ok(output) = std::process::Command::new("git")
        .args(["config", "--global", "user.email"])
        .output()
    {
        emails.push(String::from_utf8_lossy(&output.stdout).to_string());
    }
    emails.extend(
        std::env::var("GITHUB_AUTHOR_EMAILS")
            .unwrap_or_default()
            .split([',', ' ', '\n'])
            .map(str::to_string),
    );
    let mut out: Vec<String> = emails
        .into_iter()
        .map(|e| e.trim().to_lowercase())
        // A noreply address is already the account's, and `author:` covers it.
        .filter(|e| e.contains('@') && !e.ends_with("@users.noreply.github.com"))
        .collect();
    out.sort();
    out.dedup();
    out
}

/// Your commits since `since`, newest first, one per SHA: `author:<login>`,
/// then each of `emails`.
pub fn commits_since(
    token: &str,
    login: &str,
    emails: &[String],
    since: chrono::DateTime<chrono::Utc>,
) -> Result<Vec<Commit>> {
    let date = format!("author-date:>={}", since.format("%Y-%m-%dT%H:%M:%SZ"));
    let mut out: Vec<Commit> = Vec::new();
    let authors = std::iter::once(format!("author:{login}"))
        .chain(emails.iter().map(|email| format!("author-email:{email}")));
    for author in authors {
        out.extend(search(token, &format!("{author} {date}"))?);
    }
    // One piece of work however many searches or forks found it.
    let mut seen = std::collections::HashSet::new();
    out.retain(|commit| seen.insert(commit.sha.clone()));
    out.sort_by(|a, b| b.date.cmp(&a.date));
    out.truncate(MAX_COMMITS);
    Ok(out)
}

fn search(token: &str, query: &str) -> Result<Vec<Commit>> {
    let mut out: Vec<Commit> = Vec::new();
    for page in 1..=MAX_COMMITS / 100 {
        let body: serde_json::Value = get(token, &format!("{API}/search/commits"))
            .query("q", query)
            .query("sort", "author-date")
            .query("order", "desc")
            .query("per_page", "100")
            .query("page", &page.to_string())
            .call()
            .context("searching GitHub for your commits")?
            .into_json()
            .context("reading GitHub's commit search")?;
        let items = body
            .get("items")
            .and_then(|i| i.as_array())
            .cloned()
            .unwrap_or_default();
        let count = items.len();
        out.extend(items.iter().filter_map(parse_item));
        if count < 100 {
            break;
        }
    }
    Ok(out)
}

fn parse_item(item: &serde_json::Value) -> Option<Commit> {
    let commit = item.get("commit")?;
    Some(Commit {
        repo: item
            .get("repository")?
            .get("full_name")?
            .as_str()?
            .to_string(),
        sha: item.get("sha")?.as_str()?.to_string(),
        message: commit.get("message")?.as_str()?.trim().to_string(),
        date: commit
            .get("author")
            .and_then(|a| a.get("date"))
            .and_then(|d| d.as_str())
            .unwrap_or_default()
            .to_string(),
        url: item
            .get("html_url")
            .and_then(|u| u.as_str())
            .unwrap_or_default()
            .to_string(),
    })
}

/// When the project before `current` was started, which is where "since the
/// last video" begins. Project folders are named for their UTC start time.
///
/// The latest folder that parses and is older than `current`'s own; with none
/// — a first project — the last [`DEFAULT_WINDOW`] of work.
pub fn since_last_project(current: &std::path::Path) -> chrono::DateTime<chrono::Utc> {
    let started = |path: &std::path::Path| {
        let name = path.file_name()?.to_str()?;
        chrono::NaiveDateTime::parse_from_str(name, "%Y-%m-%d_%H-%M-%S")
            .ok()
            .map(|t| t.and_utc())
    };
    let now = chrono::Utc::now();
    let mine = started(current).unwrap_or(now);
    crate::sessions::list()
        .iter()
        .filter_map(|entry| started(&entry.root))
        .filter(|when| *when < mine)
        .max()
        .unwrap_or(mine - DEFAULT_WINDOW)
}

/// The commits as the model reads them: grouped by repository, oldest first
/// inside each so the story runs forward, bodies trimmed.
pub fn as_prompt(login: &str, since: &str, commits: &[Commit]) -> String {
    let mut repos: Vec<&str> = commits.iter().map(|c| c.repo.as_str()).collect();
    repos.sort();
    repos.dedup();
    let mut out = format!(
        "Commits by {login} since {since}: {} across {} repositories.\n",
        commits.len(),
        repos.len()
    );
    for repo in repos {
        out.push_str(&format!("\n## {repo}\n"));
        let mut theirs: Vec<&Commit> = commits.iter().filter(|c| c.repo == repo).collect();
        theirs.sort_by(|a, b| a.date.cmp(&b.date));
        for commit in theirs {
            let day = commit.date.get(..10).unwrap_or(&commit.date);
            out.push_str(&format!("- {day} {}\n", commit.subject()));
            let body: String = commit
                .message
                .lines()
                .skip(1)
                .map(str::trim)
                .filter(|l| !l.is_empty() && !l.starts_with("Co-Authored-By"))
                .collect::<Vec<_>>()
                .join(" ");
            if !body.is_empty() {
                let body: String = body.chars().take(400).collect();
                out.push_str(&format!("  {body}\n"));
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn commit(repo: &str, date: &str, message: &str) -> Commit {
        Commit {
            repo: repo.into(),
            sha: format!("{repo}{date}"),
            message: message.into(),
            date: date.into(),
            url: String::new(),
        }
    }

    /// Grouped by repository, oldest first inside each, trailers dropped.
    #[test]
    fn the_prompt_groups_by_repo_and_reads_forward() {
        let text = as_prompt(
            "amovfx",
            "2026-09-15",
            &[
                commit("a/app", "2026-09-20T10:00:00Z", "Second change"),
                commit(
                    "a/app",
                    "2026-09-18T10:00:00Z",
                    "First change\n\nWhy it mattered.\n\nCo-Authored-By: X <x@y>",
                ),
                commit("a/cms", "2026-09-19T10:00:00Z", "CMS fix"),
            ],
        );
        assert!(text.starts_with("Commits by amovfx since 2026-09-15: 3 across 2 repositories."));
        let first = text.find("First change").unwrap();
        let second = text.find("Second change").unwrap();
        assert!(first < second, "{text}");
        assert!(text.contains("Why it mattered."));
        assert!(!text.contains("Co-Authored-By"));
        assert!(text.find("## a/app").unwrap() < text.find("## a/cms").unwrap());
    }

    #[test]
    fn a_search_item_parses_into_a_commit() {
        let item = serde_json::json!({
            "sha": "abc",
            "html_url": "https://github.com/a/app/commit/abc",
            "repository": { "full_name": "a/app" },
            "commit": { "message": "Subject\n\nBody", "author": { "date": "2026-09-20T10:00:00Z" } }
        });
        let parsed = parse_item(&item).unwrap();
        assert_eq!(parsed.repo, "a/app");
        assert_eq!(parsed.subject(), "Subject");
        assert_eq!(parsed.date, "2026-09-20T10:00:00Z");
    }

    /// A folder that is not a timestamp is not a project start, and the window
    /// never begins after the project it is for.
    #[test]
    fn since_is_never_after_the_current_project() {
        let current = std::path::Path::new("/x/2026-09-22_10-00-00");
        let since = since_last_project(current);
        assert!(
            since
                < chrono::NaiveDateTime::parse_from_str("2026-09-22_10-00-00", "%Y-%m-%d_%H-%M-%S")
                    .unwrap()
                    .and_utc()
        );
    }
}
