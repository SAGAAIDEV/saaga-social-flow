//! The figure's picture: the pixels you dragged over, lossless, and no bigger
//! than the column needs.
//!
//! A figure used to be one shape and one size — every drag was cropped to 4:3
//! and resampled to 1600 × 1200 so figures lined up in the article column at
//! the same width and height. That lock is gone. A terminal is wide, a dialog
//! is tall and a menu is a strip, and forcing each into the same box either cut
//! off what the figure was taken to show or padded it with screen nobody meant
//! to include. The file is now the rectangle that was dragged, at the pixels
//! the display had under it, and the ledger records each figure's own size so
//! the page lays it out in its own box — see
//! [`crate::blog::payload::FigureMedia`].
//!
//! The one thing still imposed is a ceiling. A drag over most of a 5K display
//! is a 5000-pixel-wide lossless file of many megabytes, for a column ~800
//! points wide that shows at most twice that on a retina screen. So the longer
//! edge is capped at [`MAX_EDGE`] and the picture is scaled down to fit with
//! its shape kept. Nothing is ever scaled *up*: a screenshot of text is the
//! content that turns to mush when it is resampled, and enlarging one invents
//! pixels that were never on screen.
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
//! before they are sized and encoded. Rendering straight to a bitmap would save
//! the round trip, but Core Image's bitmap row order is a convention this crate
//! would have to get right blind; PNG is a format, and a wrong guess here is a
//! figure published upside down. The hop costs a fraction of a second per
//! figure, on ScreenCaptureKit's queue, once.
//!
//! [`crate::thumbnail::still`] keeps its own JPEG path: a still is a reference
//! image fed to a model, not a published picture, and the two have nothing to
//! agree on.

use std::io::Cursor;

use anyhow::{bail, Context, Result};
use image::codecs::webp::WebPEncoder;
use image::{imageops, ExtendedColorType, ImageFormat, ImageReader, RgbImage};
use objc2_core_graphics::{CGColorSpace, CGImage};
use objc2_core_image::{kCIFormatRGBA8, CIContext, CIImage};
use objc2_foundation::NSDictionary;

/// The most pixels a figure has along its longer edge. Three times an ~800
/// point column, which is past what any display shows it at, and the point
/// where a lossless file of flat UI stops being a few hundred kilobytes.
pub const MAX_EDGE: u32 = 2400;
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

/// Bring the picture under the ceiling if it is over it, keep its shape, and
/// encode it lossless.
///
/// Split from the Core Image half so it can be tested with pixels made here.
pub fn encode_rgb(pixels: RgbImage) -> Result<Vec<u8>> {
    let (w, h) = pixels.dimensions();
    if w == 0 || h == 0 {
        bail!("the screenshot has no pixels in it");
    }
    let (pw, ph) = published_size(w, h);
    let sized = match (pw, ph) == (w, h) {
        true => pixels,
        false => imageops::resize(&pixels, pw, ph, imageops::FilterType::Lanczos3),
    };
    let mut out = Vec::new();
    WebPEncoder::new_lossless(&mut out)
        .encode(sized.as_raw(), pw, ph, ExtendedColorType::Rgb8)
        .context("encoding the figure as WebP")?;
    Ok(out)
}

/// The size a picture of `w` × `h` is published at: itself, unless its longer
/// edge is past [`MAX_EDGE`], in which case it is scaled to fit with the shape
/// kept — the shorter edge rounded, and never below a pixel.
///
/// Shared with the snip overlay's label, so what the label promises while you
/// drag is exactly what the encoder does once you let go.
pub fn published_size(w: u32, h: u32) -> (u32, u32) {
    let long = w.max(h);
    if long <= MAX_EDGE {
        return (w, h);
    }
    let scale = f64::from(MAX_EDGE) / f64::from(long);
    let scaled = |edge: u32| ((f64::from(edge) * scale).round() as u32).max(1);
    match w >= h {
        true => (MAX_EDGE, scaled(h)),
        false => (scaled(w), MAX_EDGE),
    }
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

    /// Whatever shape was dragged is the shape of the file, at the pixels that
    /// were under it. The one thing every figure shares is the format.
    #[test]
    fn a_figure_keeps_the_size_and_shape_it_was_dragged_at() {
        for (w, h) in [
            (800, 600),
            (640, 500),
            (1000, 400),
            (300, 900),
            (2400, 1350),
        ] {
            let bytes = encode_rgb(gradient(w, h)).expect("encodes");
            assert_eq!(&bytes[0..4], b"RIFF", "{w}x{h} is not a RIFF container");
            assert_eq!(&bytes[8..12], b"WEBP", "{w}x{h} is not WebP");
            assert_eq!(dimensions(&bytes), Some((w, h)), "{w}x{h} was resized");
        }
    }

    /// Past the ceiling the picture comes down to it — on whichever edge is the
    /// long one — with the shape kept, so a wide figure stays wide.
    #[test]
    fn a_figure_past_the_ceiling_is_scaled_down_with_its_shape_kept() {
        assert_eq!(published_size(4800, 3000), (2400, 1500));
        assert_eq!(published_size(1500, 4800), (750, 2400));
        assert_eq!(published_size(3000, 3000), (2400, 2400));
        // A hairline strip does not round away to nothing.
        assert_eq!(published_size(9600, 1), (2400, 1));

        let bytes = encode_rgb(gradient(4800, 3000)).expect("encodes");
        assert_eq!(dimensions(&bytes), Some((2400, 1500)));
    }

    /// A small drag is a small figure. Enlarging a screenshot of text invents
    /// pixels that were never on screen, so the encoder never does it.
    #[test]
    fn nothing_is_ever_scaled_up() {
        assert_eq!(published_size(320, 200), (320, 200));
        assert_eq!(published_size(MAX_EDGE, 10), (MAX_EDGE, 10));
        let bytes = encode_rgb(gradient(320, 200)).expect("encodes");
        assert_eq!(dimensions(&bytes), Some((320, 200)));
    }

    /// A picture with nothing in it is refused rather than written as a file
    /// nothing can open.
    #[test]
    fn an_empty_picture_is_refused() {
        assert!(encode_rgb(RgbImage::new(0, 10)).is_err());
        assert!(encode_rgb(RgbImage::new(10, 0)).is_err());
    }

    /// Lossless means the pixels survive: a flat colour encodes and decodes to
    /// exactly itself.
    #[test]
    fn the_encoding_is_lossless() {
        let flat = RgbImage::from_pixel(640, 480, Rgb([17, 200, 91]));
        let bytes = encode_rgb(flat).expect("encodes");
        let back = image::load_from_memory(&bytes).expect("decodes").to_rgb8();
        assert_eq!(back.get_pixel(0, 0), &Rgb([17, 200, 91]));
        assert_eq!(back.get_pixel(639, 479), &Rgb([17, 200, 91]));
    }

    /// The whole path a real capture takes, on a picture whose corners are
    /// known: a `CGImage` built here goes through Core Image, the PNG hop and
    /// the encode, and has to come out the right way up, the right way round,
    /// and at its own size. This is the test the PNG hop exists to make passable.
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
        let (w, h) = (w as u32, h as u32);
        assert_eq!(back.dimensions(), (w, h), "the size it was dragged at");
        let top_left = back.get_pixel(w / 4, h / 4);
        let bottom_right = back.get_pixel(w * 3 / 4, h * 3 / 4);
        let top_right = back.get_pixel(w * 3 / 4, h / 4);
        assert!(
            top_left[0] > 200 && top_left[2] < 50,
            "top-left is {top_left:?}, expected red"
        );
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
