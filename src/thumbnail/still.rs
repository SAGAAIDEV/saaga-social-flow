//! Grabbing a frame off the live camera and writing it as a JPEG.
//!
//! The preview already keeps the newest frame per slot ([`crate::ops::PreviewPort`]),
//! so capturing is a read rather than new capture plumbing: pull the latest
//! `CVImageBuffer`, hand it to Core Image, encode. The camera stays running while
//! you are on the Render tab, so the button works from there.
//!
//! Stills are content-addressed. A thumbnail generated from a still is identified
//! partly by *which* still, so the name has to change when the pixels do — a
//! timestamped name would make two different frames look interchangeable to the
//! freshness check.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use objc2_core_foundation::{CGAffineTransform, CGRect};
use objc2_core_image::{CIContext, CIImage};
use objc2_core_video::CVImageBuffer;
use objc2_foundation::NSDictionary;

/// Long edge of a stored still. The image models resample anyway, and a 4K frame
/// makes every later request slower and dearer for no gain.
const MAX_EDGE: f64 = 1600.0;

pub const STILLS_DIR: &str = "thumbnails/stills";

/// Screen grabs live apart from camera stills rather than beside them under a
/// different prefix. Both lists are read newest-first and the newest of each is
/// used, so one folder would mean a screen grab could be picked as the presenter
/// simply for being the most recent file.
pub const SCREENS_DIR: &str = "thumbnails/screens";

/// Encodes `pixels` into `{root}/thumbnails/stills/still-{hash}.jpg`.
pub fn write(root: &Path, pixels: &CVImageBuffer) -> Result<PathBuf> {
    write_into(root, STILLS_DIR, "still", pixels)
}

/// The same, for what was on screen at the moment of capture.
pub fn write_screen(root: &Path, pixels: &CVImageBuffer) -> Result<PathBuf> {
    write_into(root, SCREENS_DIR, "screen", pixels)
}

fn write_into(root: &Path, subdir: &str, prefix: &str, pixels: &CVImageBuffer) -> Result<PathBuf> {
    let jpeg = encode(pixels)?;
    let dir = root.join(subdir);
    std::fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;
    let path = dir.join(format!(
        "{prefix}-{}.jpg",
        crate::agent::prompt::hash_of_bytes(&jpeg)
    ));
    if !path.exists() {
        std::fs::write(&path, &jpeg).with_context(|| format!("writing {}", path.display()))?;
    }
    Ok(path)
}

/// Core Image does the scale and the encode in one pass.
fn encode(pixels: &CVImageBuffer) -> Result<Vec<u8>> {
    let image = unsafe { CIImage::imageWithCVImageBuffer(pixels) };
    encode_image(&image, MAX_EDGE)
}

/// Re-encodes arbitrary image bytes, scaled to `max_edge`.
///
/// Shared with the reference library: a dropped photo is re-sent to the image
/// models on every generation, so shrinking it once at ingest is the difference
/// between paying for it once and paying forever.
pub fn shrink(bytes: &[u8], max_edge: f64) -> Result<Vec<u8>> {
    let data = objc2_foundation::NSData::with_bytes(bytes);
    let image = unsafe { CIImage::imageWithData(&data) }
        .context("that file is not an image Core Image can read")?;
    encode_image(&image, max_edge)
}

/// Re-encodes a `CGImage`, scaled to `max_edge`.
///
/// The figure path. `SCScreenshotManager` answers with a `CGImage` where the
/// preview keeps `CVImageBuffer`s, and rather than teach
/// [`crate::figure::shot`] a second way to make a JPEG, it comes here — this
/// module is the one place in the crate that encodes one.
/// The pixel dimensions a JPEG declares, without decoding it.
///
/// Only the frame header is read, so this costs nothing beside a decode and can
/// stand in the path of every write. `None` for bytes that are not a JPEG at
/// all, which is a caller's answer to give rather than this one's: measuring is
/// how a claim gets checked, and nothing can be claimed about an unknown format.
pub fn jpeg_dimensions(bytes: &[u8]) -> Option<(u32, u32)> {
    if bytes.first() != Some(&0xFF) || bytes.get(1) != Some(&0xD8) {
        return None;
    }
    let mut at = 2usize;
    while at + 3 < bytes.len() {
        // Fill bytes are legal between segments, and a stray one would otherwise
        // put the length read two bytes out and walk the rest off a cliff.
        if bytes[at] != 0xFF {
            at += 1;
            continue;
        }
        let marker = bytes[at + 1];
        if marker == 0xFF {
            at += 1;
            continue;
        }
        // Standalone: no length field to skip by.
        if matches!(marker, 0xD8 | 0xD9) || (0xD0..=0xD7).contains(&marker) {
            at += 2;
            continue;
        }
        let length = u16::from_be_bytes([bytes[at + 2], bytes[at + 3]]) as usize;
        // The SOFn set carries the frame header. The three holes in the range
        // are DHT, JPG and DAC, which sit among them and are not frames.
        if (0xC0..=0xCF).contains(&marker) && !matches!(marker, 0xC4 | 0xC8 | 0xCC) {
            let height = u16::from_be_bytes([*bytes.get(at + 5)?, *bytes.get(at + 6)?]);
            let width = u16::from_be_bytes([*bytes.get(at + 7)?, *bytes.get(at + 8)?]);
            return Some((u32::from(width), u32::from(height)));
        }
        at += 2 + length.max(2);
    }
    None
}

