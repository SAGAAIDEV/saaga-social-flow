//! stream-recorder — record camera, screen, and mic to three separate files
//! off one shared clock, with take/chapter markers streamed live as they're
//! dropped, not just logged after the fact.
//!
//! Every pipeline (camera, screen, mic) captures raw sample buffers and hands
//! them to its own `AVAssetWriter`; camera and screen additionally run each
//! frame through a per-chapter graph of video ops ([`ops`]) before encode, so
//! effects (face tracking, reframing, a camera-over-screen composite) can be
//! inserted without touching capture or encode. All three writers anchor to one
//! [`timesync::TimeSync`] epoch, which is what makes a marker's timestamp valid
//! against all three output files without per-file translation.
//!
//! ## Layout
//!
//! | module | responsibility |
//! |---|---|
//! | [`cli`] | arguments |
//! | [`app`] | state, window lifecycle, the winit event loop |
//! | [`hotkeys`] | ⌃⌥ chords: start/stop, mark chapter, mark take, quit |
//! | [`timesync`] | the shared host-time epoch every writer and marker anchors to |
//! | [`markers`] | `MarkerEvent{time, class}`, the live event bus, the JSONL subscriber |
//! | [`permissions`] | camera/mic/screen-recording access requests |
//! | [`capture`] | the three sample-buffer sources: camera, screen, mic |
//! | [`layouts`] | the 2x2 of hyperframes layouts, and where each one's screen slot sits |
//! | [`region`] | which part of a display gets captured, in the three coordinate spaces it crosses |
//! | [`overlay`] | the borderless window that draws those regions on the screen and lets them be dragged |
//! | [`figure`] | ⌃⇧S, drag a rectangle: a screenshot, the moment it was taken, and the blurb written about it |
//! | [`card`] | the procedural thumbnail: a TSX layout, rasterised through an offscreen web view |
//! | [`ops`] | the per-chapter `VideoOp` graph camera and screen frames run through |
//! | [`writer`] | the `AVAssetWriter`/`AVAssetWriterInput` wrapper shared by all three outputs |

mod agent;
mod analytics;
mod app;
mod blog;
mod capture;
mod card;
mod cli;
mod config;
mod distribute;
mod edit;
mod face;
mod figure;
mod hotkeys;
mod layouts;
mod longform;
mod markers;
mod notes;
mod ops;
mod overlay;
mod permissions;
mod pointer;
mod posts;
mod titles;
mod reflect;
mod publish;
mod region;
mod review;
mod router;
mod schedule;
mod session;
mod sessions;
mod settings;
mod stage;
mod substack;
mod thumbnail;
mod timesync;
mod track;
mod transcode;
mod ui;
mod writer;
mod video_brief;

use std::path::Path;

use anyhow::Result;
use clap::Parser;

use crate::cli::{Args, Command};

pub(crate) fn load_dotenv() {
    let next_to_crate = Path::new(env!("CARGO_MANIFEST_DIR")).join(".env");
    let screencast = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../screencast/.env");
    let next_to_cwd = std::env::current_dir().ok().map(|d| d.join(".env"));
    // Last, so it never shadows a checkout's own file on a development machine.
    // It is also the only one of these that exists on a machine that installed a
    // release: the binary ships in a tarball, so `CARGO_MANIFEST_DIR` names a
    // path on the CI runner and the working directory is wherever it was
    // launched from. This is the file the Settings tab writes — see
    // `settings::env_path`.
    let per_user = settings::user_env_path().ok();
    for path in [Some(next_to_crate), Some(screencast), next_to_cwd, per_user]
        .into_iter()
        .flatten()
    {
        if !path.is_file() {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(&path) else {
            continue;
        };
        for line in text.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let line = line.strip_prefix("export ").unwrap_or(line);
            let Some((key, val)) = line.split_once('=') else {
                continue;
            };
            let key = key.trim();
            let val = val.trim().trim_matches(['"', '\'']);
            if std::env::var_os(key).is_none() {
                std::env::set_var(key, val);
            }
        }
    }
    // Last, so every `.env` above wins over it: the team file holds the shared
    // saaga credentials, and a personal key or a shell export is an override of
    // those rather than something they should silently replace.
    settings::sops::load();
}

fn main() -> Result<()> {
    load_dotenv();
    agent::init_tracing();
    let args = Args::parse();

    // Resolved before anything is opened: a typo in --layout should be a
    // one-line message, not a camera and a screen stream coming up first.
    let layout = match args.layout.as_deref() {
        None => layouts::Layout::get(layouts::Pair::Split, layouts::Orientation::Horizontal),
        Some(block) => layouts::Layout::from_block(block).ok_or_else(|| {
            anyhow::anyhow!(
                "unknown layout {block:?} — expected one of {}",
                layouts::Layout::block_ids().join(", "),
            )
        })?,
    };

    if let Some(command) = &args.command {
        match command {
            Command::Credentials => return settings::report(&mut std::io::stdout()),
        Command::BlogComponents(request) => return blog::components::run(request),
            Command::Card {
                all_formats,
                format,
                out,
                title,
                description,
                kicker,
                theme,
                still,
                focus,
            } => {
                let request = card::cli::Request {
                    out: std::path::PathBuf::from(out),
                    card: card::Card {
                        format: *format,
                        title: title.clone(),
                        description: description.clone(),
                        kicker: kicker.clone(),
                        theme: theme.clone(),
                        focus: *focus,
                    },
                    still: still.as_deref().map(std::path::PathBuf::from),
                    // Use the selected orientation throughout rasterisation.
                    size: format.size(),
                };
                let drawn = if *all_formats { card::cli::run_set(request)? } else { card::cli::run(request)? };
                println!("{}", drawn.display());
                return Ok(());
            }
            Command::Mic { reselect } => {
                return capture::mic::interactive_connect(*reselect);
            }
            Command::Camera { reselect } => {
                return capture::camera::interactive_connect(*reselect);
            }
            Command::Av {
                reselect_camera,
                reselect_mic,
            } => {
                return capture::av::interactive_connect(*reselect_camera, *reselect_mic);
            }
            Command::Record {
                reselect_camera,
                reselect_mic,
            } => {
                return app::run_record_session(*reselect_camera, *reselect_mic, layout);
            }
        }
    }

    // Bare `cargo run` (no subcommand) is the full recording UI.
    app::run_record_session(false, false, layout)
}
