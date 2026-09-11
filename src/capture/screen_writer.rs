//! The screen chapter's encoder: hand-built H.264 settings and the
//! `AVAssetWriter` they configure.
//!
//! Separate from [`super::screen_stream`] because it is a different contract.
//! That module talks to ScreenCaptureKit about *what to capture*; this one
//! talks to AVFoundation about *how to encode it*, and it is the only place in
//! the screen path where getting a dictionary key wrong produces a file that
//! looks fine until someone tries to play it.

use anyhow::{anyhow, bail, Context, Result};
use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2_av_foundation::{
    AVAssetWriter, AVAssetWriterInput, AVAssetWriterInputPixelBufferAdaptor,
};
use objc2_foundation::{NSDictionary, NSNumber, NSString};

use objc2_core_foundation::CFString;
use objc2_core_video::{
    kCVPixelBufferHeightKey, kCVPixelBufferPixelFormatTypeKey, kCVPixelBufferWidthKey,
};

use super::screen_stream::FPS;
use crate::region::PixelSize;

/// Hand-built H.264 settings for a screen chapter's writer input.
///
/// There is no capture session here to ask for recommended settings, so this
/// dictionary is the whole contract with the encoder. Passing `None` to
/// `assetWriterInputWithMediaType:outputSettings:` instead would mean
/// passthrough — raw frames muxed into an mp4, which players render as
/// garbage. That exact mistake already shipped once on the audio path; see
/// `capture::av::create_chapter_writer`.
pub fn video_settings(
    width: usize,
    height: usize,
) -> Result<Retained<NSDictionary<NSString, AnyObject>>> {
    use objc2_av_foundation::{
        AVVideoAverageBitRateKey, AVVideoCodecKey, AVVideoCodecTypeH264,
        AVVideoCompressionPropertiesKey, AVVideoHeightKey, AVVideoWidthKey,
    };

    // Every one of these is an Option: the framework symbol is absent on
    // older systems. Missing any of them means the dictionary would be
    // incomplete, and an incomplete settings dict is worse than none at all
    // (see the doc comment) — so fail here rather than degrade.
    let missing = |name: &str| anyhow!("{name} is unavailable on this system");
    let codec = unsafe { AVVideoCodecTypeH264 }.ok_or_else(|| missing("AVVideoCodecTypeH264"))?;
    let codec_key = unsafe { AVVideoCodecKey }.ok_or_else(|| missing("AVVideoCodecKey"))?;
    let width_key = unsafe { AVVideoWidthKey }.ok_or_else(|| missing("AVVideoWidthKey"))?;
    let height_key = unsafe { AVVideoHeightKey }.ok_or_else(|| missing("AVVideoHeightKey"))?;
    let compression_key = unsafe { AVVideoCompressionPropertiesKey }
        .ok_or_else(|| missing("AVVideoCompressionPropertiesKey"))?;
    let bitrate_key =
        unsafe { AVVideoAverageBitRateKey }.ok_or_else(|| missing("AVVideoAverageBitRateKey"))?;

    let bitrate_number = NSNumber::new_i64(target_bitrate(width, height));
    let compression: Retained<NSDictionary<NSString, AnyObject>> =
        NSDictionary::from_slices(&[bitrate_key], &[bitrate_number.as_ref() as &AnyObject]);

    let width_number = NSNumber::new_usize(width);
    let height_number = NSNumber::new_usize(height);
    Ok(NSDictionary::from_slices(
        &[codec_key, width_key, height_key, compression_key],
        &[
            codec.as_ref() as &AnyObject,
            width_number.as_ref() as &AnyObject,
            height_number.as_ref() as &AnyObject,
            compression.as_ref() as &AnyObject,
        ],
    ))
}

