//! Live preview sinks: named latest-frame mailboxes the UI binds to.
//!
//! A preview is a graph terminal, like a file [`super::sink::OutputSpec`], but
//! it writes nothing. The capture queue publishes a retain of a *finished*
//! buffer; the main thread reads [`PreviewPort::latest`] and displays it.
//! Publishing after render returns is what makes pool buffers safe here —
//! unlike a [`super::tap::Tap`], which must never carry a buffer still being
//! drawn into.

use std::sync::{Arc, Mutex};

use objc2_core_foundation::CFRetained;
use objc2_core_media::CMTime;
use objc2_core_video::{CVImageBuffer, CVPixelBufferGetHeight, CVPixelBufferGetWidth};

use super::frame::StreamId;
use super::tap::SharedPixels;

/// What a preview sink is called, and how big its frames are.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreviewSpec {
    name: String,
    width: usize,
    height: usize,
}

impl PreviewSpec {
    pub fn new(name: &str, width: usize, height: usize) -> PreviewSpec {
        PreviewSpec {
            name: name.to_string(),
            width,
            height,
        }
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    #[allow(dead_code)]
    pub fn width(&self) -> usize {
        self.width
    }

    #[allow(dead_code)]
    pub fn height(&self) -> usize {
        self.height
    }
}

/// The most recent frame a preview sink published.
#[derive(Clone)]
pub struct PreviewFrame {
    pub pixels: SharedPixels,
    pub pts: CMTime,
    #[allow(dead_code)]
    pub stream: StreamId,
}

impl PreviewFrame {
    #[allow(dead_code)]
    pub fn size(&self) -> (usize, usize) {
        let buffer = self.pixels.get();
        (
            CVPixelBufferGetWidth(buffer),
            CVPixelBufferGetHeight(buffer),
        )
    }
}

/// A latest-frame cell the UI polls. One per preview sink.
pub struct PreviewPort {
    spec: PreviewSpec,
    latest: Mutex<Option<PreviewFrame>>,
}

impl PreviewPort {
    pub fn new(spec: PreviewSpec) -> Arc<PreviewPort> {
        Arc::new(PreviewPort {
            spec,
            latest: Mutex::new(None),
        })
    }

    pub fn spec(&self) -> &PreviewSpec {
        &self.spec
    }

    /// Publish a finished buffer as the newest preview frame.
    ///
    /// Poison-tolerant for the same reason [`super::tap::Tap::publish`] is:
    /// this runs inside an Objective-C callback.
    pub fn publish(&self, pixels: CFRetained<CVImageBuffer>, pts: CMTime, stream: StreamId) {
        let mut latest = self.latest.lock().unwrap_or_else(|e| e.into_inner());
        *latest = Some(PreviewFrame {
            pixels: SharedPixels::from_retained(pixels),
            pts,
            stream,
        });
    }

    pub fn latest(&self) -> Option<PreviewFrame> {
        self.latest
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ops::frame::test_support::pixel_buffer;
    use objc2_core_media::CMTimeFlags;
    use objc2_core_video::kCVPixelFormatType_32BGRA;

    fn time(value: i64) -> CMTime {
        CMTime {
            value,
            timescale: 1000,
            flags: CMTimeFlags::Valid,
            epoch: 0,
        }
    }

    #[test]
    fn a_preview_reads_none_before_the_first_publish() {
        let port = PreviewPort::new(PreviewSpec::new("horizontal", 1920, 1080));
        assert!(port.latest().is_none());
    }

    #[test]
    fn a_preview_keeps_the_newest_frame() {
        let port = PreviewPort::new(PreviewSpec::new("vertical", 1080, 1920));
        let first = pixel_buffer(16, 16, kCVPixelFormatType_32BGRA, 64);
        let second = pixel_buffer(32, 32, kCVPixelFormatType_32BGRA, 64);
        port.publish(first, time(1), StreamId::Camera);
        port.publish(second, time(2), StreamId::Camera);
        let latest = port.latest().expect("published");
        assert_eq!(latest.size(), (32, 32));
        let pts = latest.pts;
        let value = pts.value;
        assert_eq!(value, 2);
    }
}
