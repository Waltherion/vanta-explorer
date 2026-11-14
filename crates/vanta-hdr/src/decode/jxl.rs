//! JPEG XL via the system libjxl (jpegxl-rs links it with pkg-config).

use crate::color::{tf_from_icc, Primaries, Tf};
use crate::decode::{downscale_rgba, linearise_rgba, DecodeError, HdrImage};
use std::path::Path;

pub fn decode(path: &Path, max_dim: u32) -> Result<HdrImage, DecodeError> {
    let bytes = std::fs::read(path).map_err(|e| DecodeError::Io(e.to_string()))?;

    // All cores: single-threaded, a 4K picture takes about four times as long.
    let runner = jpegxl_rs::ThreadsRunner::default();
    let decoder = jpegxl_rs::decoder_builder()
        .parallel_runner(&runner)
        // Without asking for it, `icc_profile` is always empty and the fallback below never runs.
        .icc_profile(true)
        .build()
        .map_err(|e| DecodeError::Decode(e.to_string()))?;
    let (meta, pixels) = decoder
        .decode_with::<f32>(&bytes)
        .map_err(|e| DecodeError::Decode(e.to_string()))?;

    let (w, h) = (meta.width, meta.height);

    // A JXL declares its colour with named values in the codestream, with an embedded ICC
    // profile, or both. The named values win when they say anything beyond plain sRGB: libjxl
    // will happily synthesise an sRGB ICC profile for a PQ image, and letting that profile
    // decide turns an HDR file into an SDR one.
    let named = named_colour(&bytes);
    let (mut tf, mut primaries) = named.unwrap_or((Tf::Srgb, Primaries::Bt709));
    if tf == Tf::Srgb {
        if let Some(icc) = meta.icc_profile.as_ref() {
            if let Some((itf, iprim)) = tf_from_icc(icc) {
                tf = itf;
                if let Some(p) = iprim {
                    primaries = p;
                }
            }
        }
    }

    // Normalise to interleaved RGBA regardless of channel count.
    let rgba = to_rgba(&pixels, w as usize, h as usize)?;
    let (rgba, w, h) = downscale_rgba(&rgba, w, h, max_dim);
    let linear = linearise_rgba(&rgba, tf, true);
    Ok(HdrImage::from_linear_f32(w, h, &linear, tf, primaries))
}

/// Reads the codestream's named colour encoding, stopping as soon as it arrives: this reparses
/// the header only, never the pixels.
fn named_colour(bytes: &[u8]) -> Option<(Tf, Primaries)> {
    use jpegxl_sys::color::color_encoding::{JxlColorEncoding, JxlPrimaries, JxlTransferFunction};
    use jpegxl_sys::decode as jd;

    unsafe {
        let dec = jd::JxlDecoderCreate(std::ptr::null());
        if dec.is_null() {
            return None;
        }
        let out = (|| {
            if jd::JxlDecoderSubscribeEvents(dec, jd::JxlDecoderStatus::ColorEncoding as i32) != jd::JxlDecoderStatus::Success {
                return None;
            }
            if jd::JxlDecoderSetInput(dec, bytes.as_ptr(), bytes.len()) != jd::JxlDecoderStatus::Success {
                return None;
            }
            if jd::JxlDecoderProcessInput(dec) != jd::JxlDecoderStatus::ColorEncoding {
                return None;
            }
            let mut c = std::mem::MaybeUninit::<JxlColorEncoding>::uninit();
            if jd::JxlDecoderGetColorAsEncodedProfile(dec, jd::JxlColorProfileTarget::Data, c.as_mut_ptr())
                != jd::JxlDecoderStatus::Success
            {
                return None;
            }
            let c = c.assume_init();
            let tf = match c.transfer_function {
                JxlTransferFunction::PQ => Tf::Pq,
                JxlTransferFunction::HLG => Tf::Hlg,
                JxlTransferFunction::Linear => Tf::Linear,
                _ => Tf::Srgb,
            };
            let prim = match c.primaries {
                JxlPrimaries::Rec2100 => Primaries::Bt2020,
                JxlPrimaries::P3 => Primaries::DisplayP3,
                _ => Primaries::Bt709,
            };
            Some((tf, prim))
        })();
        jd::JxlDecoderDestroy(dec);
        out
    }
}

fn to_rgba(pixels: &[f32], w: usize, h: usize) -> Result<Vec<f32>, DecodeError> {
    let px = w * h;
    match pixels.len() / px.max(1) {
        4 => Ok(pixels.to_vec()),
        3 => Ok((0..px)
            .flat_map(|i| [pixels[i * 3], pixels[i * 3 + 1], pixels[i * 3 + 2], 1.0])
            .collect()),
        1 => Ok((0..px)
            .flat_map(|i| [pixels[i], pixels[i], pixels[i], 1.0])
            .collect()),
        2 => Ok((0..px)
            .flat_map(|i| {
                [
                    pixels[i * 2],
                    pixels[i * 2],
                    pixels[i * 2],
                    pixels[i * 2 + 1],
                ]
            })
            .collect()),
        n => Err(DecodeError::Unsupported(format!("{n} channels"))),
    }
}