pub fn encode_cg(image: &objc2_core_graphics::CGImage, max_edge: f64) -> Result<Vec<u8>> {
    let image = unsafe { CIImage::imageWithCGImage(image) };
    encode_image(&image, max_edge)
}

fn encode_image(image: &CIImage, max_edge: f64) -> Result<Vec<u8>> {
    let extent = unsafe { image.extent() };
    let scaled = match scale_for(extent, max_edge) {
        Some(scale) => unsafe { image.imageByApplyingTransform(scale_transform(scale)) },
        None => objc2::rc::Retained::from(image),
    };

    let context = unsafe { CIContext::context() };
    let space =
        objc2_core_graphics::CGColorSpace::new_device_rgb().context("device RGB colour space")?;
    // Default quality: the options dictionary keys live in ImageIO, and a still
    // this size does not need a tuned encoder to be a usable reference.
    let options = NSDictionary::new();
    let data =
        unsafe { context.JPEGRepresentationOfImage_colorSpace_options(&scaled, &space, &options) }
            .context("encoding the camera frame as JPEG")?;
    Ok(data.to_vec())
}

/// A uniform scale, built by hand — `CGAffineTransform` is a plain struct here.
fn scale_transform(scale: f64) -> CGAffineTransform {
    CGAffineTransform {
        a: scale,
        b: 0.0,
        c: 0.0,
        d: scale,
        tx: 0.0,
        ty: 0.0,
    }
}

/// The factor that brings the long edge down to [`MAX_EDGE`], or `None` when the
/// frame is already small enough. Never upscales.
fn scale_for(extent: CGRect, max_edge: f64) -> Option<f64> {
    let long = extent.size.width.max(extent.size.height);
    (long > max_edge).then(|| max_edge / long)
}

/// Writes an already-encoded image into the stills directory.
///
/// The path for a still that did not come from the live camera — importing one
/// from a file. No UI reaches it yet; the tests do.
#[allow(dead_code)]
pub fn write_bytes(root: &Path, jpeg: &[u8]) -> Result<PathBuf> {
    let dir = root.join(STILLS_DIR);
    std::fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;
    let path = dir.join(format!(
        "still-{}.jpg",
        crate::agent::prompt::hash_of_bytes(jpeg)
    ));
    if !path.exists() {
        std::fs::write(&path, jpeg).with_context(|| format!("writing {}", path.display()))?;
    }
    Ok(path)
}

/// Every camera still this project has captured, newest first.
pub fn list(root: &Path) -> Vec<PathBuf> {
    list_in(root, STILLS_DIR)
}

/// Every screen grab this project has captured, newest first.
pub fn list_screens(root: &Path) -> Vec<PathBuf> {
    list_in(root, SCREENS_DIR)
}

