//! Google's OAuth consent for YouTube, run in this process.
//!
//! The whole dance is here: build the authorize URL, hold a socket open for the
//! one redirect Google sends back, trade the code for tokens, and refresh them
//! later. No subprocess, no auth server, no tunnel.
//!
//! The tunnel is gone because of which client this app uses. Google accepts an
//! `http://localhost` redirect — it is the one provider that does — but only if
//! that exact URI is registered, and `YOUTUBE_CLIENT_ID` here has
//! `http://localhost:9876/callback` on its list where the Python server's
//! client has an ngrok domain instead. So the browser comes straight back to a
//! socket on this machine, and nothing has to be publicly reachable for a
//! connect to work.
//!
//! One consequence worth knowing: a refresh token is bound to the client that
//! minted it. A token connected through the Python server cannot be refreshed
//! here and vice versa — [`super::token_store::Token::client_id`] records which
//! one, so the mismatch surfaces as "reconnect" instead of `invalid_grant`.

use std::io::{BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};
use std::time::{Duration, Instant};

use anyhow::{anyhow, bail, Context, Result};
use serde::Deserialize;

use super::token_store::{expires_at_from, Token};

const AUTH_URL: &str = "https://accounts.google.com/o/oauth2/v2/auth";
const TOKEN_URL: &str = "https://oauth2.googleapis.com/token";
const CHANNELS_URL: &str = "https://www.googleapis.com/youtube/v3/channels";

/// The port in the redirect URI registered with the OAuth client. Changing it
/// means registering the new URI in the Cloud console first — Google checks the
/// redirect before it draws the consent screen, so a stale port fails the flow
/// with `redirect_uri_mismatch` and never reaches the user.
const CALLBACK_PORT: u16 = 9876;

/// Upload, and read the channel back to confirm which one was granted. Kept to
/// two scopes deliberately: Google rejects a request that pairs `drive.file`
/// with these, and Drive is not this app's business anyway.
const SCOPES: [&str; 2] = [
    "https://www.googleapis.com/auth/youtube.upload",
    "https://www.googleapis.com/auth/youtube.readonly",
];

/// How long to hold the socket waiting for a human to finish in the browser.
const CONSENT_TIMEOUT: Duration = Duration::from_secs(300);

fn redirect_uri() -> String {
    format!("http://localhost:{CALLBACK_PORT}/callback")
}

/// The registered OAuth client, from the environment.
pub struct Credentials {
    pub id: String,
    secret: String,
}

impl Credentials {
    /// Read `YOUTUBE_CLIENT_ID` / `YOUTUBE_CLIENT_SECRET`.
    ///
    /// `load_dotenv` has already folded `.env` into the environment by the time
    /// anything here runs, so this covers both a shell export and the file.
    pub fn from_env() -> Result<Self> {
        let id = var("YOUTUBE_CLIENT_ID")?;
        let secret = var("YOUTUBE_CLIENT_SECRET")?;
        Ok(Self { id, secret })
    }
}

fn var(key: &str) -> Result<String> {
    let value = std::env::var(key).unwrap_or_default();
    if value.trim().is_empty() {
        bail!("{key} is not set — add it to stream-recorder/.env");
    }
    Ok(value.trim().to_string())
}

/// Run the browser consent and return the granted token.
///
/// The socket is bound *before* the browser opens. The other order is a race
/// that only shows up when the user is fast: Google redirects to a port nothing
/// is listening on yet, the browser reports a connection refused, and the code
/// is spent.
pub fn consent(creds: &Credentials) -> Result<Token> {
    let listener = TcpListener::bind(("127.0.0.1", CALLBACK_PORT)).with_context(|| {
        format!(
            "binding 127.0.0.1:{CALLBACK_PORT} for the OAuth callback — if the \
             Python auth server is running (`saaga-auth serve`), stop it: it \
             holds this port"
        )
    })?;
    listener
        .set_nonblocking(true)
        .context("setting the callback socket non-blocking")?;

    let state = random_state()?;
    let url = authorize_url(creds, &state);
    open_browser(&url)?;

    let (code, returned_state) = wait_for_code(&listener)?;
    if returned_state != state {
        bail!("state mismatch on the OAuth callback — possible CSRF, nothing saved");
    }

    let granted = exchange(creds, &code)?;
    let (channel_id, channel_title) = channel(&granted.access_token)?;

    // Google names the scopes it actually granted, which can be narrower than
    // the ask. Keep its answer, not our request.
    let mut extra = serde_json::Map::new();
    if let Some(scope) = granted.scope.clone() {
        extra.insert("scope".into(), scope.into());
    }

    Ok(Token {
        access_token: granted.access_token,
        refresh_token: granted.refresh_token,
        expires_at: Some(expires_at_from(granted.expires_in.unwrap_or(3600))),
        channel_id: Some(channel_id),
        channel_title,
        client_id: Some(creds.id.clone()),
        extra,
    })
}

