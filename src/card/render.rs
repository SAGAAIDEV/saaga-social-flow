//! The card's two strings to a page of HTML, through `bun`.
//!
//! One subprocess, JSON in on stdin and HTML out on stdout. Synchronous and on
//! the calling thread: `bun` starts in about thirty milliseconds and the payload
//! is a few hundred bytes, so the whole call is over inside two frames — where a
//! worker thread would buy an async hop and a way for the button and the picture
//! to disagree about which text was drawn.
//!
//! ## Finding the script
//!
//! `card/render.tsx` sits beside `Cargo.toml` in the source tree, which is not
//! where a built binary lives. Three places are tried in order, and the last is
//! the one that makes a `cargo run` work from anywhere — see [`script`].

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use anyhow::{bail, Context, Result};
use serde_json::json;

use super::Card;

/// The TSX package, relative to whatever root it is found under.
const SCRIPT: &str = "card/render.tsx";

/// Long enough for a cold `bun` on a busy machine, short enough that a hung
/// subprocess does not hold the UI thread until someone force-quits.
const TIMEOUT: std::time::Duration = std::time::Duration::from_secs(20);

/// Where the renderer is.
///
/// `root` is the *project* folder, which is where a per-project override would
/// live if one is ever wanted; the crate's own copy is the fallback. Both are
/// checked so a card can be restyled for one video without editing the shared
/// package — and so this works at all from a binary run outside the source tree.
pub fn script(root: &Path) -> PathBuf {
    let candidates = [
        root.join(SCRIPT),
        // Baked in at compile time: the manifest directory is the crate root,
        // which is the one path that is right for `cargo run` regardless of the
        // working directory.
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(SCRIPT),
    ];
    candidates
        .iter()
        .find(|path| path.is_file())
        .cloned()
        // The manifest copy, so the error names the place it was expected.
        .unwrap_or_else(|| candidates[1].clone())
}

/// The payload the renderer reads. Pure, so the contract can be asserted without
/// starting a subprocess.
pub fn payload(card: &Card, photo: Option<&Path>, width: u32, height: u32) -> serde_json::Value {
    let mut value = json!({
        "format": card.format,
        // Named rather than measured against a literal: the size lives on
        // `Kind::Og` alone, so it cannot drift out of step with a silent
        // fallback to a letterboxed 16:9 as the only symptom.
        "og": (width, height) == super::assets::Kind::Og.size(),
        "title": card.title.trim(),
        "description": card.description.trim(),
        "kicker": card.kicker.trim(),
        "theme": card.theme_or_default(),
        "focus": card.focus_clamped(),
        "width": width,
        "height": height,
    });
    if let Some(photo) = photo {
        // A `file://` URL rather than a path: it is going into an `<img src>`,
        // and a bare absolute path in a page loaded from a file URL resolves
        // against the *volume root*, which silently draws no photograph.
        value["photo"] = json!(file_url(photo));
    }
    value
}

