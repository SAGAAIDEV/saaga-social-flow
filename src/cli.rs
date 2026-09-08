//! Command-line arguments.

use clap::{Parser, Subcommand};

#[derive(Parser, Debug)]
#[command(name = "stream-recorder", about = "Record camera, screen, and mic to separate files off one shared clock")]
pub struct Args {
    #[command(subcommand)]
    pub command: Option<Command>,

    /// Session output root (default: ~/.stream-recorder/sessions).
    #[arg(long, global = true)]
    pub out_root: Option<String>,

    /// Hyperframes block to frame this session for, which decides the screen
    /// region's aspect: talking-head-horizontal, talking-head-vertical,
    /// screen-camera-split, screen-camera-vertical.
    ///
    /// The two talking-head layouts have no screen slot, so picking one records
    /// camera only and starts no screen capture at all.
    #[arg(long, global = true)]
    pub layout: Option<String>,
}

#[derive(Subcommand, Debug)]
pub enum Command {
    /// Check everything a render needs: the component library, the HyperFrames
    /// renderer, and the S3 uploader. No GUI window.
    Doctor,
    /// Report which API keys are set and where each one came from. No GUI window.
    ///
    /// The first thing to run on a new machine: it names the file it would
    /// write, whether the team's shared credentials decrypted, and what is
    /// still missing — without printing any secret.
    Credentials,
    /// Build interactive article components from a recorded session. No publishing.
    BlogComponents(crate::blog::components::Args),
    /// Select a microphone and prove the connection works. No GUI window.
    Mic {
        /// Re-prompt for a microphone even if a default is already saved.
        #[arg(long)]
        reselect: bool,
    },
    /// Select a camera and prove the connection works. No GUI window.
    Camera {
        /// Re-prompt for a camera even if a default is already saved.
        #[arg(long)]
        reselect: bool,
    },
    /// Select a camera and microphone, record both to a single muxed .mp4. No GUI window.
    Av {
        /// Re-prompt for a camera even if a default is already saved.
        #[arg(long)]
        reselect_camera: bool,
        /// Re-prompt for a microphone even if a default is already saved.
        #[arg(long)]
        reselect_mic: bool,
    },
    /// Draw one procedural thumbnail card to a JPEG and exit. No GUI window.
    ///
    /// The same layout the render draws — see `crate::card` — reachable
    /// without a session, which is both how the rasteriser is verified and how a
    /// card gets made for a video recorded before any of this existed.
    Card {
        /// Write a complete artwork set into --out (a directory); requires --still.
        #[arg(long, requires = "still")]
        all_formats: bool,
        /// Thumbnail orientation.
        #[arg(long, value_enum, default_value = "horizontal")]
        format: crate::thumbnail::format::Format,
        /// Where to write the JPEG.
        #[arg(long)]
        out: String,
        /// The headline. A newline in it is a deliberate line break.
        #[arg(long)]
        title: String,
        /// The line under the headline.
        #[arg(long, default_value = "")]
        description: String,
        /// A small orange word above the title.
        #[arg(long, default_value = "")]
        kicker: String,
        /// `dark` or `light`.
        #[arg(long, default_value = "dark")]
        theme: String,
        /// A camera still to put beside the words — right of them on the landscape
        /// card, below them on the portrait one. Omitted draws a title card.
        #[arg(long)]
        still: Option<String>,
        /// Where across the still the subject sits, 0 to 1.
        #[arg(long, default_value_t = 0.5)]
        focus: f64,
    },
    /// Full recording session: select camera and mic, open the control window,
    /// record with live chapter cutting (⌃⌥C) and retakes (⌃⌥T).
    Record {
        /// Re-prompt for a camera even if a default is already saved.
        #[arg(long)]
        reselect_camera: bool,
        /// Re-prompt for a microphone even if a default is already saved.
        #[arg(long)]
        reselect_mic: bool,
    },
}
