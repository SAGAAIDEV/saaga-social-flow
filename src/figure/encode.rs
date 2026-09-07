//! The figure's picture: one shape, one size, one format.
//!
//! Every figure is **4:3, 1600 × 1200, lossless WebP**. The drag is locked to
//! the shape — see [`crate::figure::snip`] — and whatever it covered is
//! resampled here to the one size, so figures line up in the article column
//! at the same width and the same height rather than each choosing its own.
//! 1600 × 1200 is twice an ~800 px column, which is what holds up on a retina
//! screen; a figure is usually a screenshot of text, exactly the content that
//! turns to mush when it is scaled at the wrong step.
//!
//! Lossless WebP because of that content too. A JPEG of a terminal window rings
//! around every glyph; a lossless WebP of the same window is sharp and, for
//! flat UI, usually smaller. ImageIO cannot encode WebP at all (`sips -s format
//! webp` on macOS 15 writes nothing), so the encode goes through the `image`
//! crate.
//!
//! ## The PNG hop
//!
//! The pixels leave Core Image as a PNG and are read back by the `image` crate
//! before they are cropped, resampled and encoded. Rendering straight to a
//! bitmap would save the round trip, but Core Image's bitmap row order is a
//! convention this crate would have to get right blind; PNG is a format, and
//! a wrong guess here is a figure published upside down. The hop costs a
//! fraction of a second per figure, on ScreenCaptureKit's queue, once.
//!
//! [`crate::thumbnail::still`] keeps its own JPEG path: a still is a reference
//! image fed to a model, not a published picture, and the two have nothing to
//! agree on.

use std::io::Cursor;

use anyhow::{Context, Result};
use image::codecs::webp::WebPEncoder;
use image::{imageops, ExtendedColorType, ImageFormat, ImageReader, RgbImage};
use objc2_core_graphics::{CGColorSpace, CGImage};
use objc2_core_image::{kCIFormatRGBA8, CIContext, CIImage};
use objc2_foundation::NSDictionary;

/// Width over height. The drag is locked to it and the file is cropped to it.
pub const ASPECT: f64 = 4.0 / 3.0;
pub const WIDTH: u32 = 1600;
pub const HEIGHT: u32 = 1200;
pub const EXTENSION: &str = "webp";

/// The picture as it is published, from the image the shutter answered with.
pub fn encode_cg(image: &CGImage) -> Result<Vec<u8>> {
    let png = png_of(image)?;
    let pixels = image::load_from_memory_with_format(&png, ImageFormat::Png)
        .context("reading the screenshot back")?
        .to_rgb8();
    encode_rgb(pixels)
}

fn png_of(image: &CGImage) -> Result<Vec<u8>> {
    let image = unsafe { CIImage::imageWithCGImage(image) };
    let context = unsafe { CIContext::context() };
    let space = CGColorSpace::new_device_rgb().context("device RGB colour space")?;
    let options = NSDictionary::new();
    let data = unsafe {
        context.PNGRepresentationOfImage_format_colorSpace_options(
            &image,
            kCIFormatRGBA8,
            &space,
            &options,
        )
    }
    .context("encoding the screenshot as PNG")?;
    Ok(data.to_vec())
}

/// Crop to 4:3 about the centre, resample to the one size, encode lossless.
///
/// Split from the Core Image half so it can be tested with pixels made here.
pub fn encode_rgb(pixels: RgbImage) -> Result<Vec<u8>> {
    let framed = frame(pixels);
    let resized = imageops::resize(&framed, WIDTH, HEIGHT, imageops::FilterType::Lanczos3);
    let mut out = Vec::new();
    WebPEncoder::new_lossless(&mut out)
        .encode(resized.as_raw(), WIDTH, HEIGHT, ExtendedColorType::Rgb8)
        .context("encoding the figure as WebP")?;
    Ok(out)
}

/// Trim to exactly 4:3 about the centre.
///
/// The drag is aspect-locked, so this is normally a pixel of rounding. It
/// matters when the display's edge cut the box short of the shape — and it is
/// a crop rather than a stretch, because a resample to a different shape would
/// squash every glyph a fraction and nobody would be able to say why the
/// figure looked wrong.
fn frame(pixels: RgbImage) -> RgbImage {
    let (w, h) = pixels.dimensions();
    if w == 0 || h == 0 {
        return pixels;
    }
    let (cw, ch) = if f64::from(w) / f64::from(h) > ASPECT {
        // Too wide: keep the height, trim the sides.
        (((f64::from(h) * ASPECT).round() as u32).min(w), h)
    } else {
        // Too tall: keep the width, trim top and bottom.
        (w, ((f64::from(w) / ASPECT).round() as u32).min(h))
    };
    if (cw, ch) == (w, h) {
        return pixels;
    }
    imageops::crop_imm(&pixels, (w - cw) / 2, (h - ch) / 2, cw, ch).to_image()
}

