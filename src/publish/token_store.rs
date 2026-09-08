//! Where the YouTube token lives between runs.
//!
//! One row in the same sqlite file the rest of SAAGA already reads
//! (`~/.saaga/auth.db`, table `oauth_tokens`), written in the same shape. That
//! is a deliberate split: the *code* here owes nothing to the Python auth
//! server — no subprocess, no import, no shared config — but the *data* stays
//! one connection, so `saaga-auth status` still lists it and the other
//! publishers keep working off a token this app minted.
//!
//! The schema is matched by hand rather than migrated, because it is not ours
//! to migrate. Two details in it are load-bearing and easy to get wrong:
//! `token_expires_at` is a SQLAlchemy `DATETIME`, which is naive UTC written
//! with a *space* separator, while `expires_at` inside `data_json` is Python's
//! `isoformat()`, with a `T`. Writing either in the other's shape leaves the
//! Python side parsing `None`, which it reads as "never expires" — a token that
//! looks alive forever and 401s on every call.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use chrono::{NaiveDateTime, Utc};
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};

/// The `(platform, account)` this app owns. Shared with the Python store on
/// purpose — see the module docs.
pub const PLATFORM: &str = "youtube";
const ACCOUNT: &str = "default";

/// SQLAlchemy's `DATETIME` rendering: naive UTC, space separator, microseconds.
const DB_TIME: &str = "%Y-%m-%d %H:%M:%S%.6f";
/// Python's `datetime.isoformat()`, which is what `data_json.expires_at` holds.
///
/// Two constants because the directions differ. Writing pins six digits, since
/// `datetime.fromisoformat` on the 3.10 interpreters here accepts only three or
/// six and chrono would otherwise emit nine; reading stays lenient, because the
/// value may have been written by Python with no fraction at all.
const ISO_WRITE: &str = "%Y-%m-%dT%H:%M:%S%.6f";
const ISO_PARSE: &str = "%Y-%m-%dT%H:%M:%S%.f";

/// The provider payload, stored verbatim in `data_json`.
///
/// `extra` catches every key this app does not name so a round-trip through
/// Rust never drops a field the Python side wrote (or will write later).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Token {
    pub access_token: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub refresh_token: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub channel_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub channel_title: Option<String>,
    /// Which OAuth client minted this. A refresh token only works against the
    /// client that issued it, and this app and the Python server are registered
    /// as *different* clients, so a token from the other one cannot be
    /// refreshed here. Recording the minter turns that from a bewildering
    /// `invalid_grant` into "reconnect".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub client_id: Option<String>,
    #[serde(flatten)]
    pub extra: serde_json::Map<String, serde_json::Value>,
}

impl Token {
    /// Whether the access token is past `cutoff_secs` of its life.
    ///
    /// Unknown expiry counts as expired: a token we cannot date is one we
    /// cannot trust, and the refresh that follows is cheap.
    pub fn is_stale(&self, cutoff_secs: i64) -> bool {
        let Some(raw) = self.expires_at.as_deref() else {
            return true;
        };
        let Ok(at) = NaiveDateTime::parse_from_str(raw, ISO_PARSE) else {
            return true;
        };
        (at - Utc::now().naive_utc()).num_seconds() <= cutoff_secs
    }
}

/// The sqlite file both sides share.
///
/// Honours `SAAGA_AUTH_DB_URL` when it points at sqlite, so pointing the Python
/// server somewhere else moves this too rather than silently splitting the
/// store in half.
fn db_path() -> Result<PathBuf> {
    if let Ok(url) = std::env::var("SAAGA_AUTH_DB_URL") {
        let url = url.trim();
        if let Some(rest) = url.strip_prefix("sqlite:///") {
            if !rest.is_empty() {
                return Ok(PathBuf::from(rest));
            }
        }
    }
    let home = std::env::var_os("HOME").context("HOME not set")?;
    Ok(PathBuf::from(home).join(".saaga").join("auth.db"))
}

fn open() -> Result<Connection> {
    open_at(&db_path()?)
}

fn open_at(path: &Path) -> Result<Connection> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    }
    let conn = Connection::open(path).with_context(|| format!("opening {}", path.display()))?;
    // Matches saaga_auth.db.OAuthToken. `IF NOT EXISTS` so whichever side runs
    // first wins and the other finds what it expects.
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS oauth_tokens (
             id INTEGER NOT NULL PRIMARY KEY,
             platform VARCHAR(32) NOT NULL,
             account VARCHAR(64) NOT NULL,
             access_token VARCHAR NOT NULL,
             refresh_token VARCHAR,
             token_expires_at DATETIME,
             scopes VARCHAR,
             data_json VARCHAR NOT NULL,
             created_at DATETIME NOT NULL,
             updated_at DATETIME NOT NULL,
             CONSTRAINT uq_platform_account UNIQUE (platform, account)
         );
         CREATE INDEX IF NOT EXISTS ix_oauth_tokens_platform
             ON oauth_tokens (platform);",
    )
    .context("ensuring the oauth_tokens schema")?;
    Ok(conn)
}

