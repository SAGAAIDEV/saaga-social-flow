//! The GPU path a pixel op writes through: one Core Image context, and the
//! pools it renders into.
//!
//! Every op that changes pixels — a crop, a composite — produces a *new* buffer
//! rather than editing the one the OS delivered, because the delivered buffer
//! is owned by the capture stack and may still be in flight. That new buffer
//! has to come from somewhere and the pixels have to be drawn into it, and this
//! is the one place both happen.
//!
//! ## The context is built once, not per chapter
//!
//! `contextWithMTLDevice:` allocates a Metal device and compiles Core Image's
//! shader pipeline behind it. Doing that at every chapter cut would put a
//! multi-hundred-millisecond stall exactly where the recorder must not have one
//! — between `finish_chapter` and the next writer being installed, which is
//! already the one window where frames are dropped on the floor. So a
//! [`Renderer`] is made once when the session starts and cloned (a retain, not
//! a rebuild) into every graph that needs it.
//!
//! Apple documents `CIContext` as safe to render from on multiple threads, and
//! that is load-bearing here: the context is created on the main thread and
//! used on the two capture queues. Rust never demands `Send` of it — the
//! delegates reach Objective-C as raw pointers and the `Arc<Mutex<…>>` handles
//! are only cloned and locked — which is the same reasoning
//! [`crate::ops`] gives for `VideoOp` having no `Send` supertrait.
//!
//! ## Pool buffers come from the adaptor, and only after `startWriting`
//!
//! [`Pool`] wraps the `CVPixelBufferPool` an `AVAssetWriterInputPixelBufferAdaptor`
//! vends. Recycling through it is not an optimisation to defer: allocating a
//! fresh IOSurface-backed buffer per frame at 30fps is exactly the per-frame
//! allocation the pool exists to avoid, and the adaptor's own pool is already
//! sized and formatted to what its encoder wants.
//!
//! The header is explicit that `pixelBufferPool` is **NULL before
//! `-[AVAssetWriter startWriting]`**, so it is read after the writer has
//! started, never where the adaptor is constructed.

use std::ptr::NonNull;

use anyhow::{anyhow, bail, Result};
use objc2::rc::Retained;
use objc2_av_foundation::AVAssetWriterInputPixelBufferAdaptor;
use objc2_core_foundation::{CFDictionary, CFNumber, CFRetained, CFString, CFType};
use objc2_core_image::{CIContext, CIImage};
use objc2_core_video::{
    kCVPixelBufferHeightKey, kCVPixelBufferIOSurfacePropertiesKey, kCVPixelBufferPixelFormatTypeKey,
    kCVPixelBufferPoolMinimumBufferCountKey, kCVPixelBufferWidthKey, kCVPixelFormatType_32BGRA,
    kCVReturnSuccess, CVImageBuffer, CVPixelBufferPool,
};
use objc2_metal::MTLCreateSystemDefaultDevice;

/// One Core Image context, shared by every op that draws.
// Reached only from its own test until `Node::Sink` evaluation lands and a
// graph actually hands a renderer to an op. Built first deliberately: it is the
// one piece whose *external* dependency could have failed outright, and
// `a_renderer_draws_into_a_buffer_it_was_given` proves on every `cargo test`
// that this machine has a Metal device and Core Image will draw through it.
#[allow(dead_code)]
#[derive(Clone)]
pub struct Renderer {
    context: Retained<CIContext>,
}

#[allow(dead_code)]
impl Renderer {
    /// Build the session's render context.
    ///
    /// Fails rather than falling back to a CPU context. A software Core Image
    /// context would compose at a fraction of the speed on the capture queue
    /// that the audio callback contends for, so a machine with no Metal device
    /// should record without composites rather than record badly with them.
    pub fn new() -> Result<Renderer> {
        let device = MTLCreateSystemDefaultDevice()
            .ok_or_else(|| anyhow!("no Metal device — this machine cannot composite on the GPU"))?;
        Ok(Renderer {
            context: unsafe { CIContext::contextWithMTLDevice(&device) },
        })
    }

    /// Draw `image` into `target`, replacing its contents.
    ///
    /// `target` must be a buffer this process owns — a pool buffer — never one
    /// the capture stack delivered. Rendering into a delivered buffer would
    /// write to memory the encoder or the window server may still be reading.
    pub fn render(&self, image: &CIImage, target: &CVImageBuffer) {
        unsafe { self.context.render_toCVPixelBuffer(image, target) };
    }

    /// The underlying context, for the one caller that renders somewhere other
    /// than a pixel buffer.
    ///
    /// [`crate::face::sample`] reads frames back to the CPU as packed bytes for
    /// a model, which `render:toBitmap:` does in one call and
    /// [`render`](Renderer::render) cannot express at all. Exposed rather than
    /// wrapped because a `render_to_bitmap` method here would need a raw
    /// pointer, a row stride, a `CIFormat` and a colour space threaded through
    /// it — every argument of the call it is hiding, and none of them
    /// meaningful to any other caller.
    pub fn context(&self) -> &CIContext {
        &self.context
    }
}

/// A recycling source of buffers to render into, for one sink.
#[allow(dead_code)]
pub struct Pool {
    pool: CFRetained<CVPixelBufferPool>,
}

