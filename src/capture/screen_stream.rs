//! A running ScreenCaptureKit capture of one display.
//!
//! The camera/mic equivalent is [`crate::capture::av::Connection`], and this
//! deliberately mirrors its lifecycle: assemble, start running with no writer
//! attached, prove frames are flowing, then let the Router install writers at
//! chapter boundaries. Nothing here writes a file.
//!
//! ScreenCaptureKit is not an `AVCaptureSession`, which has two consequences
//! the rest of the pipeline has to know about:
//!
//! 1. There is no `recommendedVideoSettingsForAssetWriter…` to copy encoder
//!    settings from, so the screen writer's settings are hand-built —
//!    see [`video_settings`].
//! 2. Its buffers are timestamped against [`ScreenConnection::sync_clock`],
//!    which is not necessarily the clock the capture session uses. Comparing
//!    a screen PTS with a camera PTS without converting between the two is
//!    only valid if those clocks are the same.

use std::sync::mpsc;
use std::time::{Duration, Instant};

use anyhow::{anyhow, bail, Context, Result};
use block2::RcBlock;
use dispatch2::{DispatchQueue, DispatchQueueAttr, DispatchRetained};
use objc2::rc::Retained;
use objc2::runtime::ProtocolObject;
use objc2::AnyThread;
use objc2_core_media::CMClock;
use objc2_foundation::NSError;
use objc2_screen_capture_kit::{SCDisplay, SCStream, SCStreamConfiguration, SCStreamOutputType};

use super::screen_delegate::ScreenDelegate;
use super::screen_filter::{content_filter, fetch_content, find_display};
use crate::region::{DisplayGeometry, PixelSize, PointRect};

/// What part of a display to capture, and what size to deliver it at.
///
/// The two are deliberately independent. `region` is where the operator aimed
/// and how far they zoomed; `output` is the layout slot's own pixel size and
/// does not move when they do. ScreenCaptureKit resamples between them on the
/// GPU, which buys two things:
///
/// - **A move or a zoom never changes an `AVAssetWriterInput`'s dimensions**,
///   so neither needs a chapter cut. Only switching layout does, because only
///   that changes the slot.
/// - The composition receives exactly the size it was authored for, so nothing
///   downstream resamples a second time.
///
/// A region whose aspect matches `output`'s means `preservesAspectRatio` has no
/// bars to add; the aspect lock in `PointRect::resized` is what guarantees that,
/// and it is the reason that lock is not negotiable.
#[derive(Debug, Clone, Copy)]
pub struct Capture {
    pub region: PointRect,
    pub output: PixelSize,
}

/// Frames per second requested from the window server.
///
/// 30, not 60: a screencast of an editor or a browser has little real motion,
/// and the second video stream competes with camera capture and its encoder
/// for the same machine. `minimumFrameInterval` is a ceiling, not a
/// guarantee — a still screen delivers far fewer, by design.
///
/// Public so the Router can put it in a screen graph's [`crate::ops::StreamCtx`]
/// rather than restating 30 in a second place that could drift from this one.
pub const FPS: i32 = 30;

/// A running screen capture with no writer attached.
pub struct ScreenConnection {
    stream: Retained<SCStream>,
    /// Keep the delegate alive: `addStreamOutput:` does not retain it
    /// strongly enough to survive our dropping it.
    pub delegate: Retained<ScreenDelegate>,
    _queue: DispatchRetained<DispatchQueue>,
    /// Kept so [`set_region`](ScreenConnection::set_region) and
    /// [`refresh_exclusions`](ScreenConnection::refresh_exclusions) can rebuild
    /// a configuration or a filter without going back to ScreenCaptureKit's
    /// async content query — which would turn a region drag into a ten-second
    /// timeout risk.
    display: Retained<SCDisplay>,
    /// The display this is capturing, so a caller can size a new region
    /// without re-deriving the backing scale.
    pub geometry: DisplayGeometry,
    /// What is being captured, or `None` for the whole display at native size.
    pub capture: Option<Capture>,
    /// Output pixel dimensions, which writer settings are built from. Equal to
    /// the layout slot's size once a [`Capture`] is set — *not* the region's
    /// own pixel count, which moves every time the operator zooms.
    pub width: usize,
    pub height: usize,
}

impl ScreenConnection {
    pub fn tap(&self) -> std::sync::Arc<crate::ops::Tap> {
        self.delegate.tap()
    }

