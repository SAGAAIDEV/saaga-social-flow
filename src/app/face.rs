//! The Track Face switch: its three states, and what flipping it does.
//!
//! Split out of [`super`] for the same reason [`super::framing`] was — it is
//! one self-contained decision with its own lifecycle — and because that
//! lifecycle is the interesting part. Face tracking is not a boolean the
//! composites read; it is a boolean plus a several-second load, and the gap
//! between them is a real state the operator can see.
//!
//! ```text
//!   Off ──click──► Loading ──worker finishes──► On
//!    ▲                │                         │
//!    └────────────────┴──── click ──────────────┘
//! ```
//!
//! **Loading exists because the first build is slow.** The `mediapipe` crate
//! `dlopen`s a ~34 MB library it fetches from Google's PyPI wheel on first
//! use, then TensorFlow Lite compiles the model's graph. Measured cold that is
//! seconds; warm it is a few hundred milliseconds. Neither belongs on the main
//! thread of an app whose window has to stay live, and neither belongs
//! anywhere near a chapter cut.
//!
//! So the click starts a worker and returns. The preview graph is rebuilt
//! twice — once on the click, which changes nothing, and once when the tracker
//! arrives, which is what actually installs the [`FaceTrack`] node. Until then
//! the recording is framed exactly as it would be with the switch off, which is
//! the only honest thing for it to do: a half-loaded tracker has no idea where
//! anyone is.
//!
//! **A failed load turns the switch back off** rather than leaving it on and
//! silently inert. No network on first run is the likely cause and it is worth
//! saying out loud, because everything downstream of it looks like "tracking
//! just does not work".

use std::sync::Arc;

use crate::app::App;
use crate::face::FaceTracker;
use crate::ops::graphs::Tracking;

impl App {
    /// The tracking to build the preview graph with: `Some` only when the
    /// operator has asked for it *and* the detector is loaded.
    ///
    /// Rebuilt per call rather than cached, because it is two `Arc` clones and
    /// a config clone, and a cached one would be a second place for "is
    /// tracking on" to be true.
    pub(super) fn tracking(&self) -> Option<Tracking> {
        let tracker = self.face_tracker.as_ref()?;
        self.face_config.enabled.then(|| Tracking {
            tracker: Arc::clone(tracker),
            config: self.face_config.clone(),
        })
    }

    /// Whether the switch should read as on. True while loading, because the
    /// operator asked for it and the load is this app's problem, not theirs.
    pub(super) fn face_tracking_wanted(&self) -> bool {
        self.face_config.enabled
    }

    /// The Track Face checkbox moved.
    pub(super) fn set_face_tracking(&mut self, on: bool) {
        if self.face_config.enabled == on {
            return;
        }
        self.face_config.enabled = on;
        self.save_face_config();

        if on {
            self.start_face_tracker();
        } else {
            // The tracker is dropped, not parked. It holds a MediaPipe task
            // and a readback buffer, and someone who switched tracking off is
            // not asking to keep paying for them. The next switch-on rebuilds
            // it, which is fast once the library is cached.
            self.face_tracker = None;
        }
        self.install_preview();
    }

    /// Start the background build, unless one is already running or a tracker
    /// already exists.
    ///
    /// Guarded rather than idempotent-by-luck: without the `face_loading`
    /// check, toggling twice quickly would start two builds, and on a cold
    /// machine that is two simultaneous 34 MB downloads writing the same cache
    /// file.
    pub(super) fn start_face_tracker(&mut self) {
        if self.face_loading || self.face_tracker.is_some() {
            return;
        }
        let Some(renderer) = self.renderer.clone() else {
            eprintln!(
                "stream-recorder: face tracking needs a GPU render context and this \
                 session has none"
            );
            self.face_config.enabled = false;
            return;
        };
        let Some(tx) = self.live.as_ref().map(|live| live.ui_tx.clone()) else {
            // No window yet. `startup` calls this again once there is one.
            return;
        };

        self.face_loading = true;
        println!(
            "stream-recorder: loading the face detector — the first run also fetches \
             libmediapipe (~34 MB) into ~/.cache/mediapipe-rs/"
        );
        FaceTracker::spawn(self.face_config.clone(), renderer, move |built| {
            // The receiver is gone only if the window closed mid-build, which
            // is a shutdown, not an error worth reporting.
            let _ = tx.send(crate::ui::UiEvent::FaceTrackReady(
                built.map_err(|e| format!("{e:#}")),
            ));
        });
    }

    /// The worker finished.
    pub(super) fn face_tracker_ready(&mut self, built: Result<Arc<FaceTracker>, String>) {
        self.face_loading = false;
        match built {
            Ok(tracker) => {
                // Switched off again while it loaded: honour the switch, not
                // the click that started this.
                if !self.face_config.enabled {
                    return;
                }
                println!("stream-recorder: face tracking on");
                self.face_tracker = Some(tracker);
                self.install_preview();
            }
            Err(error) => {
                eprintln!("stream-recorder: face tracking could not start: {error}");
                self.face_config.enabled = false;
                self.face_tracker = None;
                self.save_face_config();
                self.install_preview();
            }
        }
        self.sync_face_control();
    }

    /// Push the switch's state back into the checkbox.
    ///
    /// Needed because the app can turn tracking off on its own — a failed load
    /// does — and a checkbox still showing "on" after that is a control lying
    /// about the recording.
    pub(super) fn sync_face_control(&self) {
        if let Some(live) = self.live.as_ref() {
            live.control_target
                .set_face_tracking(self.face_config.enabled, self.face_loading);
        }
    }

    /// Persist the whole block, not just `enabled`.
    ///
    /// The damping fields are hand-edited in the config file and never touched
    /// here, so writing back what was loaded is a round trip — but writing back
    /// only `enabled` would be too, and this way a config file that predates a
    /// field gains it with its default the first time the switch is used,
    /// rather than staying half-written and hard to discover.
    fn save_face_config(&self) {
        let mut cfg = crate::config::load();
        cfg.face_tracking = self.face_config.clone();
        if let Err(e) = crate::config::save(&cfg) {
            eprintln!("stream-recorder: could not save the face-tracking setting: {e:#}");
        }
    }
}