#[allow(dead_code)]
impl Pool {
    /// Take the pool an adaptor vends.
    ///
    /// **Only valid after the adaptor's writer has had `startWriting` called.**
    /// The property is documented to be NULL before that, and a `None` here
    /// almost always means the call order slipped rather than that the system
    /// is out of memory — so the error says so.
    pub fn from_adaptor(adaptor: &AVAssetWriterInputPixelBufferAdaptor) -> Result<Pool> {
        let pool = unsafe { adaptor.pixelBufferPool() }.ok_or_else(|| {
            anyhow!(
                "the pixel buffer adaptor vended no pool — its writer has almost \
                 certainly not had startWriting called yet, or its source pixel buffer \
                 attributes are missing width, height or a pixel format"
            )
        })?;
        Ok(Pool { pool: pool.into() })
    }

    /// A buffer to render into, recycled from the pool.
    pub fn take(&self) -> Result<CFRetained<CVImageBuffer>> {
        let mut out: *mut CVImageBuffer = std::ptr::null_mut();
        // SAFETY: `out` is a valid pointer to a null-initialized slot, which is
        // exactly what the out-parameter contract asks for.
        let status = unsafe {
            CVPixelBufferPool::create_pixel_buffer(None, &self.pool, NonNull::from(&mut out))
        };
        if status != kCVReturnSuccess {
            bail!("the pixel buffer pool refused a buffer (CVReturn {status})");
        }
        let Some(buffer) = NonNull::new(out) else {
            bail!("the pixel buffer pool reported success but returned NULL");
        };
        // Create rule: the out-parameter arrives at +1, so adopt it rather than
        // retaining it a second time.
        Ok(unsafe { CFRetained::from_raw(buffer) })
    }

    /// A pool this process owns, sized for one preview (or any sink that has
    /// no writer adaptor).
    ///
    /// IOSurface-backed BGRA so [`AVSampleBufferDisplayLayer`] can enqueue the
    /// buffers and Core Image can render into them.
    pub fn create(width: usize, height: usize) -> Result<Pool> {
        let format = CFNumber::new_i32(kCVPixelFormatType_32BGRA as i32);
        let w = CFNumber::new_i32(width as i32);
        let h = CFNumber::new_i32(height as i32);
        let iosurface = CFDictionary::<CFString, CFType>::from_slices(&[], &[]);
        let buffer_keys: [&CFString; 4] = unsafe {
            [
                kCVPixelBufferPixelFormatTypeKey,
                kCVPixelBufferWidthKey,
                kCVPixelBufferHeightKey,
                kCVPixelBufferIOSurfacePropertiesKey,
            ]
        };
        let buffer_values: [&CFType; 4] = [&format, &w, &h, &iosurface];
        let buffer_attrs = CFDictionary::<CFString, CFType>::from_slices(&buffer_keys, &buffer_values);

        let min = CFNumber::new_i32(8);
        let pool_keys: [&CFString; 1] = unsafe { [kCVPixelBufferPoolMinimumBufferCountKey] };
        let pool_values: [&CFType; 1] = [&min];
        let pool_attrs = CFDictionary::<CFString, CFType>::from_slices(&pool_keys, &pool_values);

        let mut out: *mut CVPixelBufferPool = std::ptr::null_mut();
        let status = unsafe {
            CVPixelBufferPool::create(
                None,
                Some(pool_attrs.as_opaque()),
                Some(buffer_attrs.as_opaque()),
                NonNull::from(&mut out),
            )
        };
        if status != kCVReturnSuccess {
            bail!("could not create a {width}×{height} pixel buffer pool (CVReturn {status})");
        }
        let Some(pool) = NonNull::new(out) else {
            bail!("CVPixelBufferPoolCreate reported success but returned NULL");
        };
        Ok(Pool {
            pool: unsafe { CFRetained::from_raw(pool) },
        })
    }
}

/// A capture buffer as a `CIImage`, ready to be transformed.
#[allow(dead_code)]
pub fn image_from(buffer: &CVImageBuffer) -> Retained<CIImage> {
    unsafe { CIImage::imageWithCVImageBuffer(buffer) }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Proves the whole render path is wired: a Metal device exists, Core Image
    /// builds a context on it, and that context can draw into a buffer.
    ///
    /// Not `#[ignore]`d despite touching the GPU — it needs no camera, no
    /// display permission and no TCC prompt, only a Metal device, which every
    /// machine this runs on has. A failure here means the pixel path is broken
    /// before any op is written, which is worth finding on every `cargo test`
    /// rather than on a hardware run.
    #[test]
    fn a_renderer_draws_into_a_buffer_it_was_given() {
        let Ok(renderer) = Renderer::new() else {
            println!("skipping: no Metal device on this machine");
            return;
        };
        let source = crate::ops::frame::test_support::pixel_buffer(64, 64, u32::from_be_bytes(*b"BGRA"), 64);
        let target = crate::ops::frame::test_support::pixel_buffer(32, 32, u32::from_be_bytes(*b"BGRA"), 64);

        let image = image_from(&source);
        // Half size, so the render also exercises a scale rather than a blit.
        let scaled = unsafe {
            image.imageByCroppingToRect(objc2_core_foundation::CGRect::new(
                objc2_core_foundation::CGPoint::new(0.0, 0.0),
                objc2_core_foundation::CGSize::new(32.0, 32.0),
            ))
        };
        renderer.render(&scaled, &target);
    }
}
