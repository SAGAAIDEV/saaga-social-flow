//! Writers for composed preview sinks: H.264 files at a layout canvas size.
//!
//! Separate from the camera master (`create_chapter_writer`) because these
//! files are fed *composed* BGRA buffers through a pixel-buffer adaptor, not
//! the original capture sample buffers.

use anyhow::{anyhow, bail, Context, Result};
use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2_av_foundation::{
    AVAssetWriter, AVAssetWriterInput, AVAssetWriterInputPixelBufferAdaptor,
};
use objc2_core_foundation::CFString;
use objc2_core_video::{
    kCVPixelBufferHeightKey, kCVPixelBufferPixelFormatTypeKey, kCVPixelBufferWidthKey,
    kCVPixelFormatType_32BGRA,
};
use objc2_foundation::{NSDictionary, NSNumber, NSString, NSURL};

use super::screen_writer::video_settings;
use crate::ops::OutputSpec;
use crate::region::PixelSize;

pub struct ComposedWriter {
    pub spec: OutputSpec,
    pub writer: Retained<AVAssetWriter>,
    pub video_input: Retained<AVAssetWriterInput>,
    pub audio_input: Option<Retained<AVAssetWriterInput>>,
    pub adaptor: Retained<AVAssetWriterInputPixelBufferAdaptor>,
}

pub fn create_composed_writer(
    spec: OutputSpec,
    audio_settings: Option<&NSDictionary<NSString, AnyObject>>,
    out_path: &std::path::Path,
) -> Result<ComposedWriter> {
    use objc2_av_foundation::{AVFileTypeMPEG4, AVMediaTypeAudio, AVMediaTypeVideo};

    let video_type =
        unsafe { AVMediaTypeVideo }.ok_or_else(|| anyhow!("AVMediaTypeVideo unavailable"))?;
    let file_type =
        unsafe { AVFileTypeMPEG4 }.ok_or_else(|| anyhow!("AVFileTypeMPEG4 unavailable"))?;

    if let Some(dir) = out_path.parent() {
        std::fs::create_dir_all(dir)
            .with_context(|| format!("creating output directory {}", dir.display()))?;
    }

    let settings = video_settings(spec.width(), spec.height())?;
    let url_string = NSString::from_str(&out_path.to_string_lossy());
    let url = NSURL::fileURLWithPath(&url_string);
    let writer = unsafe { AVAssetWriter::assetWriterWithURL_fileType_error(&url, file_type) }
        .map_err(|e| anyhow!("could not create the {} writer: {e:?}", spec.name()))?;

    let video_input = unsafe {
        AVAssetWriterInput::assetWriterInputWithMediaType_outputSettings(
            video_type,
            Some(&settings),
        )
    };
    unsafe { video_input.setExpectsMediaDataInRealTime(true) };

    let attributes = bgra_attributes(PixelSize {
        w: spec.width(),
        h: spec.height(),
    })?;
    let adaptor = unsafe {
        AVAssetWriterInputPixelBufferAdaptor::assetWriterInputPixelBufferAdaptorWithAssetWriterInput_sourcePixelBufferAttributes(
            &video_input,
            Some(&attributes),
        )
    };

    unsafe {
        if !writer.canAddInput(&video_input) {
            bail!("the {} writer refused its video input", spec.name());
        }
        writer.addInput(&video_input);
    }

    let audio_input = if spec.audio() {
        let audio_type =
            unsafe { AVMediaTypeAudio }.ok_or_else(|| anyhow!("AVMediaTypeAudio unavailable"))?;
        let settings = audio_settings.ok_or_else(|| {
            anyhow!(
                "the {} sink asked for audio but no settings were given",
                spec.name()
            )
        })?;
        let input = unsafe {
            AVAssetWriterInput::assetWriterInputWithMediaType_outputSettings(
                audio_type,
                Some(settings),
            )
        };
        unsafe { input.setExpectsMediaDataInRealTime(true) };
        unsafe {
            if !writer.canAddInput(&input) {
                bail!("the {} writer refused its audio input", spec.name());
            }
            writer.addInput(&input);
        }
        Some(input)
    } else {
        None
    };

    unsafe {
        if !writer.startWriting() {
            bail!(
                "startWriting failed for {}: {:?}",
                spec.name(),
                writer.error()
            );
        }
    }

    Ok(ComposedWriter {
        spec,
        writer,
        video_input,
        audio_input,
        adaptor,
    })
}

fn bgra_attributes(size: PixelSize) -> Result<Retained<NSDictionary<NSString, AnyObject>>> {
    let format = NSNumber::new_u32(kCVPixelFormatType_32BGRA);
    let width = NSNumber::new_usize(size.w);
    let height = NSNumber::new_usize(size.h);
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

unsafe fn ns_key(key: &'static CFString) -> &'static NSString {
    unsafe { &*(key as *const CFString as *const NSString) }
}
