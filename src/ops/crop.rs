//! Crop and scale a frame to a fixed output size.
//!
//! The first op that changes pixels rather than counting them, and therefore
//! the first that proves the whole transform path: a source rect out of the
//! delivered buffer, drawn into a pool buffer, handed back through
//! [`Frame::replace`] so the delegate appends *that* instead of the original
//! sample buffer.
//!
//! ## Why this exists when `sourceRect` already crops
//!
//! `SCStreamConfiguration::setSourceRect` crops the *screen* at the window
//! server for free, and where that is enough it is strictly better — the
//! excluded pixels are never composited, never encoded, never cross into this
//! process. What it cannot do is crop the same stream two different ways at
//! once, which is exactly what recording both orientations from one capture
//! needs. That is this op's job: one capture in, two differently-cropped sinks
//! out.
//!
//! It has no equivalent on the camera side at all. `AVCaptureConnection` has no
//! `sourceRect`, so every camera crop has to happen here.
//!
//! ## Core Image's y-axis is not the capture's
//!
//! `CIImage` puts its origin at the **bottom-left** and grows y upward, while
//! every rect in this recorder — `PointRect`, a layout slot, a `CVPixelBuffer`'s
//! rows — is top-left with y growing down. A crop rect handed over unflipped
//! takes the mirror-image band of the frame, which is a plausible-looking
//! picture of the wrong thing and survives every dimension assertion.
//! [`Crop::source_rect`] does that flip in one place, and its test is the one
//! that would catch it.

use anyhow::{Context, Result};
use objc2_core_foundation::{CGAffineTransform, CGPoint, CGRect, CGSize};

use super::graph::{Flow, StreamCtx};
use super::render::{image_from, Pool, Renderer};
use super::{Frame, Sidecar, VideoOp};
use crate::region::cover::cover_zoom;
use crate::region::framing::Framing;
use crate::region::PixelSize;
use objc2::rc::Retained;
use objc2_core_image::CIImage;
use objc2_core_video::{CVImageBuffer, CVPixelBufferGetHeight, CVPixelBufferGetWidth};
use std::sync::Arc;

/// Crop `buffer` with object-fit:cover and place it at `slot` on a canvas
/// whose height is `canvas_h`, in Core Image space.
///
/// `framing` decides both how far to punch in and where to aim, and it is asked
/// *per frame* rather than resolved once at open: a tracked framing's answer
/// changes as the subject moves, and the source's own size is not known until a
/// buffer arrives (see [`StreamCtx::size`]).
pub(crate) fn fit_into_slot(
    buffer: &CVImageBuffer,
    slot: (f64, f64, f64, f64),
    canvas_h: f64,
    framing: &Framing,
) -> Option<Retained<CIImage>> {
    let src = (
        CVPixelBufferGetWidth(buffer) as f64,
        CVPixelBufferGetHeight(buffer) as f64,
    );
    let slot_size = (slot.2, slot.3);
    let fitted = cover_zoom(
        src,
        slot_size,
        framing.offset_into(src, slot_size),
        framing.zoom(),
    );
    if !(fitted.visible.0 > 0.0 && fitted.visible.1 > 0.0) {
        return None;
    }
    let ci_crop = flip_rect(fitted.origin, fitted.visible, src.1);
    let image = image_from(buffer);
    let cropped = unsafe { image.imageByCroppingToRect(ci_crop) };
    let origin = unsafe {
        cropped.imageByApplyingTransform(CGAffineTransform {
            a: 1.0,
            b: 0.0,
            c: 0.0,
            d: 1.0,
            tx: -ci_crop.origin.x,
            ty: -ci_crop.origin.y,
        })
    };
    let scaled = unsafe {
        origin.imageByApplyingTransform(CGAffineTransform {
            a: slot.2 / fitted.visible.0,
            b: 0.0,
            c: 0.0,
            d: slot.3 / fitted.visible.1,
            tx: 0.0,
            ty: 0.0,
        })
    };
    Some(unsafe {
        scaled.imageByApplyingTransform(CGAffineTransform {
            a: 1.0,
            b: 0.0,
            c: 0.0,
            d: 1.0,
            tx: slot.0,
            ty: canvas_h - slot.1 - slot.3,
        })
    })
}

pub(crate) fn flip_rect(origin: (f64, f64), visible: (f64, f64), source_height: f64) -> CGRect {
    CGRect::new(
        CGPoint::new(origin.0, source_height - origin.1 - visible.1),
        CGSize::new(visible.0, visible.1),
    )
}

