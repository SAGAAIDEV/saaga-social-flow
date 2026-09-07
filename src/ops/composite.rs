//! Camera-over-screen (or screen-over-camera) into a layout canvas.
//!
//! The camera frame arrives on this graph. The screen is read from a
//! [`Tap`](super::tap::Tap) — latest-frame, never blocking — because the two
//! streams live on different queues.

use std::sync::Arc;

use anyhow::{Context, Result};
use objc2_core_foundation::{CGPoint, CGRect, CGSize};
use objc2_core_image::{CIColor, CIImage};
use objc2_core_video::{CVPixelBufferGetHeight, CVPixelBufferGetWidth};

use super::crop::{crop_rect_into_slot, fit_into_slot};
use super::graph::{Flow, StreamCtx};
use super::render::{Pool, Renderer};
use super::tap::Tap;
use super::{Frame, Sidecar, VideoOp};
use crate::layouts::{Layout, Topmost};
use crate::region::framing::{Framing, TrackCell};
use crate::region::track::tracked_crop;
use crate::region::PixelSize;

/// One orientation's composite: camera + optional screen, into `layout.canvas`.
pub struct Composite {
    name: &'static str,
    canvas: (f64, f64),
    camera_slot: (f64, f64, f64, f64),
    screen_slot: (f64, f64, f64, f64),
    topmost: Topmost,
    tap: Option<Arc<Tap>>,
    /// Where this orientation's region sits in the tapped screen buffer, in
    /// buffer pixels, top-left origin — the operator's own framing, resolved.
    /// `None` falls back to covering the whole buffer into the screen slot.
    ///
    /// Baked at graph build and then left alone. This is the **authored** rect;
    /// tracking picks a sub-rect of it rather than replacing it, which is what
    /// keeps `App::union_capture` — and therefore the whole `SCStream` — still
    /// while the frame moves.
    screen_rest: Option<(f64, f64, f64, f64)>,
    /// The live tracked framing, when the pointer is being followed.
    ///
    /// Read per frame rather than baked in: rebuilding the graph to move a
    /// crop would cost two pixel-buffer pool allocations and a display-layer
    /// flush every time the pointer moved. `None` is "not tracking", and
    /// reproduces [`screen_rest`](Self::screen_rest) exactly.
    ///
    /// The camera beside it is aimed by [`Framing`], and the two are
    /// deliberately separate mechanisms pointed at different things. What this
    /// one follows is the **pointer** — where the operator has put attention on
    /// a screen they are demonstrating, drawn into the capture by
    /// `setShowsCursor` so the viewer follows it too. This comment used to say
    /// the screen should not be framed at all: "it is a document, not a
    /// subject, and following a face across a slide deck is nobody's idea of a
    /// feature." That is still exactly right about a face. A pointer is not
    /// one.
    screen_track: Option<Arc<TrackCell>>,
    /// How the camera is aimed into `camera_slot`.
    camera_framing: Framing,
    output: PixelSize,
    renderer: Option<Renderer>,
    pool: Option<Arc<Pool>>,
    failed: u64,
}

impl Composite {
    pub fn for_layout(
        name: &'static str,
        layout: &Layout,
        tap: Option<Arc<Tap>>,
        screen_rest: Option<(f64, f64, f64, f64)>,
        screen_track: Option<Arc<TrackCell>>,
        camera_framing: Framing,
    ) -> Composite {
        let screen_slot = layout.screen_slot.unwrap_or(layout.camera_slot);
        Composite {
            name,
            canvas: layout.canvas,
            camera_slot: layout.camera_slot,
            screen_slot,
            topmost: layout.topmost,
            tap,
            screen_rest,
            screen_track,
            camera_framing,
            output: PixelSize::rounded(layout.canvas.0, layout.canvas.1),
            renderer: None,
            pool: None,
            failed: 0,
        }
    }
}


impl Composite {
    /// The rect to crop out of the tapped screen buffer for *this* frame.
    ///
    /// Asked per frame rather than resolved once, for the same reason
    /// `Cover`'s framing is: a tracked answer changes as the pointer moves.
    /// With nothing tracking — or before the tracker has published anything —
    /// this is the authored rect and the composite behaves exactly as it did
    /// before the feature existed.
    fn screen_crop(&self, buffer: (f64, f64)) -> Option<(f64, f64, f64, f64)> {
        let rest = self.screen_rest?;
        let Some(track) = self.screen_track.as_ref().and_then(|cell| cell.get()) else {
            return Some(rest);
        };
        if !(rest.2 > 0.0 && rest.3 > 0.0) {
            return Some(rest);
        }
        // The anchor arrives normalized to the whole captured buffer, which is
        // both where the pointer is and the envelope the frame may travel in —
        // so it needs multiplying out and nothing else.
        let pointer = (track.anchor.0 * buffer.0, track.anchor.1 * buffer.1);
        Some(tracked_crop(
            rest,
            (0.0, 0.0, buffer.0, buffer.1),
            pointer,
            track.punch,
            // The slot's own pixels: one buffer pixel per output pixel is
            // where the punch-in stops, and this is that number.
            (self.screen_slot.2, self.screen_slot.3),
        ))
    }
}