/// Trade a refresh token for a new access token.
///
/// Google does not reissue the refresh token here, so the caller keeps the one
/// it had; only the access token and its expiry move.
pub fn refresh(creds: &Credentials, refresh_token: &str) -> Result<(String, String)> {
    let granted: TokenResponse = post_form(
        TOKEN_URL,
        &[
            ("grant_type", "refresh_token"),
            ("refresh_token", refresh_token),
            ("client_id", &creds.id),
            ("client_secret", &creds.secret),
        ],
    )
    .context("refreshing the YouTube access token")?;
    Ok((
        granted.access_token,
        expires_at_from(granted.expires_in.unwrap_or(3600)),
    ))
}

/// The channel a token is bound to: `(id, title)`.
///
/// `mine=true` answers for whichever channel the consent landed on, which is
/// the only way to know before an upload goes to the wrong one.
pub fn channel(access_token: &str) -> Result<(String, Option<String>)> {
    let resp = ureq::get(CHANNELS_URL)
        .query("part", "snippet")
        .query("mine", "true")
        .set("Authorization", &format!("Bearer {access_token}"))
        .call()
        .map_err(describe)
        .context("asking YouTube which channel this token is for")?;
    let list: ChannelList = resp
        .into_json()
        .context("parsing the YouTube channels response")?;
    let first = list.items.into_iter().next().ok_or_else(|| {
        anyhow!(
            "the Google account you signed in with owns no YouTube channel — \
             pick a different account, or create a channel for it"
        )
    })?;
    Ok((first.id, first.snippet.title))
}

fn authorize_url(creds: &Credentials, state: &str) -> String {
    let params = [
        ("client_id", creds.id.as_str()),
        ("redirect_uri", &redirect_uri()),
        ("response_type", "code"),
        ("scope", &SCOPES.join(" ")),
        ("state", state),
        ("access_type", "offline"),
        // select_account puts Google's chooser in front of the grant. Without
        // it Google reuses whatever session is already active, which is how a
        // consent silently binds to a personal channel instead of the brand
        // one; `consent` reads the channel back precisely because this can
        // still be got wrong by hand.
        ("prompt", "select_account consent"),
    ];
    let query: Vec<String> = params
        .iter()
        .map(|(k, v)| format!("{k}={}", encode(v)))
        .collect();
    format!("{AUTH_URL}?{}", query.join("&"))
}

fn exchange(creds: &Credentials, code: &str) -> Result<TokenResponse> {
    post_form(
        TOKEN_URL,
        &[
            ("grant_type", "authorization_code"),
            ("code", code),
            // Google checks this a second time at exchange and rejects a code
            // whose redirect_uri has moved, so it must match the authorize call
            // byte for byte.
            ("redirect_uri", &redirect_uri()),
            ("client_id", &creds.id),
            ("client_secret", &creds.secret),
        ],
    )
    .context("exchanging the authorization code")
}

fn post_form(url: &str, form: &[(&str, &str)]) -> Result<TokenResponse> {
    let resp = ureq::post(url).send_form(form).map_err(describe)?;
    resp.into_json().context("parsing the token response")
}

/// ureq hides the response body on a non-2xx, which is where Google puts the
/// only useful part — `invalid_grant`, `redirect_uri_mismatch`, and friends.
fn describe(err: ureq::Error) -> anyhow::Error {
    match err {
        ureq::Error::Status(code, resp) => {
            let body = resp.into_string().unwrap_or_default();
            anyhow!("Google returned {code}: {}", body.trim())
        }
        other => anyhow!(other),
    }
}

/// Block until the browser hits the callback, or give up.
fn wait_for_code(listener: &TcpListener) -> Result<(String, String)> {
    let deadline = Instant::now() + CONSENT_TIMEOUT;
    loop {
        match listener.accept() {
            Ok((stream, _)) => match handle(stream) {
                // Anything but the callback (favicon, a stray probe) is ignored
                // rather than fatal — the browser sends more than one request.
                Ok(Some(found)) => return Ok(found),
                Ok(None) => continue,
                Err(e) => return Err(e),
            },
            Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                if Instant::now() >= deadline {
                    bail!("timed out waiting for the Google callback — nothing was saved");
                }
                std::thread::sleep(Duration::from_millis(200));
            }
            Err(e) => return Err(anyhow!(e).context("accepting the OAuth callback")),
        }
    }
}

