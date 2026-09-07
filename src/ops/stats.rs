//! The first op that reads pixels: frame count, mean luma, and the graph's own
//! drop count, into `chapter-NN.stats.json`.

use anyhow::Result;
use objc2_core_video::{
    kCVPixelFormatType_420YpCbCr8BiPlanarFullRange,
    kCVPixelFormatType_420YpCbCr8BiPlanarVideoRange, kCVReturnSuccess, CVImageBuffer,
    CVPixelBufferGetBaseAddressOfPlane, CVPixelBufferGetBytesPerRowOfPlane,
    CVPixelBufferGetHeightOfPlane, CVPixelBufferGetPixelFormatType, CVPixelBufferGetWidthOfPlane,
    CVPixelBufferLockBaseAddress, CVPixelBufferLockFlags, CVPixelBufferUnlockBaseAddress,
};

use super::graph::Flow;
use super::sidecar::Sidecar;
use super::{Frame, VideoOp};

/// Frames seen, mean luma over the Y plane, and the format the buffers
/// actually carried.
///
/// **Deliberately not in [`default_graph`](super::graphs::default_graph).** It
/// locks and walks the entire Y plane on the capture queue — on a 3456×2234
/// ScreenCaptureKit surface that is ~7.7 MB of reads per frame — and on the
/// camera path it does so while the delegate holds the writer-state `Mutex`
/// that the *audio* callback contends for. Putting it in the default would make
/// every recording pay for it. It ships as
/// [`stats_graph`](super::graphs::stats_graph) instead: exercised by the unit
/// tests below, and available for a manual A/B.
///
/// `apply` always returns [`Flow::Continue`]. It is an analyzer, and stage 1
/// must not change what is appended.
pub struct Stats {
    frames: u64,
    luma_sum: f64,
    luma_frames: u64,
    /// The pixel format of the first buffer that arrived. Reported verbatim
    /// even when it is one this op cannot read — see [`luma_mean`].
    format: Option<u32>,
    /// Frames whose format this op declined to read.
    unreadable: u64,
}

impl Stats {
    pub fn new() -> Stats {
        Stats {
            frames: 0,
            luma_sum: 0.0,
            luma_frames: 0,
            format: None,
            unreadable: 0,
        }
    }
}

impl Default for Stats {
    fn default() -> Self {
        Stats::new()
    }
}

impl VideoOp for Stats {
    fn name(&self) -> &'static str {
        "stats"
    }

    fn apply(&mut self, frame: &mut Frame) -> Result<Flow> {
        self.frames += 1;
        let pixels = frame.pixels();
        if self.format.is_none() {
            self.format = Some(CVPixelBufferGetPixelFormatType(pixels));
        }
        match luma_mean(pixels) {
            Some(mean) => {
                self.luma_sum += mean;
                self.luma_frames += 1;
            }
            None => self.unreadable += 1,
        }
        Ok(Flow::Continue)
    }

    fn close(&mut self, sidecar: &mut Sidecar) -> Result<()> {
        let mean = if self.luma_frames > 0 {
            Some(self.luma_sum / self.luma_frames as f64)
        } else {
            None
        };
        sidecar.record(
            "stats",
            serde_json::json!({
                "frames": self.frames,
                "mean_luma": mean,
                "luma_frames": self.luma_frames,
                "unreadable_frames": self.unreadable,
                "pixel_format": self.format.map(fourcc),
                "graph_frames_seen": sidecar.counters.seen,
                "graph_frames_dropped": sidecar.counters.dropped,
                "graph_bypassed": sidecar.counters.bypassed,
            }),
        );
        Ok(())
    }
}

/// A `CVPixelBuffer` format code as the four characters Apple writes it with:
/// `0x34323076` → `"420v"`.
fn fourcc(format: u32) -> String {
    String::from_utf8_lossy(&format.to_be_bytes()).into_owned()
}

