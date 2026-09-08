//! The unit of work an op sees: one frame's pixels, its time, where it came
//! from, and somewhere to write findings.

use objc2_core_foundation::CFRetained;
use objc2_core_media::{CMClock, CMTime};
use objc2_core_video::{CVImageBuffer, CVPixelBufferGetHeight, CVPixelBufferGetWidth};

use super::sidecar::Sidecar;

/// Which capture source a frame came from.
///
/// A graph is rooted at exactly one of these (see the module docs), so this is
/// less about routing than about letting a shared op — an analyzer, a
/// compositor reading a tap — say which stream it is looking at without being
/// handed a second constructor argument.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StreamId {
    Camera,
    Screen,
}

impl StreamId {
    /// Lowercase name, for log lines and sidecar payloads.
    pub fn as_str(self) -> &'static str {
        match self {
            StreamId::Camera => "camera",
            StreamId::Screen => "screen",
        }
    }
}

/// One frame in flight through a graph.
///
/// `pixels` is a `CFRetained<CVImageBuffer>` rather than a borrow because
/// `CMSampleBuffer::image_buffer()` already hands back exactly that, a retain
/// is not a copy, and stage 2's [`Frame::replace`] needs an owned handle
/// anyway. `CVPixelBuffer`, `CVImageBuffer` and `CVBuffer` are the same Rust
/// type in `objc2-core-video` 0.3.2, so no cast appears anywhere along this
/// path — including at the stage-2 `appendPixelBuffer:withPresentationTime:`
/// call site.
pub struct Frame<'a> {
    pixels: CFRetained<CVImageBuffer>,
    /// **On the host clock**, not the capture stream's — see [`to_host_time`].
    pts: CMTime,
    stream: StreamId,
    /// No op reads this in stage 1 — `passthrough` records nothing and `stats`
    /// reports at close rather than per frame — but it is what makes
    /// "ops can emit data" true of `apply` and not only of `close`.
    #[allow(dead_code)]
    sidecar: &'a mut Sidecar,
    replaced: bool,
}

impl<'a> Frame<'a> {
    pub(crate) fn new(
        pixels: CFRetained<CVImageBuffer>,
        pts: CMTime,
        stream: StreamId,
        sidecar: &'a mut Sidecar,
    ) -> Frame<'a> {
        Frame {
            pixels,
            pts,
            stream,
            sidecar,
            replaced: false,
        }
    }

    pub fn pixels(&self) -> &CVImageBuffer {
        &self.pixels
    }

    /// This frame's pixel dimensions, read off the buffer rather than carried
    /// alongside it.
    ///
    /// Read by `crop`, which is not yet reachable from a graph.
    ///
    /// The camera's size is not known until a buffer arrives (see
    /// [`super::graph::StreamCtx::size`]), and after an op calls
    /// [`replace`](Frame::replace) the frame is a different size from the one
    /// the chapter opened with. Asking the buffer is the only answer that stays
    /// true through both.
    #[allow(dead_code)]
    pub fn size(&self) -> (usize, usize) {
        (
            CVPixelBufferGetWidth(&self.pixels),
            CVPixelBufferGetHeight(&self.pixels),
        )
    }

    #[allow(dead_code)]
    pub fn width(&self) -> usize {
        self.size().0
    }

    #[allow(dead_code)]
    pub fn height(&self) -> usize {
        self.size().1
    }

    /// Consume the frame and take its pixels, whether or not an op replaced
    /// them.
    ///
    /// The graph walk's exit from every op: what comes out is what the next
    /// node — or the sink — sees, so an op that left the buffer alone and one
    /// that swapped it are indistinguishable here by design.
    pub fn into_pixels(self) -> CFRetained<CVImageBuffer> {
        self.pixels
    }

    /// A second owning handle on this frame's pixels — a retain, not a copy.
    ///
    /// Exists for [`Tap::publish`](super::tap::Tap::publish), which has to keep
    /// the buffer alive past the end of the callback that delivered it.
    #[allow(dead_code)] // the tap is wired up in stage 4.
    pub fn pixels_retained(&self) -> CFRetained<CVImageBuffer> {
        self.pixels.clone()
    }

    /// This frame's presentation time, **normalized to the host clock**.
    pub fn pts(&self) -> CMTime {
        self.pts
    }

    pub fn stream(&self) -> StreamId {
        self.stream
    }

    /// Where an op records what it learned. One entry per op per chapter; see
    /// [`Sidecar`].
    #[allow(dead_code)] // see the field.
    pub fn sidecar_mut(&mut self) -> &mut Sidecar {
        self.sidecar
    }

    /// Whether any op in this chain has swapped the pixels out.
    #[allow(dead_code)] // read by the delegates from stage 2 on.
    pub fn was_replaced(&self) -> bool {
        self.replaced
    }

    /// Swap this frame's pixels for a different buffer.
    ///
    /// **This changes nothing today.** Stage 1 leaves both delegates appending
    /// the original `CMSampleBuffer` regardless of the flag, so an op that
    /// calls `replace` in stage 1 does work that never reaches a file. It is
    /// here so ops and the delegates agree on the shape of the thing before the
    /// plumbing behind it exists.
    ///
    /// Honouring it requires an `AVAssetWriterInputPixelBufferAdaptor`, which
    /// stage 2 must construct *between* `addInput` and `startWriting` inside
    /// `av::create_chapter_writer` / `screen_stream::create_screen_writer` —
    /// constructing it after the writer has moved past
    /// `AVAssetWriterStatusUnknown` throws, and an Objective-C exception
    /// unwinding through these `unsafe fn` bindings aborts the process rather
    /// than returning `Err`. Stage 2 must also add `objc2-core-video` to
    /// `objc2-av-foundation`'s feature list in `Cargo.toml`: without it,
    /// `pixelBufferPool` and `appendPixelBuffer_withPresentationTime` are
    /// `cfg`'d out of the compiled bindings entirely and read as a mystery
    /// "no method found".
    ///
    /// The append API must then be chosen **per sink at chapter open**, never
    /// per frame: pool buffers are attachment-free, a video track's format
    /// description has to agree with its image buffers' attachments, and
    /// alternating the two append calls on one input therefore alternates the
    /// track's format description frame by frame. Nothing returns an error for
    /// that; it shows up as a colour shift on playback.
    #[allow(dead_code)] // the stage-2 seam; nothing calls it in stage 1.
    pub fn replace(&mut self, pixels: CFRetained<CVImageBuffer>) {
        self.pixels = pixels;
        self.replaced = true;
    }
}

