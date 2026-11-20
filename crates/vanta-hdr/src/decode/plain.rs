//! Everything the `image` crate handles: SDR formats plus scene-linear EXR and Radiance HDR.

use crate::color::{Primaries, Tf};
use crate::decode::{linearise_rgba, DecodeError, HdrImage};
use std::path::Path;

pub fn decode(path: &Path, max_dim: u32) -> Result<HdrImage, DecodeError> {
    let reader = image::ImageReader::open(path)
        .map_err(|e| DecodeError::Io(e.to_string()))?
        .with_guessed_format()
        .map_err(|e| DecodeError::Io(e.to_string()))?;
    let format = reader.format();
    // Upright first, so the thumbnail size below is worked out from the rotated dimensions.
    let (img, icc) = crate::decode::decode_oriented_with_profile(reader)?;

    // A thumbnail is scaled in the image's own type (8 bits for a JPEG) before it becomes
    // float, which is four times larger: a 50-megapixel photo is 800 MB as float RGBA.
    let img = match crate::decode::scaled_size(img.width(), img.height(), max_dim) {
        (w, h) if (w, h) != (img.width(), img.height()) => img.thumbnail_exact(w, h),
        _ => img,
    };
    let (w, h) = (img.width(), img.height());

    // EXR and Radiance carry SCENE-LINEAR float with values above 1.0 preserved. Treat those
    // as genuine HDR: 1.0 is scene white, which is exactly our reference. Running them through
    // an sRGB EOTF would crush the highlights that make them HDR in the first place.
    let is_linear_float = matches!(
        format,
        Some(image::ImageFormat::OpenExr) | Some(image::ImageFormat::Hdr)
    );

    let mut rgba: Vec<f32> = img.to_rgba32f().into_raw();
    if is_linear_float {
        return Ok(HdrImage::from_linear_f32(w, h, &rgba, Tf::Linear, Primaries::Bt709));
    }

    // to_rgba32f gives the file's own 0..1 codes for 8/16-bit sources. Without a profile those
    // are sRGB. A profile the pipeline knows by name (Display P3 above all, which every recent
    // phone and Mac writes) keeps the codes and says the primaries; any other is used to convert
    // the pixels to linear BT.2020, so an Adobe RGB or ProPhoto picture is not shown washed out.
    use crate::color::Embedded;
    let embedded = icc.as_deref().map_or(Embedded::Srgb, crate::color::embedded);
    let (linear, tf, primaries) = match embedded {
        Embedded::Known(tf, p) => (linearise_rgba(&rgba, tf, true), tf, p),
        Embedded::Other if crate::color::convert_to_linear_bt2020(icc.as_deref().unwrap_or_default(), &mut rgba) => {
            // Still an SDR picture: only its gamut was wider than sRGB.
            (rgba, Tf::Srgb, Primaries::Bt2020)
        }
        _ => (linearise_rgba(&rgba, Tf::Srgb, true), Tf::Srgb, Primaries::Bt709),
    };
    Ok(HdrImage::from_linear_f32(w, h, &linear, tf, primaries))
}