/// Renders the card, returning the page.
pub fn html(root: &Path, card: &Card, photo: Option<&Path>, width: u32, height: u32) -> Result<String> {
    let script = script(root);
    let body = serde_json::to_vec(&payload(card, photo, width, height))
        .context("serializing the card payload")?;

    let mut child = Command::new("bun")
        .arg("run")
        .arg(&script)
        .current_dir(script.parent().unwrap_or(root))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .with_context(|| {
            format!(
                "starting bun to render {} — is bun installed and on PATH?",
                script.display()
            )
        })?;

    child
        .stdin
        .take()
        .context("bun took no stdin")?
        .write_all(&body)
        .context("writing the card payload to bun")?;

    let output = wait(child)?;
    if !output.status.success() {
        // The renderer's usage text goes to stderr, and it is the whole
        // explanation of a rejected payload.
        bail!(
            "the card renderer failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    let html = String::from_utf8(output.stdout).context("the card renderer emitted non-UTF-8")?;
    if html.trim().is_empty() {
        bail!("the card renderer produced an empty page");
    }
    Ok(html)
}

/// `wait_with_output` with a ceiling.
///
/// Without one, a renderer that blocks on stdin — the failure mode of every
/// mistake in the script above — hangs the UI thread with no way back.
fn wait(mut child: std::process::Child) -> Result<std::process::Output> {
    let started = std::time::Instant::now();
    loop {
        match child.try_wait().context("waiting for bun")? {
            Some(_) => return child.wait_with_output().context("reading bun's output"),
            None if started.elapsed() >= TIMEOUT => {
                let _ = child.kill();
                bail!("the card renderer did not finish within {:?}", TIMEOUT);
            }
            None => std::thread::sleep(std::time::Duration::from_millis(5)),
        }
    }
}

/// Same encoding as everywhere else in this crate that builds one.
fn file_url(path: &Path) -> String {
    use std::os::unix::ffi::OsStrExt;

    let mut out = String::from("file://");
    for byte in path.as_os_str().as_bytes() {
        match byte {
            b'/' | b'-' | b'_' | b'.' | b'~' => out.push(*byte as char),
            byte if byte.is_ascii_alphanumeric() => out.push(*byte as char),
            byte => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn card() -> Card {
        Card {
            title: "  Ship it anyway  ".into(),
            description: " Why the queue fell over. ".into(),
            kicker: " saaga ".into(),
            theme: "light".into(),
            focus: 0.32,
            format: Default::default(),
        }
    }

    #[test]
    fn the_payload_trims_and_carries_every_field() {
        let value = payload(&card(), None, 1280, 720);
        assert_eq!(value["title"], "Ship it anyway");
        assert_eq!(value["description"], "Why the queue fell over.");
        assert_eq!(value["kicker"], "saaga");
        assert_eq!(value["theme"], "light");
        assert_eq!(value["focus"], 0.32);
        assert_eq!(value["width"], 1280);
        assert_eq!(value["height"], 720);
        assert!(value.get("photo").is_none(), "a card with no still sends none");
    }

    /// A bare path in an `<img src>` on a page loaded from a file URL resolves
    /// against the volume root and draws nothing, with no error anywhere.
    #[test]
    fn the_photo_travels_as_a_file_url() {
        let value = payload(
            &card(),
            Some(Path::new("/tmp/a project/thumbnails/stills/still-ab.jpg")),
            1280,
            720,
        );
        assert_eq!(
            value["photo"],
            "file:///tmp/a%20project/thumbnails/stills/still-ab.jpg"
        );
    }

    /// A hand-edited `card.json` can say anything, and an unknown theme would
    /// draw a card with no colours rather than fail.
    #[test]
    fn an_unknown_theme_falls_back_rather_than_reaching_the_renderer() {
        let mut odd = card();
        odd.theme = "neon".into();
        assert_eq!(payload(&odd, None, 1280, 720)["theme"], "dark");
    }

    #[test]
    fn the_focus_is_clamped_before_it_is_sent() {
        let mut wild = card();
        wild.focus = 1.8;
        assert_eq!(payload(&wild, None, 1280, 720)["focus"], 1.0);
        wild.focus = -3.0;
        assert_eq!(payload(&wild, None, 1280, 720)["focus"], 0.0);
    }

    /// The script has to be findable from a binary run anywhere, which is what
    /// the compile-time manifest path is for.
    #[test]
    fn the_renderer_is_found_beside_the_crate() {
        let found = script(Path::new("/nonexistent"));
        assert!(found.is_file(), "{} is missing", found.display());
        assert!(found.ends_with("card/render.tsx"));
    }

    /// The real subprocess. Proves the whole TSX toolchain works — bun present,
    /// the factory transpiling, the payload contract matching — which no unit
    /// test of the payload can.
    #[test]
    fn bun_renders_the_card_to_a_page() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"));
        let page = match html(root, &card(), None, 1280, 720) {
            Ok(page) => page,
            // Skipped rather than failed when bun is not installed: this is the
            // one test in the crate with a toolchain outside cargo.
            Err(err) if err.to_string().contains("is bun installed") => {
                eprintln!("skipping: {err:#}");
                return;
            }
            Err(err) => panic!("{err:#}"),
        };
        assert!(page.starts_with("<!doctype html>"), "{}", &page[..80.min(page.len())]);
        assert!(page.contains("Ship it anyway"));
        assert!(page.contains("Why the queue fell over."));
        // The light theme's panel, so the theme reached the layout rather than
        // being defaulted somewhere in the middle.
        assert!(page.contains("#F6F6F6"), "the light theme did not arrive");
        assert!(page.contains("width:1280px"));
    }

    /// An empty title is refused by the renderer, not silently drawn as a blank
    /// card — the payload contract is the one thing both sides must agree on.
    #[test]
    fn the_renderer_refuses_a_card_with_no_title() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"));
        let empty = Card::default();
        match html(root, &empty, None, 1280, 720) {
            Ok(page) => panic!("an empty card rendered: {}", &page[..80.min(page.len())]),
            Err(err) => {
                let message = format!("{err:#}");
                if message.contains("is bun installed") {
                    eprintln!("skipping: {message}");
                    return;
                }
                assert!(message.contains("title"), "{message}");
            }
        }
    }
}
