//! The three sample-buffer sources.
//!
//! Build order: `mic` first (simplest end-to-end proof of
//! `AVCaptureSession`/`AVAssetWriter`/objc2 mechanics, no video, no
//! processing stage), then `camera` (single-stream video, now legacy), then
//! `screen` (highest-uncertainty: ScreenCaptureKit, raw
//! `objc2-screen-capture-kit` vs. the `screencapturekit` wrapper crate is an
//! open build-vs-buy call to spike before committing).
//!
//! The camera and screen paths the record UI actually uses are `av` +
//! `av_delegate` and `screen_stream` + `screen_delegate`; both run each frame
//! through a per-chapter [`crate::ops`] graph before appending it.
//!
//! The screen path is four modules rather than one, because capturing a screen
//! is four separable jobs:
//!
//! | module | responsibility |
//! |---|---|
//! | [`screen`] | enumerating displays for the picker, and each one's geometry in both units |
//! | [`screen_filter`] | resolving a display, and excluding this process's own windows from it |
//! | [`screen_stream`] | the running `SCStream`: its region, its lifecycle, its clock |
//! | [`screen_writer`] | the hand-built H.264 settings and the `AVAssetWriter` they configure |

mod audio_delegate;
pub mod av_delegate;
pub mod device_picker;
pub mod level;
pub mod av;
pub mod camera;
pub mod composed;
pub mod mic;
pub mod pause;
pub mod screen;
pub mod screen_delegate;
pub mod screen_filter;
pub mod screen_stream;
pub mod screen_writer;
mod video_delegate;