    /// Start capturing `display_uid` (a `CGDirectDisplayID` in decimal, as
    /// produced by [`crate::capture::screen::list_displays`]), optionally
    /// narrowed to `capture`.
    ///
    /// A region is a real crop, not a post-process one: `setSourceRect` makes
    /// the window server composite only that part of the display into our
    /// surface, so the rest is never drawn, never encoded, and never crosses
    /// into this process. It is strictly cheaper than capturing everything and
    /// cropping later, which is why the recorder does its framing here rather
    /// than in the op graph.
    pub fn start_capture(display_uid: &str, capture: Option<Capture>) -> Result<ScreenConnection> {
        let target: u32 = display_uid
            .parse()
            .with_context(|| format!("display id {display_uid} is not a CGDirectDisplayID"))?;

        let geometry = super::screen::display_geometry(display_uid)?;
        let (display, own_windows, full_width, full_height) = find_display(target)?;

        let filter = content_filter(&display, &own_windows);
        let config = unsafe { SCStreamConfiguration::new() };
        let (width, height) = apply_capture(&config, capture, (full_width, full_height));
        unsafe {
            // CMTime(1, FPS) is an interval, i.e. the *fastest* rate allowed.
            config.setMinimumFrameInterval(objc2_core_media::CMTime {
                value: 1,
                timescale: FPS,
                flags: objc2_core_media::CMTimeFlags::Valid,
                epoch: 0,
            });
            config.setShowsCursor(true);
            // Match the camera path's pixel layout: 4:2:0 bi-planar video
            // range is what the hardware H.264 encoder consumes natively, so
            // the encoder does not pay for a BGRA conversion on every frame.
            config.setPixelFormat(u32::from_be_bytes(*b"420v"));
        }

        // Writerless, and graphless with it: the op graph arrives per chapter
        // inside the `ScreenState` the Router installs.
        let delegate = ScreenDelegate::new();
        let stream = unsafe {
            SCStream::initWithFilter_configuration_delegate(
                SCStream::alloc(),
                &filter,
                &config,
                Some(ProtocolObject::from_ref(&*delegate)),
            )
        };

        // Serial: frames must be appended to the writer in order.
        let queue = DispatchQueue::new("stream-recorder.screen", DispatchQueueAttr::SERIAL);
        unsafe {
            stream.addStreamOutput_type_sampleHandlerQueue_error(
                ProtocolObject::from_ref(&*delegate),
                SCStreamOutputType::Screen,
                Some(&queue),
            )
        }
        .map_err(|e| anyhow!("could not add the screen stream output: {e:?}"))?;

        start_stream(&stream)?;

        Ok(ScreenConnection {
            stream,
            delegate,
            _queue: queue,
            display,
            geometry,
            capture,
            width,
            height,
        })
    }

    /// Re-aim the capture, without restarting the stream.
    ///
    /// **Safe mid-chapter as long as `output` does not change**, which is the
    /// point of [`Capture`] carrying it separately: a move or a zoom keeps the
    /// same output size, so the live `AVAssetWriterInput` keeps receiving
    /// exactly the buffer dimensions it locked onto at creation and the
    /// operator can reframe while recording.
    ///
    /// Changing `output` — which only a *layout* change does — must still
    /// happen with no writer installed. Nothing here can enforce that:
    /// ScreenCaptureKit does not know about the writer, and AVFoundation is
    /// simply handed buffers of the wrong size and reports nothing.
    /// `Router::reopen_with_screen` is the caller that gets the ordering right.
    ///
    /// Rebuilding the whole `SCStreamConfiguration` rather than mutating the
    /// live one: `configuration` is not readable back off an `SCStream`, so
    /// every field has to be restated anyway, and restating them in one place
    /// keeps this from drifting away from `start_capture`.
    pub fn set_capture(&mut self, capture: Option<Capture>) -> Result<()> {
        let full = display_pixel_size(&self.display, &self.geometry);
        let config = unsafe { SCStreamConfiguration::new() };
        let (width, height) = apply_capture(&config, capture, full);
        unsafe {
            config.setMinimumFrameInterval(objc2_core_media::CMTime {
                value: 1,
                timescale: FPS,
                flags: objc2_core_media::CMTimeFlags::Valid,
                epoch: 0,
            });
            config.setShowsCursor(true);
            config.setPixelFormat(u32::from_be_bytes(*b"420v"));
        }

        await_completion(
            |handler| unsafe {
                self.stream
                    .updateConfiguration_completionHandler(&config, Some(handler))
            },
            "updating the screen stream configuration",
        )?;

        self.capture = capture;
        self.width = width;
        self.height = height;
        Ok(())
    }

