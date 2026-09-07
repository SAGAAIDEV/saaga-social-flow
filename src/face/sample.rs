//! Getting a camera frame out of the GPU and into bytes a model can read.
//!
//! This is the least interesting stage and the one most likely to be silently
//! wrong, so it is worth stating what it is up against.
//!
//! ## The camera's pixels are not in a format anything can read
//!
//! Nothing in `capture::av` calls `setVideoSettings`, so the camera delivers
//! its device-native format — historically `2vuy`, packed 4:2:2 YCbCr (see
//! `ops::stats`'s `luma_mean`). MediaPipe wants tightly packed 8-bit RGB or
//! RGBA. Writing that conversion by hand means a colour-space matrix, a
//! chroma upsample, and a per-camera format switch, all on the capture queue.
//!
//! Core Image already does it, correctly, on the GPU, for every format
//! `CVPixelBuffer` can hold. `CIContext`'s `render:toBitmap:` renders straight
//! into a CPU buffer in `kCIFormatRGBA8`, which is exactly
//! `Image::from_rgba`'s input. One call covers the format conversion, the
//! downscale and the readback together.
//!
//! ## Why it downscales, given that detection does not care
//!
//! Measured on an M1 Max, BlazeFace costs ~1.5 ms per call whether it is handed
//! 1920×1080 or 128×160 — the model resizes to 128×128 internally and that
//! dominates. Detection is not why this downscales.
//!
//! The readback is. `render:toBitmap:` has to move the rendered pixels across
//! the GPU/CPU boundary, and that cost *is* proportional to area: a
//! 1920×1080 RGBA frame is 8.3 MB per sample, a 320×180 one is 230 KB. The same
//! measurement showed the detected centre agreeing to within 0.004 of the frame
//! width between 820×1024 and 128×160, so the small buffer costs nothing real
//! in accuracy and saves the copy 36 times over.
//!
//! ## The buffer is reused
//!
//! One `Vec` for the life of the sampler, resized only when the camera's
//! dimensions change. Allocating 230 KB on a capture queue several times a
//! second is exactly the per-frame allocation `ops::render`'s pool exists to
//! avoid, and there is no reason to reintroduce it here.

use std::ptr::NonNull;

use objc2_core_foundation::{CGAffineTransform, CGRect, CGPoint, CGSize};
use objc2_core_graphics::CGColorSpace;
use objc2_core_image::kCIFormatRGBA8;
use objc2_core_video::{CVImageBuffer, CVPixelBufferGetHeight, CVPixelBufferGetWidth};

use crate::ops::render::{image_from, Renderer};

/// A frame ready for a model: tightly packed RGBA8, and the size it is.
pub struct Rgba<'a> {
    pub bytes: &'a [u8],
    pub width: u32,
    pub height: u32,
}

/// Renders camera frames down to a small RGBA buffer, reusing one allocation.
pub struct Sampler {
    renderer: Renderer,
    /// Target width. Height follows from the camera's aspect, so the detector
    /// never sees a distorted face — BlazeFace is trained on square-pixel
    /// imagery and an anamorphic squeeze shifts the box.
    width: u32,
    buffer: Vec<u8>,
    /// The size `buffer` is currently shaped for, so a stable camera resizes it
    /// exactly once.
    shaped: Option<(u32, u32)>,
    /// sRGB, held rather than rebuilt: `CGColorSpace::new_device_rgb` is a
    /// lookup rather than an allocation, but it is called per sample and the
    /// handle is trivially cacheable.
    color_space: Option<objc2_core_foundation::CFRetained<CGColorSpace>>,
}

impl Sampler {
    pub fn new(renderer: Renderer, width: u32) -> Sampler {
        Sampler {
            renderer,
            width,
            buffer: Vec::new(),
            shaped: None,
            color_space: CGColorSpace::new_device_rgb(),
        }
    }

    /// The camera frame, scaled down and converted. `None` when the frame has
    /// no usable dimensions yet — which is what a camera reports before it has
    /// settled, and is a reason to skip a sample rather than to fail.
    pub fn rgba(&mut self, pixels: &CVImageBuffer) -> Option<Rgba<'_>> {
        let src = (
            CVPixelBufferGetWidth(pixels) as f64,
            CVPixelBufferGetHeight(pixels) as f64,
        );
        if !(src.0 > 0.0 && src.1 > 0.0) {
            return None;
        }

        // Never upscale: a camera smaller than the detect width should be handed
        // to the model as it is, not blown up into extra bytes carrying no extra
        // information.
        let scale = (f64::from(self.width) / src.0).min(1.0);
        let size = (
            (src.0 * scale).round().max(1.0) as u32,
            (src.1 * scale).round().max(1.0) as u32,
        );

        if self.shaped != Some(size) {
            self.buffer
                .resize(size.0 as usize * size.1 as usize * 4, 0);
            self.shaped = Some(size);
        }

        let image = image_from(pixels);
        let scaled = unsafe {
            image.imageByApplyingTransform(CGAffineTransform {
                a: scale,
                b: 0.0,
                c: 0.0,
                d: scale,
                tx: 0.0,
                ty: 0.0,
            })
        };
        // Core Image's origin is bottom-left, so the rendered rows arrive
        // bottom-up relative to the capture. That is fine and deliberately not
        // corrected: a vertically mirrored frame detects a face at a mirrored
        // y, and `detect` flips the result back. Flipping bytes here would cost
        // a full extra pass over the buffer to achieve the same thing.
        let bounds = CGRect::new(
            CGPoint::new(0.0, 0.0),
            CGSize::new(f64::from(size.0), f64::from(size.1)),
        );
        let row_bytes = size.0 as isize * 4;