/// Mean of the Y plane, 0..=255 — video-range content clusters in 16..=235.
///
/// `None` when the buffer is not 8-bit bi-planar 4:2:0, or when the lock or the
/// base address fails. Declining is not defensive padding: the camera path's
/// pixel format is never configured (see [`StreamCtx::size`](super::StreamCtx))
/// and is historically `2vuy`, so a camera stats file legitimately reports a
/// null luma plus the format it actually saw, which is more useful than a
/// confidently wrong number.
///
/// Three sharp edges, all of which this codebase would otherwise have to learn
/// the hard way:
///
/// - `CVPixelBufferLockBaseAddress`/`UnlockBaseAddress` are the *only* unsafe
///   calls here — every plane accessor is a safe fn in these bindings. The
///   `ReadOnly` flag must be passed to both, symmetrically; asymmetric use is
///   documented undefined behaviour. So this function has a single exit after
///   the lock and no early `return` between lock and unlock — an early return
///   leaks the lock and eventually deadlocks the capture queue it runs on.
/// - Rows are `bytes_per_row_of_plane(0)` apart, **not** `width`. On a
///   3456-wide IOSurface the padding is real, and treating the plane as
///   contiguous reads padding as pixels and skews the mean.
/// - The plane accessors being safe fns means they happily return NULL when the
///   buffer is not locked. The base address is null-checked explicitly here
///   because the type system will not do it.
fn luma_mean(buffer: &CVImageBuffer) -> Option<f64> {
    let format = CVPixelBufferGetPixelFormatType(buffer);
    if format != kCVPixelFormatType_420YpCbCr8BiPlanarVideoRange
        && format != kCVPixelFormatType_420YpCbCr8BiPlanarFullRange
    {
        return None;
    }

    let flags = CVPixelBufferLockFlags::ReadOnly;
    // SAFETY: `buffer` is a live pixel buffer, and the matching unlock below
    // runs on every path out of this function with the same flags.
    if unsafe { CVPixelBufferLockBaseAddress(buffer, flags) } != kCVReturnSuccess {
        return None;
    }

    let base = CVPixelBufferGetBaseAddressOfPlane(buffer, 0);
    let stride = CVPixelBufferGetBytesPerRowOfPlane(buffer, 0);
    let width = CVPixelBufferGetWidthOfPlane(buffer, 0);
    let height = CVPixelBufferGetHeightOfPlane(buffer, 0);

    let mean = if base.is_null() || width == 0 || height == 0 || stride < width {
        None
    } else {
        let mut sum: u64 = 0;
        for row in 0..height {
            // SAFETY: the buffer is locked, plane 0 is `height` rows of
            // `stride` bytes, and only the first `width` bytes of each row are
            // pixels rather than padding.
            let bytes = unsafe {
                std::slice::from_raw_parts(base.cast::<u8>().add(row * stride), width)
            };
            sum += bytes.iter().map(|&value| u64::from(value)).sum::<u64>();
        }
        Some(sum as f64 / (width * height) as f64)
    };

    // SAFETY: pairs with the lock above, same flags.
    unsafe { CVPixelBufferUnlockBaseAddress(buffer, flags) };
    mean
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ops::frame::test_support::pixel_buffer;
    use objc2_core_foundation::CFRetained;

    const V420: u32 = kCVPixelFormatType_420YpCbCr8BiPlanarVideoRange;
    /// `2vuy` — `kCVPixelFormatType_422YpCbCr8`, the camera path's likely real
    /// format and one this op must decline rather than misread.
    const VUY2: u32 = 0x32767579;

    /// Fill plane 0's *pixels* with `pixel` and its row padding with `padding`.
    ///
    /// Returns the stride, so a test can prove there was padding to skip.
    fn fill_luma(buffer: &CVImageBuffer, pixel: u8, padding: u8) -> usize {
        let flags = CVPixelBufferLockFlags::empty();
        assert_eq!(
            unsafe { CVPixelBufferLockBaseAddress(buffer, flags) },
            kCVReturnSuccess
        );
        let base = CVPixelBufferGetBaseAddressOfPlane(buffer, 0);
        let stride = CVPixelBufferGetBytesPerRowOfPlane(buffer, 0);
        let width = CVPixelBufferGetWidthOfPlane(buffer, 0);
        let height = CVPixelBufferGetHeightOfPlane(buffer, 0);
        assert!(!base.is_null());
        for row in 0..height {
            let bytes =
                unsafe { std::slice::from_raw_parts_mut(base.cast::<u8>().add(row * stride), stride) };
            bytes[..width].fill(pixel);
            bytes[width..].fill(padding);
        }
        unsafe { CVPixelBufferUnlockBaseAddress(buffer, flags) };
        stride
    }

    fn buffer_420v(width: usize, height: usize) -> CFRetained<CVImageBuffer> {
        pixel_buffer(width, height, V420, 64)
    }

    #[test]
    fn luma_mean_reads_a_flat_y_plane() {
        let buffer = buffer_420v(16, 16);
        fill_luma(&buffer, 128, 128);
        let mean = luma_mean(&buffer).expect("420v is readable");
        assert!((mean - 128.0).abs() < 1e-9, "flat 128 plane read as {mean}");
    }

    #[test]
    fn luma_mean_ignores_row_padding() {
        // 18 px wide with a 64-byte row alignment guarantees a padded stride,
        // so this test cannot pass vacuously.
        let buffer = buffer_420v(18, 8);
        let stride = fill_luma(&buffer, 100, 255);
        assert!(
            stride > 18,
            "the test needs a padded stride to be worth anything, got {stride}"
        );
        let mean = luma_mean(&buffer).expect("420v is readable");
        assert!(
            (mean - 100.0).abs() < 1e-9,
            "row padding leaked into the mean: {mean} (stride {stride}, width 18)"
        );
    }

    #[test]
    fn luma_mean_declines_a_format_it_does_not_understand() {
        let buffer = pixel_buffer(16, 16, VUY2, 64);
        assert!(
            luma_mean(&buffer).is_none(),
            "2vuy must be declined, not misread as bi-planar 4:2:0"
        );
    }

    #[test]
    fn a_declined_format_is_still_reported_by_name() {
        assert_eq!(fourcc(V420), "420v");
        assert_eq!(fourcc(VUY2), "2vuy");
    }
}