impl VideoOp for Composite {
    fn name(&self) -> &'static str {
        self.name
    }

    fn open(&mut self, ctx: &StreamCtx) -> Result<()> {
        self.renderer = Some(
            ctx.renderer
                .clone()
                .context("the composite op needs a GPU render context and this session has none")?,
        );
        self.pool = Some(Arc::new(
            Pool::create(self.output.w, self.output.h)
                .context("creating the composite op's pixel buffer pool")?,
        ));
        self.failed = 0;
        Ok(())
    }

    fn apply(&mut self, frame: &mut Frame) -> Result<Flow> {
        let (Some(renderer), Some(pool)) = (self.renderer.as_ref(), self.pool.as_ref()) else {
            return Ok(Flow::Drop);
        };

        let canvas_h = self.canvas.1;
        let Some(camera) = fit_into_slot(
            frame.pixels(),
            self.camera_slot,
            canvas_h,
            &self.camera_framing,
        ) else {
            self.failed += 1;
            return Ok(Flow::Drop);
        };

        let composed = match self.tap.as_ref().and_then(|tap| tap.latest()).and_then(|screen| {
            let buffer = (
                CVPixelBufferGetWidth(screen.pixels.get()) as f64,
                CVPixelBufferGetHeight(screen.pixels.get()) as f64,
            );
            match self.screen_crop(buffer) {
                Some(crop) => {
                    crop_rect_into_slot(screen.pixels.get(), crop, self.screen_slot, canvas_h)
                }
                None => fit_into_slot(
                    screen.pixels.get(),
                    self.screen_slot,
                    canvas_h,
                    &Framing::fixed(),
                ),
            }
        }) {
            Some(screen) => match self.topmost {
                Topmost::Screen => unsafe { screen.imageByCompositingOverImage(&camera) },
                Topmost::Camera => unsafe { camera.imageByCompositingOverImage(&screen) },
            },
            None => camera,
        };

        let canvas = CGRect::new(
            CGPoint::new(0.0, 0.0),
            CGSize::new(self.canvas.0, self.canvas.1),
        );
        let black = unsafe {
            CIImage::imageWithColor(&CIColor::colorWithRed_green_blue_alpha(0.0, 0.0, 0.0, 1.0))
        };
        let background = unsafe { black.imageByCroppingToRect(canvas) };
        let finished = unsafe {
            composed
                .imageByCompositingOverImage(&background)
                .imageByCroppingToRect(canvas)
        };

        let target = match pool.take() {
            Ok(target) => target,
            Err(_) => {
                self.failed += 1;
                return Ok(Flow::Drop);
            }
        };
        renderer.render(&finished, &target);
        frame.replace(target);
        Ok(Flow::Continue)
    }

    fn close(&mut self, sidecar: &mut Sidecar) -> Result<()> {
        sidecar.record(
            self.name(),
            serde_json::json!({
                "canvas": [self.canvas.0, self.canvas.1],
                "zoom": self.camera_framing.zoom(),
                "tracking": self.camera_framing.is_tracking(),
                "final_anchor": self.camera_framing.anchor().map(|a| [a.0, a.1]),
                "frames_dropped": self.failed,
            }),
        );
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::layouts::{Layout, Orientation, Pair};
    use crate::ops::graph::Flow;

    #[test]
    fn an_unopened_composite_drops() {
        let layout = Layout::get(Pair::Split, Orientation::Horizontal);
        let mut op =
            Composite::for_layout("composite-horizontal", layout, None, None, None, Framing::fixed());
        let mut sidecar = crate::ops::Sidecar::default();
        let pixels = crate::ops::frame::test_support::pixel_buffer(
            64,
            64,
            u32::from_be_bytes(*b"BGRA"),
            64,
        );
        let mut frame = Frame::new(
            pixels,
            objc2_core_media::CMTime {
                value: 0,
                timescale: 1,
                flags: objc2_core_media::CMTimeFlags::Valid,
                epoch: 0,
            },
            crate::ops::StreamId::Camera,
            &mut sidecar,
        );
        assert!(matches!(op.apply(&mut frame).unwrap(), Flow::Drop));
        assert!(!frame.was_replaced());
    }
}
