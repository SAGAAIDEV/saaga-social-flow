//! Global ⌃⌥ chords.
//!
//! Same approach as region-marker: `global-hotkey` registers chords through
//! Carbon's `RegisterEventHotKey`, which needs no Accessibility prompt.
//! Start/stop/mark-chapter/mark-take chords land in the control-surface phase
//! once there's a pipeline for them to drive; Quit is enough to prove the
//! mechanism and to have a way out of an Accessory-policy app (no Dock icon,
//! no menu bar) during early testing.

use std::collections::HashMap;

use anyhow::{Context, Result};
use global_hotkey::hotkey::{Code, HotKey, Modifiers};
use global_hotkey::{GlobalHotKeyEvent, GlobalHotKeyManager, HotKeyState};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    Quit,
    Stop,
    NewChapter,
    Retake,
    /// Pause the open chapter, or pick it back up. The break comes out of the
    /// file — see [`crate::capture::pause`] — so a paused minute costs nothing
    /// but the minute. ⌃⌥P, because it is pressed mid-take with the eyes on
    /// whatever is being demonstrated, not on this window.
    TogglePause,
    Notes,
    /// Put every transcribed chapter on the pasteboard. No hotkey: it is a
    /// reach for something outside this app, always with a hand already on the
    /// mouse, and a global chord that overwrites the pasteboard is a bad
    /// neighbour to every other app.
    CopyTranscript,
    Render,
    GenerateTitles,
    GeneratePosts,
    SavePosts,
    /// Notes for a Substack essay. A post step whose output nobody sends —
    /// see [`crate::substack`].
    GenerateSubstack,
    /// Seed and open the standing `substack.notes` prompt.
    EditSubstackPrompt,
    /// Write the article to disk and stop there. No hotkey: it is pressed while
    /// reading the Blog tab, and it is the cheap half of the stage.
    WriteBlog,
    /// Render the draft as a local page and open it in the browser.
    PreviewBlog,
    /// Write the article and post it to the video blog at `/blog` —
    /// see [`crate::blog`].
    PublishBlog,
    /// Seed and open the standing `blog.article` prompt.
    EditBlogPrompt,
    /// Re-read the author and category lists from the CMS. No hotkey:
    /// it is a rare housekeeping action, pressed when someone has just added a
    /// row in Strapi admin.
    RefreshBlogLibrary,
    Distribute,
    SchedulePlan,
    ScheduleApproveAll,
    ScheduleQueue,
    /// Delete queued posts from Buffer. Deliberately has no hotkey — it is the
    /// one action here that destroys live work, so it costs a deliberate click
    /// and a confirmation.
    ScheduleClear,
    /// Upload the longform straight to YouTube. Not a Buffer action — the
    /// queue holds posts for a slot, and this publishes a video when it is ready.
    YoutubeUpload,
    /// Run the YouTube OAuth flow, which opens a browser.
    ConnectYoutube,
    PullAnalytics,
    Reflect,
    ApplyRewrites,
    CaptureFrame,
    /// Grab the screen alone, keeping the photo — see `App::capture_screen`.
    /// No hotkey: it is pressed from the pane that shows both pictures.
    CaptureScreen,
    /// Open the drag-to-select overlay for a blog figure — see
    /// [`crate::figure`]. Its own chord rather than a click, because the whole
    /// point is to catch what is on screen *now*, and reaching for a button in
    /// this app first would put this app on screen instead.
    ///
    /// During a take the drag pauses the chapter and records the author
    /// explaining the figure — see [`crate::figure::aside`] — and the same
    /// chord ends that break and picks the take back up where it left off.
    CaptureFigure,
    /// Write a blurb for every captured figure that has none. No hotkey: it
    /// spends money per figure and is pressed once the recording is over, with
    /// a hand already on the mouse.
    WriteBlurbs,
    GenerateThumbnails,
    /// Draw the procedural thumbnail — see [`crate::card`]. No hotkey: it is
    /// pressed after typing in the two boxes right above the button.
    DrawCard,
    CollectAllAnalytics,
    NewProject,
    NewVersion,
    /// Remove the audio and video from every project untouched for a week —
    /// see [`crate::sessions::stale`]. No hotkey: it deletes recordings, so it
    /// costs a click and a confirmation.
    CleanUp,
}

const BASE: Modifiers = Modifiers::CONTROL.union(Modifiers::ALT);

/// ⌃⇧, for the one chord that is pressed while looking at another app rather
/// than at this one. ⌃⌥S is unbound but ⌃⇧S is what a hand reaches for after
/// years of screenshot tools, and a snip chord that has to be looked up is one
/// nobody takes a figure with.
const SNIP: Modifiers = Modifiers::CONTROL.union(Modifiers::SHIFT);

pub struct Hotkeys {
    _manager: GlobalHotKeyManager,
    by_id: HashMap<u32, Action>,
}

impl Hotkeys {
    pub fn register() -> Result<Hotkeys> {
        let manager = GlobalHotKeyManager::new().context("registering global hotkeys")?;

        let bindings: Vec<(Modifiers, Code, Action)> = vec![
            (BASE, Code::KeyQ, Action::Quit),
            (BASE, Code::KeyC, Action::NewChapter),
            (BASE, Code::KeyT, Action::Retake),
            (BASE, Code::KeyP, Action::TogglePause),
            (SNIP, Code::KeyS, Action::CaptureFigure),
        ];
        let mut by_id = HashMap::new();
        for (mods, code, action) in bindings {
            let key = HotKey::new(Some(mods), code);
            match manager.register(key) {
                Ok(()) => {
                    by_id.insert(key.id(), action);
                }
                Err(e) => eprintln!("stream-recorder: could not bind {key} ({action:?}): {e}"),
            }
        }
        if by_id.is_empty() {
            anyhow::bail!("no hotkeys could be registered");
        }
        Ok(Hotkeys {
            _manager: manager,
            by_id,
        })
    }

    /// Drain everything queued since the last tick.
    pub fn drain(&self) -> Vec<Action> {
        let rx = GlobalHotKeyEvent::receiver();
        let mut out = Vec::new();
        while let Ok(event) = rx.try_recv() {
            if event.state() != HotKeyState::Pressed {
                continue;
            }
            if let Some(action) = self.by_id.get(&event.id()) {
                out.push(*action);
            }
        }
        out
    }
}
