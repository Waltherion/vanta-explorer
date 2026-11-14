//! PNG, including 16-bit HDR PNG carrying a cICP chunk.
//!
//! The `image` crate ignores cICP entirely, so we parse the chunk ourselves before decoding.
//! HDR stills written by vanta-hdr's own encoder declare themselves PQ this way, so without it
//! they would be shown washed out.

use crate::color::{tf_from_icc, Primaries, Tf};
use crate::decode::{downscale_rgba, linearise_rgba, DecodeError, HdrImage};
use std::path::Path;

pub fn decode(path: &Path, max_dim: u32) -> Result<HdrImage, DecodeError> {
    let bytes = std::fs::read(path).map_err(|e| DecodeError::Io(e.to_string()))?;

    let (mut tf, mut primaries) = (Tf::Srgb, Primaries::Bt709);
    if let Some((ctf, cprim)) = read_cicp(&bytes) {
        tf = ctf;
        primaries = cprim;
    } else if let Some(icc) = read_iccp(&bytes) {
        if let Some((itf, iprim)) = tf_from_icc(&icc) {
            tf = itf;
            if let Some(p) = iprim {
                primaries = p;
            }
        }
    }

    let img = image::load_from_memory_with_format(&bytes, image::ImageFormat::Png)
        .map_err(|e| DecodeError::Decode(e.to_string()))?;
    let (w, h) = (img.width(), img.height());
    if max_dim == 0 || (w <= max_dim && h <= max_dim) {
        // Full size: 16-bit codes (an 8-bit file scaled up exactly) through one table.
        let codes = match img {
            image::DynamicImage::ImageRgba16(b) => b.into_raw(),
            other => other.to_rgba16().into_raw(),
        };
        return Ok(crate::decode::from_codes_u16(w, h, &codes, tf, primaries));
    }
    let rgba: Vec<f32> = img.to_rgba32f().into_raw();

    let (rgba, w, h) = downscale_rgba(&rgba, w, h, max_dim);
    let linear = linearise_rgba(&rgba, tf, true);
    Ok(HdrImage::from_linear_f32(w, h, &linear, tf, primaries))
}

/// Walk the chunk list for cICP. Stops at IDAT/IEND: anything after is pixel data.
fn read_cicp(bytes: &[u8]) -> Option<(Tf, Primaries)> {
    let payload = find_chunk(bytes, b"cICP")?;
    if payload.len() < 4 {
        return None;
    }
    // [primaries, transfer, matrix, full-range]
    let tf = match payload[1] {
        16 => Tf::Pq,
        18 => Tf::Hlg,
        8 => Tf::Linear,
        _ => Tf::Srgb,
    };
    let prim = match payload[0] {
        9 => Primaries::Bt2020,
        11 | 12 => Primaries::DisplayP3,
        _ => Primaries::Bt709,
    };
    Some((tf, prim))
}

fn read_iccp(bytes: &[u8]) -> Option<Vec<u8>> {
    let payload = find_chunk(bytes, b"iCCP")?;
    // name\0 compression-method compressed-profile
    let nul = payload.iter().position(|b| *b == 0)?;
    let data = payload.get(nul + 2..)?;
    let mut out = Vec::new();
    let mut dec = flate2::read::ZlibDecoder::new(data);
    std::io::Read::read_to_end(&mut dec, &mut out).ok()?;
    Some(out)
}

fn find_chunk<'a>(bytes: &'a [u8], want: &[u8; 4]) -> Option<&'a [u8]> {
    if bytes.len() < 8 || &bytes[..8] != b"\x89PNG\r\n\x1a\n" {
        return None;
    }
    let mut pos = 8;
    while pos + 8 <= bytes.len() {
        let len = u32::from_be_bytes(bytes[pos..pos + 4].try_into().ok()?) as usize;
        let kind = &bytes[pos + 4..pos + 8];
        if kind == b"IDAT" || kind == b"IEND" {
            return None;
        }
        let start = pos + 8;
        let end = start.checked_add(len)?;
        if end > bytes.len() {
            return None;
        }
        if kind == want {
            return Some(&bytes[start..end]);
        }
        pos = end + 4; // skip CRC
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_non_png() {
        assert!(find_chunk(b"not a png at all", b"cICP").is_none());
    }

    #[test]
    fn finds_cicp_and_maps_pq_bt2020() {
        // Minimal PNG header + a cICP chunk declaring BT.2020 / PQ.
        let mut b = b"\x89PNG\r\n\x1a\n".to_vec();
        b.extend_from_slice(&4u32.to_be_bytes());
        b.extend_from_slice(b"cICP");
        b.extend_from_slice(&[9, 16, 0, 1]);
        b.extend_from_slice(&[0, 0, 0, 0]); // CRC (unchecked)
        assert_eq!(read_cicp(&b), Some((Tf::Pq, Primaries::Bt2020)));
    }

    #[test]
    fn stops_at_idat() {
        let mut b = b"\x89PNG\r\n\x1a\n".to_vec();
        b.extend_from_slice(&0u32.to_be_bytes());
        b.extend_from_slice(b"IDAT");
        b.extend_from_slice(&[0, 0, 0, 0]);
        assert!(read_cicp(&b).is_none());
    }
}