/// The pixel size an image file declares, from its header alone.
///
/// Whatever format the bytes are in — WebP now, and the JPEGs earlier projects
/// still hold. `None` for bytes that are not an image, which is the caller's
/// answer to give: a size is a claim about a file, and nothing can be claimed
/// about an unknown one.
pub fn dimensions(bytes: &[u8]) -> Option<(u32, u32)> {
    ImageReader::new(Cursor::new(bytes))
        .with_guessed_format()
        .ok()?
        .into_dimensions()
        .ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::Rgb;

    fn gradient(w: u32, h: u32) -> RgbImage {
        RgbImage::from_fn(w, h, |x, y| Rgb([(x % 256) as u8, (y % 256) as u8, 128]))
    }

    /// Whatever was dragged, the file is one size and one format.
    #[test]
    fn every_figure_comes_out_the_same_size_as_webp() {
        for (w, h) in [(800, 600), (3000, 2250), (640, 500), (1000, 400)] {
            let bytes = encode_rgb(gradient(w, h)).expect("encodes");
            assert_eq!(&bytes[0..4], b"RIFF", "{w}x{h} is not a RIFF container");
            assert_eq!(&bytes[8..12], b"WEBP", "{w}x{h} is not WebP");
            assert_eq!(dimensions(&bytes), Some((WIDTH, HEIGHT)), "{w}x{h}");
        }
    }

    /// Off-shape input is cropped about the centre, never stretched.
    #[test]
    fn an_off_shape_picture_is_cropped_to_four_by_three_about_the_centre() {
        let wide = frame(gradient(1000, 600));
        assert_eq!(wide.dimensions(), (800, 600));
        // The centre column of the original is the centre column of the crop.
        assert_eq!(wide.get_pixel(400, 0), gradient(1000, 600).get_pixel(500, 0));

        let tall = frame(gradient(400, 600));
        assert_eq!(tall.dimensions(), (400, 300));
        assert_eq!(tall.get_pixel(0, 150), gradient(400, 600).get_pixel(0, 300));

        let already = frame(gradient(800, 600));
        assert_eq!(already.dimensions(), (800, 600));
    }

    /// Lossless means the pixels survive: a flat colour encodes and decodes to
    /// exactly itself.
    #[test]
    fn the_encoding_is_lossless() {
        let flat = RgbImage::from_pixel(1600, 1200, Rgb([17, 200, 91]));
        let bytes = encode_rgb(flat).expect("encodes");
        let back = image::load_from_memory(&bytes).expect("decodes").to_rgb8();
        assert_eq!(back.get_pixel(0, 0), &Rgb([17, 200, 91]));
        assert_eq!(back.get_pixel(1599, 1199), &Rgb([17, 200, 91]));
    }

    /// The whole path a real capture takes, on a picture whose corners are
    /// known: a `CGImage` built here goes through Core Image, the PNG hop, the
    /// crop and the resample, and has to come out the right way up and the
    /// right way round. This is the test the PNG hop exists to make passable.
    #[test]
    fn a_cgimage_comes_out_the_right_way_up_and_the_right_way_round() {
        use objc2_core_graphics::{
            CGBitmapInfo, CGColorRenderingIntent, CGDataProvider, CGImageAlphaInfo,
        };
        let (w, h) = (800usize, 600usize);
        // Red top-left quadrant, blue bottom-right, black elsewhere. RGBX, top
        // row first, which is what `NoneSkipLast` with the default byte order
        // means to Core Graphics.
        let mut rgbx = vec![0u8; w * h * 4];
        for y in 0..h {
            for x in 0..w {
                let i = (y * w + x) * 4;
                if x < w / 2 && y < h / 2 {
                    rgbx[i] = 255;
                } else if x >= w / 2 && y >= h / 2 {
                    rgbx[i + 2] = 255;
                }
                rgbx[i + 3] = 255;
            }
        }
        let space = CGColorSpace::new_device_rgb().expect("colour space");
        let provider = unsafe {
            CGDataProvider::with_data(std::ptr::null_mut(), rgbx.as_ptr().cast(), rgbx.len(), None)
        }
        .expect("data provider");
        // Default byte order is zero, so the alpha info is the whole bitmap info.
        let info = CGBitmapInfo(CGImageAlphaInfo::NoneSkipLast.0);
        let image = unsafe {
            CGImage::new(
                w,
                h,
                8,
                32,
                w * 4,
                Some(&space),
                info,
                Some(&provider),
                std::ptr::null(),
                false,
                CGColorRenderingIntent::RenderingIntentDefault,
            )
        }
        .expect("cgimage");

        let bytes = encode_cg(&image).expect("encodes");
        let back = image::load_from_memory(&bytes).expect("decodes").to_rgb8();
        assert_eq!(back.dimensions(), (WIDTH, HEIGHT));
        let top_left = back.get_pixel(WIDTH / 4, HEIGHT / 4);
        let bottom_right = back.get_pixel(WIDTH * 3 / 4, HEIGHT * 3 / 4);
        let top_right = back.get_pixel(WIDTH * 3 / 4, HEIGHT / 4);
        assert!(top_left[0] > 200 && top_left[2] < 50, "top-left is {top_left:?}, expected red");
        assert!(
            bottom_right[2] > 200 && bottom_right[0] < 50,
            "bottom-right is {bottom_right:?}, expected blue"
        );
        assert!(
            top_right[0] < 50 && top_right[2] < 50,
            "top-right is {top_right:?}, expected black — the picture was mirrored"
        );
    }

    #[test]
    fn bytes_that_are_not_an_image_have_no_size() {
        assert_eq!(dimensions(b"not a picture"), None);
        assert_eq!(dimensions(b""), None);
    }
}