/// Re-express a capture presentation timestamp on the host clock.
///
/// Done once, at graph entry, so no op ever has to know which stream's clock it
/// is looking at. The camera session runs on the audio interface's clock and
/// the screen stream on ScreenCaptureKit's — measured 3.6 ppm apart on the
/// machine this was built on (see [`crate::timesync`] and `Router::anchors`) —
/// so a camera PTS and a screen PTS are not comparable until both sit on one
/// timeline. The cross-stream tap in stage 4 is nothing *but* a comparison of
/// two PTSs, and normalizing at the boundary is what makes that comparison mean
/// anything.
///
/// `Retained<CMClock>` is safe to park anywhere, unlike every CoreVideo type:
/// `CMClock` is one of the few Core Media types `objc2-core-media` 0.3.2 marks
/// `Send + Sync`.
pub fn to_host_time(pts: CMTime, from: &CMClock) -> CMTime {
    let host = unsafe { CMClock::host_time_clock() };
    crate::timesync::convert(pts, from, &host)
}

#[cfg(test)]
pub(crate) mod test_support {
    //! In-process pixel buffers, so the ops that read pixels are testable with
    //! no camera, no display, and no permission prompt.

    use core::ptr::NonNull;

    use objc2_core_foundation::{CFDictionary, CFNumber, CFRetained, CFString, CFType};
    use objc2_core_video::{
        kCVPixelBufferBytesPerRowAlignmentKey, kCVReturnSuccess, CVImageBuffer, CVPixelBufferCreate,
    };

    /// Allocate a pixel buffer of `format` at `width`×`height`.
    ///
    /// `row_alignment` is forced rather than left to CoreVideo so the padding
    /// tests are deterministic: at 16 px wide the allocator would otherwise be
    /// free to hand back a stride exactly equal to the width, and the test that
    /// exists to prove padding is skipped would silently prove nothing.
    pub(crate) fn pixel_buffer(
        width: usize,
        height: usize,
        format: u32,
        row_alignment: i32,
    ) -> CFRetained<CVImageBuffer> {
        let alignment = CFNumber::new_i32(row_alignment);
        let keys: [&CFString; 1] = unsafe { [kCVPixelBufferBytesPerRowAlignmentKey] };
        let values: [&CFType; 1] = [&alignment];
        let attrs = CFDictionary::<CFString, CFType>::from_slices(&keys, &values);

        let mut out: *mut CVImageBuffer = core::ptr::null_mut();
        // SAFETY: the attributes dictionary holds CFString keys and CFType
        // values as CVPixelBufferCreate requires, and `out` is a valid pointer.
        let status = unsafe {
            CVPixelBufferCreate(
                None,
                width,
                height,
                format,
                Some(attrs.as_opaque()),
                NonNull::from(&mut out),
            )
        };
        assert_eq!(status, kCVReturnSuccess, "CVPixelBufferCreate failed");
        assert!(!out.is_null(), "CVPixelBufferCreate returned NULL");
        // Create rule: the out-param arrives at +1, so adopt it rather than
        // retaining it again.
        unsafe { CFRetained::from_raw(NonNull::new_unchecked(out)) }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use objc2_core_media::CMTimeFlags;

    fn time(value: i64, timescale: i32) -> CMTime {
        CMTime {
            value,
            timescale,
            flags: CMTimeFlags::Valid,
            epoch: 0,
        }
    }

    #[test]
    fn host_time_normalization_is_an_identity_on_the_host_clock() {
        let host = unsafe { CMClock::host_time_clock() };
        let raw = time(123_456, 1_000);
        let normalized = to_host_time(raw, &host);
        assert!(
            (crate::timesync::seconds(normalized) - crate::timesync::seconds(raw)).abs() < 1e-6,
            "converting host time to host time moved it: {} -> {}",
            crate::timesync::seconds(raw),
            crate::timesync::seconds(normalized),
        );
    }

    #[test]
    fn a_stream_id_names_itself() {
        assert_eq!(StreamId::Camera.as_str(), "camera");
        assert_eq!(StreamId::Screen.as_str(), "screen");
    }
}
