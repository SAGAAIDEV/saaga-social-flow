//! `AVAssetWriter`/`AVAssetWriterInput` wrapper for media output files.

use std::path::Path;
use std::sync::mpsc;
use std::time::Duration;

use anyhow::{anyhow, bail, Context, Result};
use block2::RcBlock;
use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2_av_foundation::{
    AVAssetWriter, AVAssetWriterInput, AVAssetWriterStatus, AVFileType, AVMediaType,
};
use objc2_foundation::{NSDictionary, NSString, NSURL};

pub struct MediaFileWriter {
    writer: Retained<AVAssetWriter>,
    pub input: Retained<AVAssetWriterInput>,
}

impl MediaFileWriter {
    /// Create a media file writer with explicit file type and media type.
    /// `settings` should come from the corresponding
    /// `AVCapture*DataOutput::recommendedSettingsForAssetWriterWithOutputFileType`
    /// call — no need to hand-construct settings dictionaries.
    pub fn create(
        path: &Path,
        file_type: &AVFileType,
        media_type: &AVMediaType,
        settings: Option<Retained<NSDictionary<NSString, AnyObject>>>,
    ) -> Result<MediaFileWriter> {
        let url_string = NSString::from_str(&path.to_string_lossy());
        let url = NSURL::fileURLWithPath(&url_string);

        let writer = unsafe { AVAssetWriter::assetWriterWithURL_fileType_error(&url, file_type) }
            .map_err(|e| anyhow!("could not create asset writer: {e:?}"))?;

        let input = unsafe {
            AVAssetWriterInput::assetWriterInputWithMediaType_outputSettings(
                media_type,
                settings.as_deref(),
            )
        };
        unsafe { input.setExpectsMediaDataInRealTime(true) };

        unsafe {
            if !writer.canAddInput(&input) {
                bail!("writer refused the input");
            }
            writer.addInput(&input);
            if !writer.startWriting() {
                bail!("startWriting failed: {:?}", writer.error());
            }
        }

        Ok(MediaFileWriter { writer, input })
    }

    pub fn writer(&self) -> Retained<AVAssetWriter> {
        self.writer.clone()
    }

    /// Mark the input finished and wait for the writer to flush the file to
    /// disk. `finishWritingWithCompletionHandler:` is async, so this blocks
    /// on a channel the completion block fires into.
    pub fn finish(&self) -> Result<()> {
        unsafe { self.input.markAsFinished() };

        let (tx, rx) = mpsc::channel::<()>();
        let handler = RcBlock::new(move || {
            let _ = tx.send(());
        });
        unsafe { self.writer.finishWritingWithCompletionHandler(&handler) };
        rx.recv_timeout(Duration::from_secs(30))
            .context("timed out waiting for the asset writer to finish")?;

        let status = unsafe { self.writer.status() };
        if status == AVAssetWriterStatus::Completed {
            Ok(())
        } else {
            bail!(
                "asset writer finished with status {status:?}: {:?}",
                unsafe { self.writer.error() }
            )
        }
    }
}

// Backwards-compatibility alias for the mic pipeline's existing naming
pub type AudioFileWriter = MediaFileWriter;