/// Create the `AVAssetWriter` + input for one screen chapter file.
///
/// The counterpart to [`crate::capture::av::create_chapter_writer`], and it
/// carries the same warning: `settings` must be a complete dictionary, never
/// `None`.
/// One chapter file's writer, its input, and the adaptor that lets an op's
/// *modified* pixels reach it.
///
/// The adaptor is the difference between a graph that can analyse frames and
/// one that can change them. Without it, the only way into the encoder is
/// `appendSampleBuffer` with the original `CMSampleBuffer` — the buffer the
/// window server delivered, which no crop can touch.
pub struct ScreenWriter {
    pub writer: Retained<AVAssetWriter>,
    pub input: Retained<AVAssetWriterInput>,
    pub adaptor: Retained<AVAssetWriterInputPixelBufferAdaptor>,
}

pub fn create_screen_writer(
    settings: &NSDictionary<NSString, AnyObject>,
    out_path: &std::path::Path,
    size: PixelSize,
) -> Result<ScreenWriter> {
    use objc2_av_foundation::{AVFileTypeMPEG4, AVMediaTypeVideo};
    use objc2_foundation::NSURL;

    let video_type =
        unsafe { AVMediaTypeVideo }.ok_or_else(|| anyhow!("AVMediaTypeVideo unavailable"))?;
    let file_type =
        unsafe { AVFileTypeMPEG4 }.ok_or_else(|| anyhow!("AVFileTypeMPEG4 unavailable"))?;

    // Same reasoning as the camera writer: a session directory can vanish
    // mid-session, and AVAssetWriter reports that only as "Cannot create file".
    if let Some(dir) = out_path.parent() {
        std::fs::create_dir_all(dir)
            .with_context(|| format!("creating output directory {}", dir.display()))?;
    }

    let url_string = NSString::from_str(&out_path.to_string_lossy());
    let url = NSURL::fileURLWithPath(&url_string);
    let writer = unsafe { AVAssetWriter::assetWriterWithURL_fileType_error(&url, file_type) }
        .map_err(|e| anyhow!("could not create the screen asset writer: {e:?}"))?;

    let input = unsafe {
        AVAssetWriterInput::assetWriterInputWithMediaType_outputSettings(video_type, Some(settings))
    };
    unsafe { input.setExpectsMediaDataInRealTime(true) };

    // Built here, between `addInput` and `startWriting`, and that ordering is
    // not stylistic. Constructing the adaptor after the writer has moved past
    // `AVAssetWriterStatusUnknown` throws an Objective-C exception, and an
    // exception unwinding through these `unsafe fn` bindings aborts the process
    // rather than returning `Err` — the recorder would die with no message on
    // the frame a chapter opened.
    //
    // The attributes dictionary must carry width, height *and* a pixel format:
    // the header states `pixelBufferPool` throws without all three, and a pool
    // that throws at chapter open is the same abort by a different route.
    let attributes = pixel_buffer_attributes(size)?;
    let adaptor = unsafe {
        AVAssetWriterInputPixelBufferAdaptor::assetWriterInputPixelBufferAdaptorWithAssetWriterInput_sourcePixelBufferAttributes(
            &input,
            Some(&attributes),
        )
    };

    unsafe {
        if !writer.canAddInput(&input) {
            bail!("the screen writer refused its video input");
        }
        writer.addInput(&input);
        if !writer.startWriting() {
            bail!(
                "startWriting failed for the screen file: {:?}",
                writer.error()
            );
        }
    }

    Ok(ScreenWriter {
        writer,
        input,
        adaptor,
    })
}

/// Source pixel buffer attributes for the adaptor's pool.
///
/// `420v` to match what the stream delivers and what the hardware H.264 encoder
/// consumes natively — the same reasoning as `screen_stream`'s
/// `setPixelFormat`. A pool in any other format would make every frame pay for
/// a conversion the encoder did not ask for.
fn pixel_buffer_attributes(size: PixelSize) -> Result<Retained<NSDictionary<NSString, AnyObject>>> {
    let format = NSNumber::new_u32(u32::from_be_bytes(*b"420v"));
    let width = NSNumber::new_usize(size.w);
    let height = NSNumber::new_usize(size.h);

    // SAFETY: these three are CoreVideo's own key constants, read from the
    // framework rather than spelled out as string literals so a renamed key is
    // a link error instead of a pool that silently refuses to build.
    let keys: [&NSString; 3] = unsafe {
        [
            ns_key(kCVPixelBufferPixelFormatTypeKey),
            ns_key(kCVPixelBufferWidthKey),
            ns_key(kCVPixelBufferHeightKey),
        ]
    };
    Ok(NSDictionary::from_slices(
        &keys,
        &[
            format.as_ref() as &AnyObject,
            width.as_ref() as &AnyObject,
            height.as_ref() as &AnyObject,
        ],
    ))
}