/// The stored token, or `None` when this platform was never connected.
pub fn load() -> Result<Option<Token>> {
    load_in(&open()?)
}

fn load_in(conn: &Connection) -> Result<Option<Token>> {
    let json: Option<String> = conn
        .query_row(
            "SELECT data_json FROM oauth_tokens WHERE platform = ?1 AND account = ?2",
            params![PLATFORM, ACCOUNT],
            |row| row.get(0),
        )
        .optional()
        .context("reading the stored token")?;
    let Some(json) = json else {
        return Ok(None);
    };
    let token = serde_json::from_str(&json).context("parsing the stored token payload")?;
    Ok(Some(token))
}

/// Write the token, replacing whatever was there for this platform.
///
/// The mirrored columns are not the source of truth — `data_json` is — but they
/// are what `saaga-auth status` reads, so they are kept honest.
pub fn save(token: &Token, scopes: Option<&str>) -> Result<()> {
    save_in(&open()?, token, scopes)
}

fn save_in(conn: &Connection, token: &Token, scopes: Option<&str>) -> Result<()> {
    let now = Utc::now().naive_utc().format(DB_TIME).to_string();
    let expires = token
        .expires_at
        .as_deref()
        .and_then(|raw| NaiveDateTime::parse_from_str(raw, ISO_PARSE).ok())
        .map(|at| at.format(DB_TIME).to_string());
    let payload = serde_json::to_string(token).context("serializing the token payload")?;

    conn.execute(
        "INSERT INTO oauth_tokens
             (platform, account, access_token, refresh_token, token_expires_at,
              scopes, data_json, created_at, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?8)
         ON CONFLICT (platform, account) DO UPDATE SET
             access_token = excluded.access_token,
             refresh_token = excluded.refresh_token,
             token_expires_at = excluded.token_expires_at,
             scopes = excluded.scopes,
             data_json = excluded.data_json,
             updated_at = excluded.updated_at",
        params![
            PLATFORM,
            ACCOUNT,
            token.access_token,
            token.refresh_token,
            expires,
            scopes,
            payload,
            now,
        ],
    )
    .context("saving the token")?;
    Ok(())
}

/// An ISO timestamp `expires_in` seconds from now, in the shape `data_json` uses.
pub fn expires_at_from(expires_in: i64) -> String {
    (Utc::now().naive_utc() + chrono::Duration::seconds(expires_in))
        .format(ISO_WRITE)
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unknown_expiry_is_stale() {
        let token = Token::default();
        assert!(token.is_stale(60));
    }

    #[test]
    fn a_token_minted_now_is_not_stale() {
        let token = Token {
            expires_at: Some(expires_at_from(3600)),
            ..Token::default()
        };
        assert!(!token.is_stale(60));
    }

    #[test]
    fn an_expiry_inside_the_cutoff_is_stale() {
        let token = Token {
            expires_at: Some(expires_at_from(30)),
            ..Token::default()
        };
        assert!(token.is_stale(60));
    }

    /// The two timestamp formats are the whole compatibility story with the
    /// Python side, so pin the one this app writes.
    #[test]
    fn expiry_round_trips_through_pythons_isoformat() {
        let raw = expires_at_from(0);
        assert!(raw.contains('T'), "data_json wants isoformat, got {raw}");
        let fraction = raw.split('.').nth(1).unwrap_or_default();
        assert_eq!(
            fraction.len(),
            6,
            "Python 3.10 parses 3 or 6 digits, got {raw}"
        );
        let parsed = NaiveDateTime::parse_from_str(&raw, ISO_PARSE);
        assert!(parsed.is_ok(), "cannot reparse what we wrote: {raw}");
        let db = parsed.unwrap().format(DB_TIME).to_string();
        assert!(
            db.contains(' '),
            "the column wants a space separator, got {db}"
        );
    }

    /// A payload written by the Python server carries keys this struct does not
    /// name; losing them on a Rust round-trip would quietly drop identity.
    #[test]
    fn unknown_fields_survive_a_round_trip() {
        let stored = r#"{"access_token":"a","scope":"x y","email":"who@example.com"}"#;
        let token: Token = serde_json::from_str(stored).unwrap();
        let out = serde_json::to_string(&token).unwrap();
        assert!(out.contains("\"scope\":\"x y\""), "{out}");
        assert!(out.contains("who@example.com"), "{out}");
    }
}