/// Crop `source` (top-left, buffer pixels) out of `buffer` and place it at `slot`.
pub(crate) fn crop_rect_into_slot(
    buffer: &CVImageBuffer,
    source: (f64, f64, f64, f64),
    slot: (f64, f64, f64, f64),
    canvas_h: f64,
) -> Option<Retained<CIImage>> {
    if !(source.2 > 0.0 && source.3 > 0.0 && slot.2 > 0.0 && slot.3 > 0.0) {
        return None;
    }
    let src_h = CVPixelBufferGetHeight(buffer) as f64;
    let ci_crop = flip_rect((source.0, source.1), (source.2, source.3), src_h);
    let image = image_from(buffer);
    let cropped = unsafe { image.imageByCroppingToRect(ci_crop) };
    let origin = unsafe {
        cropped.imageByApplyingTransform(CGAffineTransform {
            a: 1.0,
            b: 0.0,
            c: 0.0,
            d: 1.0,
            tx: -ci_crop.origin.x,
            ty: -ci_crop.origin.y,
        })
    };
    let scaled = unsafe {
        origin.imageByApplyingTransform(CGAffineTransform {
            a: slot.2 / source.2,
            b: 0.0,
            c: 0.0,
            d: slot.3 / source.3,
            tx: 0.0,
            ty: 0.0,
        })
    };
    Some(unsafe {
        scaled.imageByApplyingTransform(CGAffineTransform {
            a: 1.0,
            b: 0.0,
            c: 0.0,
            d: 1.0,
            tx: slot.0,
            ty: canvas_h - slot.1 - slot.3,
        })
    })
}

/// Crop a rect out of each frame and scale it to `output`.
// Nothing builds one yet: a `Crop` only becomes reachable once a graph can
// route its output to a sink, which is the next piece. Written now because it
// is what the render path exists *for*, and because its coordinate flip is
// worth pinning with tests before anything depends on it being right.
#[allow(dead_code)]
pub struct Crop {
    /// The source rect in **capture pixels**, top-left origin.
    source: (f64, f64, f64, f64),
    output: PixelSize,
    renderer: Option<Renderer>,
    pool: Option<Arc<Pool>>,
    /// Frames this op could not render, counted rather than logged per frame —
    /// a failing render on a capture queue would otherwise print thirty lines a
    /// second.
    failed: u64,
}

#[allow(dead_code)]
impl Crop {
    pub fn new(source: (f64, f64, f64, f64), output: PixelSize) -> Crop {
        Crop {
            source,
            output,
            renderer: None,
            pool: None,
            failed: 0,
        }
    }

    /// The crop rect in Core Image's coordinate space.
    ///
    /// Core Image is bottom-left origin; the rect this op is configured with is
    /// top-left, like everything else here. `height` is the *source buffer's*
    /// height, not the crop's — the flip is about where the rect sits in the
    /// frame, so it needs the frame's extent.
    pub fn source_rect(&self, source_height: f64) -> CGRect {
        let (x, y, w, h) = self.source;
        CGRect::new(
            CGPoint::new(x, source_height - y - h),
            CGSize::new(w, h),
        )
    }
}

impl VideoOp for Crop {
    fn name(&self) -> &'static str {
        "crop"
    }

    fn open(&mut self, ctx: &StreamCtx) -> Result<()> {
        // Refuse rather than degrade. An op that cannot draw but reports
        // success would pass frames through at the *capture's* aspect, and the
        // composition would trim the difference with `object-fit: cover` — a
        // wrong-aspect take that nothing reports until the edit.
        self.renderer = Some(
            ctx.renderer
                .clone()
                .context("the crop op needs a GPU render context and this session has none")?,
        );
        self.pool = Some(
            ctx.pool
                .clone()
                .context("the crop op needs a pixel buffer pool and this sink has none")?,
        );
        self.failed = 0;
        Ok(())
    }

    fn apply(&mut self, frame: &mut Frame) -> Result<Flow> {
        let (Some(renderer), Some(pool)) = (self.renderer.as_ref(), self.pool.as_ref()) else {
            // `open` refuses without both, so reaching here means the graph ran
            // an op it never opened. Drop rather than pass the frame through
            // uncropped, for the same reason `open` refuses.
            return Ok(Flow::Drop);
        };

        let source_height = frame.height() as f64;
        let image = image_from(frame.pixels());
        let cropped = unsafe { image.imageByCroppingToRect(self.source_rect(source_height)) };

        let target = match pool.take() {
            Ok(target) => target,
            Err(_) => {
                // The pool is exhausted when the encoder is behind, which is a
                // transient state, not a broken chapter. Dropping this frame
                // lets it catch up; failing the chapter would not.
                self.failed += 1;
                return Ok(Flow::Drop);
            }
        };
        renderer.render(&cropped, &target);
        frame.replace(target);
        Ok(Flow::Continue)
    }

    fn close(&mut self, sidecar: &mut Sidecar) -> Result<()> {
        sidecar.record(
            self.name(),
            serde_json::json!({
                "source": self.source,
                "output": [self.output.w, self.output.h],
                "frames_dropped_for_no_buffer": self.failed,
            }),
        );
        Ok(())
    }
}