fn list_in(root: &Path, subdir: &str) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(root.join(subdir)) else {
        return Vec::new();
    };
    let mut stills: Vec<(std::time::SystemTime, PathBuf)> = entries
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "jpg"))
        .filter_map(|path| {
            let at = path.metadata().and_then(|meta| meta.modified()).ok()?;
            Some((at, path))
        })
        .collect();
    stills.sort_by_key(|(at, _)| std::cmp::Reverse(*at));
    stills.into_iter().map(|(_, path)| path).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use objc2_core_foundation::{CGPoint, CGSize};

    fn extent(width: f64, height: f64) -> CGRect {
        CGRect::new(CGPoint::new(0.0, 0.0), CGSize::new(width, height))
    }

    #[test]
    fn the_scale_transform_is_uniform_and_untranslated() {
        let matrix = scale_transform(0.5);
        assert_eq!(
            (matrix.a, matrix.d),
            (0.5, 0.5),
            "both axes, so aspect holds"
        );
        assert_eq!((matrix.b, matrix.c), (0.0, 0.0), "no shear");
        assert_eq!((matrix.tx, matrix.ty), (0.0, 0.0), "no translation");
    }

    #[test]
    fn a_large_frame_is_scaled_to_the_long_edge() {
        let scale = scale_for(extent(3840.0, 2160.0), MAX_EDGE).expect("scaled");
        assert!((3840.0 * scale - MAX_EDGE).abs() < 0.001);
        // Aspect is preserved because one factor is used for both axes.
        assert!((2160.0 * scale - MAX_EDGE * 2160.0 / 3840.0).abs() < 0.001);
    }

    #[test]
    fn a_portrait_frame_scales_on_its_own_long_edge() {
        let scale = scale_for(extent(1080.0, 1920.0), MAX_EDGE).expect("scaled");
        assert!((1920.0 * scale - MAX_EDGE).abs() < 0.001);
    }

    /// Upscaling a small frame would invent detail the model then imitates.
    #[test]
    fn a_small_frame_is_left_alone() {
        assert_eq!(scale_for(extent(1280.0, 720.0), MAX_EDGE), None);
        assert_eq!(scale_for(extent(MAX_EDGE, 900.0), MAX_EDGE), None);
    }

    #[test]
    fn identical_bytes_land_on_one_file() {
        let root = std::env::temp_dir().join(format!(
            "stream-recorder-still-{}-dedupe",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        let first = write_bytes(&root, b"pretend jpeg").unwrap();
        let again = write_bytes(&root, b"pretend jpeg").unwrap();
        assert_eq!(
            first, again,
            "content-addressed, so the same frame is one file"
        );
        let different = write_bytes(&root, b"another frame").unwrap();
        assert_ne!(first, different);
        assert_eq!(list(&root).len(), 2);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn listing_a_project_with_no_stills_is_empty() {
        let root = std::env::temp_dir().join(format!(
            "stream-recorder-still-{}-empty",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        assert!(list(&root).is_empty());
    }

    /// A JPEG header, by hand: `jpeg_dimensions` reads nothing else, so this is
    /// the whole of its input and states the layout it depends on.
    fn jpeg_header(marker: u8, width: u16, height: u16, before: &[u8]) -> Vec<u8> {
        let mut bytes = vec![0xFF, 0xD8];
        bytes.extend_from_slice(before);
        bytes.extend_from_slice(&[0xFF, marker, 0x00, 0x11, 0x08]);
        bytes.extend_from_slice(&height.to_be_bytes());
        bytes.extend_from_slice(&width.to_be_bytes());
        bytes.extend_from_slice(&[0x03, 0x01, 0x22, 0x00, 0x02, 0x11, 0x01, 0x03, 0x11, 0x01]);
        bytes
    }

    /// The manifest records what the file *is*, so this is the measurement the
    /// claim rests on. Baseline and progressive both, because the encoder is
    /// WebKit's and which one it reaches for is not this code's to assume.
    #[test]
    fn a_jpeg_declares_its_size_and_anything_else_declares_nothing() {
        // An APP0 first, so the walk is proved to skip a segment by its length
        // rather than to find the frame by luck.
        let app0 = [
            0xFF, 0xE0, 0x00, 0x10, 0x4A, 0x46, 0x49, 0x46, 0x00, 0x01, 0x01, 0x00, 0x00, 0x01,
            0x00, 0x01, 0x00, 0x00,
        ];
        for marker in [0xC0, 0xC1, 0xC2] {
            assert_eq!(
                jpeg_dimensions(&jpeg_header(marker, 1200, 630, &app0)),
                Some((1200, 630))
            );
            assert_eq!(
                jpeg_dimensions(&jpeg_header(marker, 720, 1280, &[])),
                Some((720, 1280))
            );
        }
        // 0xC4 sits inside the SOFn range and is a Huffman table, not a frame.
        assert_eq!(jpeg_dimensions(&jpeg_header(0xC4, 1200, 630, &[])), None);
        for not_a_jpeg in [&b""[..], b"og", b"\x89PNG\r\n\x1a\n", &[0xFF, 0xD8][..]] {
            assert_eq!(jpeg_dimensions(not_a_jpeg), None, "{not_a_jpeg:?}");
        }
    }
}