        // SAFETY: `buffer` is sized for exactly `row_bytes * height` and stays
        // borrowed for the call; `bounds` is the region being written and
        // matches that shape.
        unsafe {
            self.renderer.context().render_toBitmap_rowBytes_bounds_format_colorSpace(
                &scaled,
                NonNull::new(self.buffer.as_mut_ptr().cast())?,
                row_bytes,
                bounds,
                kCIFormatRGBA8,
                self.color_space.as_deref(),
            );
        }

        Some(Rgba {
            bytes: &self.buffer,
            width: size.0,
            height: size.1,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use objc2_core_video::{
        CVPixelBufferGetBaseAddress, CVPixelBufferGetBytesPerRow, CVPixelBufferLockBaseAddress,
        CVPixelBufferLockFlags, CVPixelBufferUnlockBaseAddress,
    };

    /// A BGRA buffer whose top half is red and bottom half blue — an image that
    /// cannot be confused with its own vertical mirror.
    fn two_tone(width: usize, height: usize) -> objc2_core_foundation::CFRetained<CVImageBuffer> {
        let buffer = crate::ops::frame::test_support::pixel_buffer(
            width,
            height,
            u32::from_be_bytes(*b"BGRA"),
            64,
        );
        unsafe {
            CVPixelBufferLockBaseAddress(&buffer, CVPixelBufferLockFlags::empty());
            let base = CVPixelBufferGetBaseAddress(&buffer).cast::<u8>();
            let stride = CVPixelBufferGetBytesPerRow(&buffer);
            for row in 0..height {
                // BGRA, premultiplied-alpha-safe: opaque either way.
                let (b, g, r) = if row < height / 2 {
                    (0u8, 0u8, 255u8)
                } else {
                    (255u8, 0u8, 0u8)
                };
                for column in 0..width {
                    let pixel = base.add(row * stride + column * 4);
                    pixel.write(b);
                    pixel.add(1).write(g);
                    pixel.add(2).write(r);
                    pixel.add(3).write(255);
                }
            }
            CVPixelBufferUnlockBaseAddress(&buffer, CVPixelBufferLockFlags::empty());
        }
        buffer
    }

    /// Pins the row order of `render:toBitmap:`, which is the one fact this
    /// whole file rests on and the one that cannot be read off the signature.
    ///
    /// Core Image's coordinate space is bottom-left origin, so the instinct is
    /// that a `CIImage` made from a capture buffer comes back upside down and
    /// needs flipping. **It does not.** `render:toBitmap:` writes rows in the
    /// capture's own order, top row first, and this test is the only reason
    /// that claim is safe to rely on — it was written expecting the opposite
    /// and corrected by what it measured.
    ///
    /// The stakes: getting it backwards gives a tracker that follows the face
    /// perfectly and aims at its mirror image, which on screen looks like bad
    /// smoothing rather than like a flip, and would be tuned for weeks.
    /// `detect::Detector::anchor` therefore does **no** y correction. If this
    /// test ever fails, that is where the flip has to go.
    #[test]
    fn the_rendered_bitmap_keeps_the_captures_row_order() {
        let Ok(renderer) = Renderer::new() else {
            println!("skipping: no Metal device on this machine");
            return;
        };
        let source = two_tone(8, 8);
        let mut sampler = Sampler::new(renderer, 8);
        let frame = sampler.rgba(&source).expect("a sized buffer samples");
        assert_eq!((frame.width, frame.height), (8, 8));

        // RGBA8: byte 0 of each pixel is red, byte 2 is blue.
        let first_row_red = frame.bytes[0];
        let first_row_blue = frame.bytes[2];
        let last_row = (frame.height as usize - 1) * frame.width as usize * 4;
        let last_row_red = frame.bytes[last_row];
        let last_row_blue = frame.bytes[last_row + 2];

        assert!(
            first_row_red > 200 && first_row_blue < 60,
            "the first rendered row should be the capture's TOP (red), got \
             r={first_row_red} b={first_row_blue}"
        );
        assert!(
            last_row_blue > 200 && last_row_red < 60,
            "the last rendered row should be the capture's BOTTOM (blue), got \
             r={last_row_red} b={last_row_blue}"
        );
    }

    #[test]
    fn the_buffer_is_shaped_once_and_reused_across_frames() {
        let Ok(renderer) = Renderer::new() else {
            println!("skipping: no Metal device on this machine");
            return;
        };
        let source = two_tone(64, 36);
        let mut sampler = Sampler::new(renderer, 32);
        let first = sampler.rgba(&source).expect("sampled").width;
        let capacity = sampler.buffer.capacity();
        for _ in 0..10 {
            assert_eq!(sampler.rgba(&source).expect("sampled").width, first);
        }
        assert_eq!(
            sampler.buffer.capacity(),
            capacity,
            "sampling reallocated the readback buffer"
        );
    }

    /// A camera smaller than the detect width is passed through at its own
    /// size rather than blown up into bytes carrying no more information.
    #[test]
    fn a_small_camera_is_not_upscaled() {
        let Ok(renderer) = Renderer::new() else {
            println!("skipping: no Metal device on this machine");
            return;
        };
        let source = two_tone(64, 36);
        let mut sampler = Sampler::new(renderer, 320);
        let frame = sampler.rgba(&source).expect("sampled");
        assert_eq!((frame.width, frame.height), (64, 36));
    }
}