    /// Re-read the window list and exclude this process's windows again.
    ///
    /// Needed because `SCShareableContent` is a *snapshot*: a window created
    /// after the filter was built — the region overlay, which only appears when
    /// the operator asks for it — is not in the array the filter was
    /// constructed from, and would otherwise be recorded.
    pub fn refresh_exclusions(&self) -> Result<()> {
        let (_, own_windows) = fetch_content(unsafe { self.display.displayID() })?;
        let filter = content_filter(&self.display, &own_windows);
        await_completion(
            |handler| unsafe {
                self.stream
                    .updateContentFilter_completionHandler(&filter, Some(handler))
            },
            "updating the screen stream content filter",
        )
    }

    /// The clock ScreenCaptureKit timestamps this stream's buffers against.
    pub fn sync_clock(&self) -> Option<Retained<CMClock>> {
        unsafe { self.stream.synchronizationClock() }
    }

    /// Block until frames are actually arriving.
    ///
    /// Only *complete* frames count. A screen with nothing moving on it can
    /// legitimately deliver nothing for a while, so this is a proof the
    /// stream works, not a promise about frame rate — and callers should be
    /// prepared for it to time out on a genuinely idle display.
    pub fn wait_for_warmup(&self, timeout: Duration) -> Result<()> {
        let deadline = Instant::now() + timeout;
        while self.delegate.frames_seen() < 2 {
            if let Some(error) = self.delegate.stop_error() {
                bail!("screen capture stopped during warmup: {error}");
            }
            if Instant::now() > deadline {
                bail!(
                    "screen capture warmup timed out: {} complete frames, {} skipped as idle \
                     (is anything on this display changing?)",
                    self.delegate.frames_seen(),
                    self.delegate.frames_skipped()
                );
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        Ok(())
    }

    /// Stop the stream. Any writer still installed is the Router's to finish.
    pub fn stop(&self) -> Result<()> {
        await_completion(
            |handler| unsafe { self.stream.stopCaptureWithCompletionHandler(Some(handler)) },
            "stopping screen capture",
        )
    }
}

/// Set `config`'s crop and output size, and report the output chosen.
///
/// The two units on this one object are the trap the whole region model exists
/// to close: `setSourceRect` is in **points** in the display's own coordinate
/// space, while `setWidth`/`setHeight` are in **pixels**. Passing the same
/// numbers to both captures a Retina display at half resolution — the mistake
/// [`find_display`] already carries a warning about.
///
/// `preservesAspectRatio` is on by default and would letterbox a mismatch, so
/// the region's aspect has to equal the output's. Nothing here can check that
/// — `PointRect::resized`'s aspect lock is what makes it true upstream, which
/// is why that lock is not negotiable.
fn apply_capture(
    config: &SCStreamConfiguration,
    capture: Option<Capture>,
    full: (usize, usize),
) -> (usize, usize) {
    let (width, height) = match capture {
        Some(capture) => {
            unsafe { config.setSourceRect(capture.region.to_source_rect()) };
            (capture.output.w, capture.output.h)
        }
        // Not `setSourceRect(CGRect::ZERO)` to clear it: a zero rect is a
        // *valid* rect to ScreenCaptureKit, and the header's contract is that
        // an unset sourceRect means the whole display. A fresh
        // SCStreamConfiguration is therefore the only way back to "everything",
        // which is why both callers build one rather than mutating in place.
        None => full,
    };
    unsafe {
        config.setWidth(width);
        config.setHeight(height);
    }
    (width, height)
}

/// This display's full size in pixels, for the no-region case.
fn display_pixel_size(display: &SCDisplay, geometry: &DisplayGeometry) -> (usize, usize) {
    // H.264 requires even dimensions; `geometry.pixels` is the raw mode size.
    let (width, height) = (geometry.pixels.0 & !1, geometry.pixels.1 & !1);
    if width > 0 && height > 0 {
        return (width, height);
    }
    // Only reachable when Core Graphics declined to describe the display *and*
    // its bounds were degenerate, which `display_geometry` already rejects —
    // kept so a future change there cannot silently produce a 0x0 stream.
    let fallback_w = unsafe { display.width() }.max(0) as usize & !1;
    let fallback_h = unsafe { display.height() }.max(0) as usize & !1;
    (fallback_w, fallback_h)
}

fn start_stream(stream: &SCStream) -> Result<()> {
    await_completion(
        |handler| unsafe { stream.startCaptureWithCompletionHandler(Some(handler)) },
        "starting screen capture",
    )
    .map_err(|e| {
        anyhow!(
            "{e:#} — check System Settings > Privacy & Security > Screen & System Audio \
             Recording"
        )
    })
}

/// Run one of ScreenCaptureKit's `…WithCompletionHandler:` calls and block
/// until it answers.
///
/// Four call sites had the same channel-plus-`RcBlock` dance with four
/// slightly different error strings; `updateConfiguration` and
/// `updateContentFilter` would have made six. `what` is a present participle
/// so the two failure messages read as sentences: "starting screen capture
/// failed: …" and "timed out starting screen capture".
fn await_completion(
    call: impl FnOnce(&block2::DynBlock<dyn Fn(*mut NSError)>),
    what: &str,
) -> Result<()> {
    let (tx, rx) = mpsc::channel::<Option<String>>();
    let handler = RcBlock::new(move |error: *mut NSError| {
        let message = unsafe { error.as_ref() }.map(|e| e.localizedDescription().to_string());
        let _ = tx.send(message);
    });
    call(&handler);
    match rx.recv_timeout(Duration::from_secs(10)) {
        Ok(None) => Ok(()),
        Ok(Some(error)) => bail!("{what} failed: {error}"),
        Err(_) => bail!("timed out {what}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Answers the question the whole two-file design rests on: are camera
    /// buffers and screen frames timestamped against the same clock?
    ///
    /// If they are, one anchor time works for both writers untouched. If they
    /// are not, screen frames need [`crate::timesync::convert`] before they
    /// can share a timeline with the camera — and this test prints the offset
    /// so the size of that error is on the record rather than guessed at.
    ///
    /// Needs a saved camera+mic and a real display.
    /// `cargo test -- --ignored --nocapture clocks`
    #[test]
    #[ignore]
    fn clocks_agree_between_capture_session_and_screen_stream() {
        use crate::timesync::{offset_seconds, same_timeline};
        use objc2_core_media::CMClock;

        let cfg = crate::config::load();
        let (camera_uid, audio_uid) = match (cfg.camera_device_uid, cfg.audio_device_uid) {
            (Some(c), Some(a)) => (c, a),
            _ => panic!("needs saved camera+mic defaults; run the record command once first"),
        };
        let display_uid = crate::capture::screen::list_displays()
            .expect("displays list")
            .first()
            .expect("at least one display")
            .uid
            .clone();

        let av = crate::capture::av::Connection::start_capture(&camera_uid, &audio_uid)
            .expect("start capture");
        av.wait_for_warmup(Duration::from_secs(5))
            .expect("av warmup");
        // No region: this measures clocks, and the whole display is the
        // simplest thing to get frames out of.
        let screen =
            ScreenConnection::start_capture(&display_uid, None).expect("start screen capture");
        screen
            .wait_for_warmup(Duration::from_secs(10))
            .expect("screen warmup");

        let host = unsafe { CMClock::host_time_clock() };
        let av_clock = unsafe { av.session.synchronizationClock() }
            .expect("capture session exposes a synchronization clock");
        let screen_clock = screen
            .sync_clock()
            .expect("screen stream exposes a synchronization clock");

        // The rate is the number that matters. A constant offset is harmless
        // (both writers start from one anchor anyway); a rate that is not
        // exactly 1.0 means the two files drift apart for as long as the
        // chapter runs, which no single anchor can fix.
        let report = |label: &str, a: &CMClock, b: &CMClock| {
            let rate = unsafe { objc2_core_media::CMSyncGetRelativeRate(a, b) };
            let ppm = (rate - 1.0) * 1e6;
            println!(
                "{label}: same={} offset={:+.6}s rate={rate:.12} drift={ppm:+.3} ppm \
                 ({:+.1} ms/hour)",
                same_timeline(a, b),
                offset_seconds(a, b),
                ppm * 3.6,
            );
        };
        report("capture session vs host  ", &av_clock, &host);
        report("screen stream   vs host  ", &screen_clock, &host);
        report("screen vs capture session", &screen_clock, &av_clock);
        let offset = offset_seconds(&screen_clock, &av_clock);
        println!(
            "screen frames: {} complete, {} skipped as idle",
            screen.delegate.frames_seen(),
            screen.delegate.frames_skipped()
        );

        screen.stop().expect("stop screen");
        unsafe { av.session.stopRunning() };

        // Not an assertion that they agree — an assertion that we *know*
        // whether they do. A full second apart means the conversion path is
        // mandatory and something in the design is wrong if it is skipped.
        assert!(
            offset.abs() < 1.0,
            "screen and capture clocks are {offset:.3}s apart — chapters cannot be aligned by \
             a shared anchor alone"
        );
    }
}