/// Read a CoreVideo `CFString` key as the `NSString` an `NSDictionary` wants.
///
/// CoreVideo hands out `&'static CFString`; the pixel buffer adaptor takes an
/// `NSDictionary<NSString, _>`. objc2 models the two as distinct types and
/// offers no conversion, but `CFString` and `NSString` are **toll-free
/// bridged** — one object, two names — so the pointer is already valid as both.
/// The alternative is hardcoding `"Width"`, `"Height"` and `"PixelFormatType"`
/// as literals, which trades a justified cast for an unchecked copy of a
/// framework ABI.
///
/// # Safety
///
/// `key` must be a genuine CoreFoundation string. Every caller passes a
/// framework constant, which is.
unsafe fn ns_key(key: &'static CFString) -> &'static NSString {
    // SAFETY: toll-free bridging guarantees a CFStringRef is a valid NSString
    // instance, and both are `repr(C)` opaque types here, so the reference is
    // sound for the 'static lifetime the constant already has.
    unsafe { &*(key as *const CFString as *const NSString) }
}

/// Bits per pixel per frame the composites are encoded at.
///
/// This was 0.1, on the reasoning that screen content is flat colour and sharp
/// text, which H.264 codes cheaply. It gave a 1080p master 6.2 Mbps, and that
/// master is not a screen: it is the screen scaled into a slot beside a camera
/// feed, and the camera's noise and motion eat the budget the text needed.
/// The result read soft, and every stage after it — the cut at CRF 18, the
/// YouTube re-encode — could only inherit that. At 0.25 a 1080p master is
/// 15.6 Mbps, about twice YouTube's recommended upload rate for 1080p30 and
/// what a master feeding a re-encode should be. Roughly 2.5× the disk of
/// before: a ten-minute take is about 1.2 GB per orientation.
const BITS_PER_PIXEL_FRAME: f64 = 0.25;
/// Below this even a small capture reads soft; above it a 5K display would be
/// asking for more than any player needs.
const BITRATE_FLOOR: i64 = 8_000_000;
const BITRATE_CEILING: i64 = 60_000_000;

/// The average bitrate asked of the encoder for a `width`×`height` composite.
pub fn target_bitrate(width: usize, height: usize) -> i64 {
    let pixels = (width * height) as f64;
    ((pixels * f64::from(FPS) * BITS_PER_PIXEL_FRAME) as i64).clamp(BITRATE_FLOOR, BITRATE_CEILING)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The 1080p master that read soft was 6.2 Mbps. It is now within sight of
    /// what a re-encode wants fed, and the clamps still hold at both ends.
    #[test]
    fn a_1080p_master_is_encoded_at_a_rate_text_survives() {
        assert_eq!(target_bitrate(1920, 1080), 15_552_000);
        assert_eq!(target_bitrate(640, 480), BITRATE_FLOOR);
        assert_eq!(target_bitrate(6016, 3384), BITRATE_CEILING);
        assert!(target_bitrate(1920, 1080) > 2 * 6_200_000);
    }

    #[test]
    fn video_settings_clamps_the_bitrate_for_tiny_and_huge_displays() {
        // A 640×480 stream would compute well under the floor.
        let small = video_settings(640, 480).expect("settings build");
        // A 6K display would compute well over the ceiling.
        let large = video_settings(6016, 3384).expect("settings build");
        // Both must still produce a complete dictionary — the point of the
        // clamp is that no display size can yield an unusable one.
        assert_eq!(small.count(), 4);
        assert_eq!(large.count(), 4);
    }
}
