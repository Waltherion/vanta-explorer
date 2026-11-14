//! Everything the `image` crate handles: SDR formats plus scene-linear EXR and Radiance HDR.

use crate::color::{Primaries, Tf};
use crate::decode::{downscale_rgba, linearise_rgba, DecodeError, HdrImage};
use std::path::Path;

pub fn decode(path: &Path, max_dim: u32) -> Result<HdrImage, DecodeError> {
    let reader = image::ImageReader::open(path)
        .map_err(|e| DecodeError::Io(e.to_string()))?
        .with_guessed_format()
        .map_err(|e| DecodeError::Io(e.to_string()))?;
    let format = reader.format();
    let img = reader
        .decode()
        .map_err(|e| DecodeError::Decode(e.to_string()))?;

    let (w, h) = (img.width(), img.height());

    // EXR and Radiance carry SCENE-LINEAR float with values above 1.0 preserved. Treat those
    // as genuine HDR: 1.0 is scene white, which is exactly our reference. Running them through
    // an sRGB EOTF would crush the highlights that make them HDR in the first place.
    let is_linear_float = matches!(
        format,
        Some(image::ImageFormat::OpenExr) | Some(image::ImageFormat::Hdr)
    );

    let rgba: Vec<f32> = img.to_rgba32f().into_raw();
    let (rgba, w, h) = downscale_rgba(&rgba, w, h, max_dim);

    let tf = if is_linear_float {
        Tf::Linear
    } else {
        Tf::Srgb
    };
    let linear = if is_linear_float {
        rgba
    } else {
        // to_rgba32f gives 0..1 sRGB-encoded values for 8/16-bit sources.
        linearise_rgba(&rgba, Tf::Srgb, true)
    };

    Ok(HdrImage::from_linear_f32(
        w,
        h,
        &linear,
        tf,
        Primaries::Bt709,
    ))
}