fn handle(stream: TcpStream) -> Result<Option<(String, String)>> {
    stream
        .set_nonblocking(false)
        .context("switching the accepted callback to blocking")?;
    stream
        .set_read_timeout(Some(Duration::from_secs(10)))
        .context("setting a read timeout on the callback")?;

    let mut reader = BufReader::new(&stream);
    let mut request = String::new();
    reader
        .read_line(&mut request)
        .context("reading the callback request")?;

    let target = request.split_whitespace().nth(1).unwrap_or_default();
    let Some((path, query)) = target.split_once('?') else {
        respond(&stream, "Waiting for the Google callback…");
        return Ok(None);
    };
    if path != "/callback" {
        respond(&stream, "Waiting for the Google callback…");
        return Ok(None);
    }

    let mut code = None;
    let mut state = None;
    let mut denied = None;
    for pair in query.split('&') {
        let Some((key, value)) = pair.split_once('=') else {
            continue;
        };
        match key {
            "code" => code = Some(decode(value)),
            "state" => state = Some(decode(value)),
            "error" => denied = Some(decode(value)),
            _ => {}
        }
    }

    if let Some(reason) = denied {
        respond(&stream, "Authorization was declined. You can close this window.");
        bail!("Google returned an error on the callback: {reason}");
    }
    match (code, state) {
        (Some(code), Some(state)) => {
            respond(&stream, "Connected. You can close this window.");
            Ok(Some((code, state)))
        }
        _ => {
            respond(&stream, "That callback had no code. You can close this window.");
            bail!("the Google callback carried no authorization code");
        }
    }
}

fn respond(mut stream: &TcpStream, message: &str) {
    let body = format!("<html><body><h2>{message}</h2></body></html>");
    let response = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    // Best effort: the token matters, the courtesy page does not.
    let _ = stream.write_all(response.as_bytes());
    let _ = stream.flush();
}

fn open_browser(url: &str) -> Result<()> {
    std::process::Command::new("open")
        .arg(url)
        .status()
        .context("opening the browser for Google's consent screen")?;
    Ok(())
}

/// 32 bytes of urandom, hex-encoded, as the CSRF state.
///
/// `read_exact`, not `fs::read`: the device never reaches EOF, so reading it to
/// the end never returns.
fn random_state() -> Result<String> {
    use std::io::Read;

    let mut bytes = [0u8; 32];
    std::fs::File::open("/dev/urandom")
        .context("opening /dev/urandom for the OAuth state")?
        .read_exact(&mut bytes)
        .context("reading randomness for the OAuth state")?;
    Ok(bytes.iter().map(|b| format!("{b:02x}")).collect())
}

fn encode(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for byte in value.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(byte as char)
            }
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}

fn decode(value: &str) -> String {
    let bytes = value.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'%' if i + 2 < bytes.len() => {
                let hex = std::str::from_utf8(&bytes[i + 1..i + 3]).unwrap_or("");
                match u8::from_str_radix(hex, 16) {
                    Ok(byte) => {
                        out.push(byte);
                        i += 3;
                    }
                    Err(_) => {
                        out.push(bytes[i]);
                        i += 1;
                    }
                }
            }
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            byte => {
                out.push(byte);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

#[derive(Deserialize)]
struct TokenResponse {
    access_token: String,
    refresh_token: Option<String>,
    expires_in: Option<i64>,
    scope: Option<String>,
}

#[derive(Deserialize)]
struct ChannelList {
    #[serde(default)]
    items: Vec<ChannelItem>,
}

#[derive(Deserialize)]
struct ChannelItem {
    id: String,
    snippet: ChannelSnippet,
}

#[derive(Deserialize)]
struct ChannelSnippet {
    title: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn creds() -> Credentials {
        Credentials {
            id: "client-1".into(),
            secret: "shh".into(),
        }
    }

    #[test]
    fn the_authorize_url_carries_both_scopes_and_the_loopback_redirect() {
        let url = authorize_url(&creds(), "abc");
        assert!(url.contains("client_id=client-1"), "{url}");
        assert!(
            url.contains("redirect_uri=http%3A%2F%2Flocalhost%3A9876%2Fcallback"),
            "{url}"
        );
        assert!(url.contains("youtube.upload"), "{url}");
        assert!(url.contains("youtube.readonly"), "{url}");
        assert!(!url.contains("drive."), "drive scopes must stay out: {url}");
        assert!(url.contains("state=abc"), "{url}");
    }

    /// The chooser is what stops a consent binding to the wrong channel, so it
    /// has to survive encoding as a space-separated pair.
    #[test]
    fn the_account_chooser_is_requested() {
        let url = authorize_url(&creds(), "abc");
        assert!(url.contains("prompt=select_account%20consent"), "{url}");
    }

    #[test]
    fn state_is_random_per_call() {
        let one = random_state().unwrap();
        let two = random_state().unwrap();
        assert_eq!(one.len(), 64);
        assert_ne!(one, two);
    }

    #[test]
    fn percent_round_trip() {
        let raw = "4/0Ab_c-d.e~f g&h";
        assert_eq!(decode(&encode(raw)), raw);
    }

    #[test]
    fn a_google_code_survives_decoding() {
        assert_eq!(decode("4%2F0AY0e-g7%2Fabc"), "4/0AY0e-g7/abc");
    }
}