/// Cover a source frame into a fixed slot — `object-fit: cover` plus scale.
///
/// Unlike [`Crop`], the source rect is computed per frame from the live buffer
/// size. The camera's dimensions are unknown until a frame arrives, so a
/// talking-head vertical crop cannot be wired as a constant rect at graph
/// build.
pub struct Cover {
    name: &'static str,
    slot: (f64, f64),
    /// How the camera is aimed into `slot`. [`Framing::fixed`] reproduces the
    /// hardcoded centre this field replaced.
    framing: Framing,
    output: PixelSize,
    renderer: Option<Renderer>,
    pool: Option<Arc<Pool>>,
    failed: u64,
}

impl Cover {
    pub fn for_canvas(name: &'static str, canvas: (f64, f64), framing: Framing) -> Cover {
        Cover {
            name,
            slot: canvas,
            framing,
            output: PixelSize::rounded(canvas.0, canvas.1),
            renderer: None,
            pool: None,
            failed: 0,
        }
    }

    fn source_rect(&self, origin: (f64, f64), visible: (f64, f64), source_height: f64) -> CGRect {
        flip_rect(origin, visible, source_height)
    }
}

impl VideoOp for Cover {
    fn name(&self) -> &'static str {
        self.name
    }

    fn open(&mut self, ctx: &StreamCtx) -> Result<()> {
        self.renderer = Some(
            ctx.renderer
                .clone()
                .context("the cover op needs a GPU render context and this session has none")?,
        );
        self.pool = Some(Arc::new(
            Pool::create(self.output.w, self.output.h)
                .context("creating the cover op's pixel buffer pool")?,
        ));
        self.failed = 0;
        Ok(())
    }

    fn apply(&mut self, frame: &mut Frame) -> Result<Flow> {
        let (Some(renderer), Some(pool)) = (self.renderer.as_ref(), self.pool.as_ref()) else {
            return Ok(Flow::Drop);
        };

        let src = (frame.width() as f64, frame.height() as f64);
        let fitted = cover_zoom(
            src,
            self.slot,
            self.framing.offset_into(src, self.slot),
            self.framing.zoom(),
        );
        if !(fitted.visible.0 > 0.0 && fitted.visible.1 > 0.0) {
            self.failed += 1;
            return Ok(Flow::Drop);
        }

        let ci_rect = self.source_rect(fitted.origin, fitted.visible, src.1);
        let image = image_from(frame.pixels());
        let cropped = unsafe { image.imageByCroppingToRect(ci_rect) };
        let to_origin = CGAffineTransform {
            a: 1.0,
            b: 0.0,
            c: 0.0,
            d: 1.0,
            tx: -ci_rect.origin.x,
            ty: -ci_rect.origin.y,
        };
        let origin = unsafe { cropped.imageByApplyingTransform(to_origin) };
        let sx = self.output.w as f64 / fitted.visible.0;
        let sy = self.output.h as f64 / fitted.visible.1;
        let scale = CGAffineTransform {
            a: sx,
            b: 0.0,
            c: 0.0,
            d: sy,
            tx: 0.0,
            ty: 0.0,
        };
        let scaled = unsafe { origin.imageByApplyingTransform(scale) };

        let target = match pool.take() {
            Ok(target) => target,
            Err(_) => {
                self.failed += 1;
                return Ok(Flow::Drop);
            }
        };
        renderer.render(&scaled, &target);
        frame.replace(target);
        Ok(Flow::Continue)
    }

    fn close(&mut self, sidecar: &mut Sidecar) -> Result<()> {
        // Where the framing *ended*, not a setting: for a tracked chapter this
        // is the subject's last known position, which is the number worth
        // having when a take turns out to be framed oddly.
        let anchor = self.framing.anchor().map(|a| [a.0, a.1]);
        sidecar.record(
            self.name(),
            serde_json::json!({
                "slot": [self.slot.0, self.slot.1],
                "zoom": self.framing.zoom(),
                "tracking": self.framing.is_tracking(),
                "final_anchor": anchor,
                "output": [self.output.w, self.output.h],
                "frames_dropped_for_no_buffer": self.failed,
            }),
        );
        Ok(())
    }
}

#[cfg(test)]
#[path = "crop_tests.rs"]
mod tests;
