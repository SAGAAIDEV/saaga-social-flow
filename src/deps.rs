//! ffmpeg, which the app cannot do its job without, and getting it installed.
//!
//! Every closed chapter goes through ffmpeg before anything else can happen to
//! it: the mp3 the transcript is made from, the metadata repair, the cut, the
//! waveform, every render. A machine without it records a take and then
//! quietly never transcribes a word of it — the render sits on "Waiting for
//! chapter 02 transcript…" — so this is checked at launch, said in the window
//! rather than on stderr, and installed through Homebrew when Homebrew is
//! there to do it.
//!
//! ## PATH
//!
//! An app opened from Finder or the Dock is handed launchd's `PATH`
//! (`/usr/bin:/bin:/usr/sbin:/sbin`), which has no Homebrew in it. ffmpeg can
//! be installed and still not be found. [`extend_path`] adds Homebrew's bin
//! directories when they exist, so an install — including one this module just
//! made — is found without a relaunch.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::mpsc::Sender;

/// Where Homebrew puts binaries: Apple silicon, then Intel.
const HOMEBREW_BINS: &[&str] = &["/opt/homebrew/bin", "/usr/local/bin"];

/// The programs the app runs. ffprobe ships in the same formula.
const PROGRAMS: &[&str] = &["ffmpeg", "ffprobe"];

/// The one line to run by hand, when the app cannot run it itself.
pub const INSTALL_COMMAND: &str = "brew install ffmpeg";

/// Put Homebrew's bin directories on `PATH` when they exist and are not there.
///
/// Appended, not prepended: someone with a deliberate `PATH` keeps their
/// order, and this only fills in what a Finder launch left out.
///
/// Call from the top of `main`, before any thread exists — it sets the
/// environment.
pub fn extend_path() {
    let current = std::env::var_os("PATH").unwrap_or_default();
    let mut dirs: Vec<PathBuf> = std::env::split_paths(&current).collect();
    let mut added = false;
    for bin in HOMEBREW_BINS {
        let bin = PathBuf::from(bin);
        if bin.is_dir() && !dirs.contains(&bin) {
            dirs.push(bin);
            added = true;
        }
    }
    if added {
        if let Ok(joined) = std::env::join_paths(dirs) {
            // SAFETY: called from the top of `main`, before any thread that
            // reads the environment has been spawned.
            unsafe { std::env::set_var("PATH", joined) };
        }
    }
}

fn on_path(program: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|dir| dir.join(program))
        .find(|candidate| candidate.is_file())
}

/// The programs from [`PROGRAMS`] that are not on `PATH`.
pub fn missing() -> Vec<&'static str> {
    PROGRAMS
        .iter()
        .copied()
        .filter(|program| on_path(program).is_none())
        .collect()
}

/// Homebrew, if this machine has it.
pub fn homebrew() -> Option<PathBuf> {
    on_path("brew").or_else(|| {
        HOMEBREW_BINS
            .iter()
            .map(|dir| Path::new(dir).join("brew"))
            .find(|brew| brew.is_file())
    })
}

/// The error a stage gives when it needs ffmpeg and there is none: what is
/// missing and the command that fixes it.
pub fn require() -> anyhow::Result<()> {
    let missing = missing();
    if missing.is_empty() {
        return Ok(());
    }
    anyhow::bail!(
        "{} not installed — the app installs it through Homebrew at launch; if that \
         did not happen, run `{INSTALL_COMMAND}` in Terminal, then press this again",
        missing.join(" and ")
    )
}

/// What the window says while ffmpeg is missing, before anything is tried.
pub fn missing_notice(brew: bool) -> String {
    let what = missing().join(" and ");
    if brew {
        format!(
            "{what} is not installed — without it chapters are never turned into audio, \
             transcribed or rendered. Installing it with Homebrew now; this can take a \
             few minutes, and recording works meanwhile."
        )
    } else {
        format!(
            "{what} is not installed — without it chapters are never turned into audio, \
             transcribed or rendered. Install Homebrew (https://brew.sh), run \
             `{INSTALL_COMMAND}` in Terminal, then relaunch."
        )
    }
}

pub enum InstallEvent {
    /// ffmpeg and ffprobe are on `PATH` now.
    Installed,
    /// The install ran and ffmpeg is still missing, with the reason.
    Failed(String),
}

/// Run `brew install ffmpeg` on a worker thread and say how it went.
///
/// brew's own output goes to stderr, which the launch log keeps — see
/// [`crate::applog`]. `NONINTERACTIVE` so it never stops to ask.
pub fn spawn_install(brew: PathBuf, tx: Sender<InstallEvent>) {
    let unstarted = tx.clone();
    let spawned = std::thread::Builder::new()
        .name("install-ffmpeg".into())
        .spawn(move || {
            eprintln!("stream-recorder: ffmpeg missing — running `{INSTALL_COMMAND}`");
            let status = Command::new(&brew)
                .args(["install", "ffmpeg"])
                .env("NONINTERACTIVE", "1")
                .status();
            let event = match status {
                Ok(_) if missing().is_empty() => InstallEvent::Installed,
                Ok(status) => InstallEvent::Failed(format!(
                    "`{INSTALL_COMMAND}` exited with {status} and {} is still missing",
                    missing().join(" and ")
                )),
                Err(err) => InstallEvent::Failed(format!("could not run brew: {err}")),
            };
            match &event {
                InstallEvent::Installed => eprintln!("stream-recorder: ffmpeg installed"),
                InstallEvent::Failed(why) => {
                    eprintln!("stream-recorder: ffmpeg install failed: {why}")
                }
            }
            let _ = tx.send(event);
        });
    if let Err(err) = spawned {
        let _ = unstarted.send(InstallEvent::Failed(format!(
            "could not start the install: {err}"
        )));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Both notices have to name the fix: the install the app is running, or
    /// the command and the relaunch when it cannot run one.
    #[test]
    fn the_notice_names_what_happens_next() {
        assert!(missing_notice(true).contains("Installing it with Homebrew"));
        let manual = missing_notice(false);
        assert!(manual.contains(INSTALL_COMMAND), "{manual}");
        assert!(manual.contains("relaunch"), "{manual}");
    }
}
