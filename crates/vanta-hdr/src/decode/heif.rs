//! HEIC/HEIF via the system libheif. Mostly iPhone photos, which are Display-P3.

use crate::color::{tf_from_icc, Primaries, Tf};
use crate::decode::{linearise_rgba, sample_rgba, DecodeError, HdrImage};
use libheif_rs::{ColorSpace, HeifContext, LibHeif, RgbChroma};
use std::path::Path;

pub fn decode(path: &Path, max_dim: u32) -> Result<HdrImage, DecodeError> {
    let lib = LibHeif::new();
    let ctx = HeifContext::read_from_file(&path.to_string_lossy())
        .map_err(|e| DecodeError::Decode(e.to_string()))?;
    let handle = ctx
        .primary_image_handle()
        .map_err(|e| DecodeError::Decode(e.to_string()))?;

    // Ask for 16-bit interleaved so >8-bit HDR survives the trip.
    let img = lib
        .decode(&handle, ColorSpace::Rgb(RgbChroma::HdrRgbaBe), None)
        .or_else(|_| lib.decode(&handle, ColorSpace::Rgb(RgbChroma::Rgba), None))
        .map_err(|e| DecodeError::Decode(e.to_string()))?;

    let planes = img.planes();
    let plane = planes
        .interleaved
        .ok_or_else(|| DecodeError::Decode("no interleaved plane".into()))?;
    let (w, h) = (plane.width, plane.height);
    let wide = plane.storage_bits_per_pixel > 32; // 32 for 8-bit RGBA, 64 for 16-bit RGBA

    // A 10-bit image comes back as 10-bit values sitting in 16-bit words, NOT stretched to fill
    // them, so the divisor is the range of the samples actually returned (the plane's own bit
    // depth). Dividing by 65535 instead leaves the picture 64 times too dark, which PQ then
    // crushes to what looks like pure black; and the source's luma depth is wrong for an 8-bit
    // photo, which libheif hands back as 10-bit samples: divided by 511 it came out twice as
    // bright and clipped.
    let depth = if wide { (plane.bits_per_pixel as u32).clamp(8, 16) } else { 8 };
    let max = ((1u32 << depth) - 1) as f32;
    // Read at the size wanted, straight from libheif's buffer.
    let (data, stride) = (plane.data, plane.stride);
    let (rgba, w, h) = sample_rgba(w, h, max_dim, |x, y| {
        let row = &data[y * stride..];
        std::array::from_fn(|c| {
            if wide {
                // Big-endian 16-bit, per HdrRgbaBe.
                let i = (x * 4 + c) * 2;
                (u16::from_be_bytes([row[i], row[i + 1]]) as f32) / max
            } else {
                (row[x * 4 + c] as f32) / max
            }
        })
    });

    let (mut tf, mut primaries) = (Tf::Srgb, Primaries::Bt709);
    // NCLX first, then ICC wins, the same order as the AVIF path.
    if let Some(nclx) = handle.color_profile_nclx() {
        use libheif_rs::{ColorPrimaries, TransferCharacteristics};
        tf = match nclx.transfer_characteristics() {
            TransferCharacteristics::ITU_R_BT_2100_0_PQ => Tf::Pq,
            TransferCharacteristics::ITU_R_BT_2100_0_HLG => Tf::Hlg,
            TransferCharacteristics::Linear => Tf::Linear,
            _ => Tf::Srgb,
        };
        primaries = match nclx.color_primaries() {
            ColorPrimaries::ITU_R_BT_2020_2_and_2100_0 => Primaries::Bt2020,
            ColorPrimaries::SMPTE_RP_431_2 | ColorPrimaries::SMPTE_EG_432_1 => Primaries::DisplayP3,
            _ => Primaries::Bt709,
        };
    }
    if let Some(raw) = handle.color_profile_raw() {
        if let Some((itf, iprim)) = tf_from_icc(&raw.data) {
            if itf != Tf::Srgb {
                tf = itf;
            }
            if let Some(p) = iprim {
                primaries = p;
            }
        }
    }

    let linear = linearise_rgba(&rgba, tf, true);
    Ok(HdrImage::from_linear_f32(w, h, &linear, tf, primaries))
}
