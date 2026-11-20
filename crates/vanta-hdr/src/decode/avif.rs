//! AVIF via the system libavif (bindgen; see build.rs).
//!
//! Colour handling order matters:
//! read CICP/nclx first, then let an ICC profile OVERRIDE it, because real HDR exports set
//! CICP = unspecified and describe the space only in ICC.

use crate::color::{tf_from_icc, Primaries, Tf};
use crate::decode::{linearise_rgba, DecodeError, HdrImage};
use image::metadata::Orientation;
use rayon::prelude::*;
use std::path::Path;

use crate::avif_sys as sys;

/// RAII wrapper so every early return frees the decoder.
struct Decoder(*mut sys::avifDecoder);

impl Drop for Decoder {
    fn drop(&mut self) {
        unsafe { sys::avifDecoderDestroy(self.0) };
    }
}

pub fn decode(path: &Path, max_dim: u32) -> Result<HdrImage, DecodeError> {
    let c_path = std::ffi::CString::new(path.as_os_str().as_encoded_bytes())
        .map_err(|e| DecodeError::Io(e.to_string()))?;

    unsafe {
        let dec = sys::avifDecoderCreate();
        if dec.is_null() {
            return Err(DecodeError::Decode("avifDecoderCreate failed".into()));
        }
        let dec = Decoder(dec);
        // libavif decodes on one thread unless told otherwise: about three times slower at 4K.
        let threads = std::thread::available_parallelism().map_or(1, |n| n.get()) as i32;
        (*dec.0).maxThreads = threads;

        let r = sys::avifDecoderSetIOFile(dec.0, c_path.as_ptr());
        if r != sys::avifResult_AVIF_RESULT_OK {
            return Err(DecodeError::Io(result_str(r)));
        }
        let r = sys::avifDecoderParse(dec.0);
        if r != sys::avifResult_AVIF_RESULT_OK {
            return Err(DecodeError::Decode(result_str(r)));
        }
        let r = sys::avifDecoderNextImage(dec.0);
        if r != sys::avifResult_AVIF_RESULT_OK {
            return Err(DecodeError::Decode(result_str(r)));
        }

        let image = (*dec.0).image;
        if image.is_null() {
            return Err(DecodeError::Decode("no image".into()));
        }

        // --- colour space: CICP first ---
        // bindgen emits these as plain consts, not enum variants, so they cannot be match
        // patterns, so compare instead.
        let cicp_tf = (*image).transferCharacteristics as u32;
        let mut tf = if cicp_tf == sys::AVIF_TRANSFER_CHARACTERISTICS_SMPTE2084 {
            Tf::Pq
        } else if cicp_tf == sys::AVIF_TRANSFER_CHARACTERISTICS_HLG {
            Tf::Hlg
        } else if cicp_tf == sys::AVIF_TRANSFER_CHARACTERISTICS_LINEAR {
            Tf::Linear
        } else {
            Tf::Srgb
        };
        let cicp_prim = (*image).colorPrimaries as u32;
        let mut primaries = if cicp_prim == sys::AVIF_COLOR_PRIMARIES_BT2020 {
            Primaries::Bt2020
        } else if cicp_prim == sys::AVIF_COLOR_PRIMARIES_SMPTE432
            || cicp_prim == sys::AVIF_COLOR_PRIMARIES_SMPTE431
        {
            Primaries::DisplayP3
        } else {
            Primaries::Bt709
        };

        // --- then ICC, which wins: Lightroom writes CICP=unspecified + an ICC profile ---
        let icc = (*image).icc;
        if !icc.data.is_null() && icc.size > 0 {
            let bytes = std::slice::from_raw_parts(icc.data, icc.size);
            if let Some((itf, iprim)) = tf_from_icc(bytes) {
                if itf != Tf::Srgb {
                    tf = itf;
                }
                if let Some(p) = iprim {
                    primaries = p;
                }
            }
        }

        // --- orientation: libavif reports irot/imir but never applies them to the pixels ---
        let flags = (*image).transformFlags;
        let angle = (flags & sys::avifTransformFlag_AVIF_TRANSFORM_IROT != 0).then_some((*image).irot.angle);
        let axis = (flags & sys::avifTransformFlag_AVIF_TRANSFORM_IMIR != 0).then_some((*image).imir.axis);
        let orientation = exif_orientation(angle, axis);

        // --- downscale BEFORE linearising, as vantaviewer does ---
        let (mut w, mut h) = ((*image).width, (*image).height);
        if max_dim > 0 && (w > max_dim || h > max_dim) {
            let scale = max_dim as f32 / w.max(h) as f32;
            let nw = ((w as f32 * scale).round() as u32).max(1);
            let nh = ((h as f32 * scale).round() as u32).max(1);
            if sys::avifImageScale(image, nw, nh, &mut (*dec.0).diag)
                == sys::avifResult_AVIF_RESULT_OK
            {
                w = nw;
                h = nh;
            }
        }

        // --- YUV -> float RGBA, letting libavif do the (correct) conversion ---
        let mut rgb = sys::avifRGBImage::default();
        sys::avifRGBImageSetDefaults(&mut rgb, image);
        rgb.format = sys::avifRGBFormat_AVIF_RGB_FORMAT_RGBA;
        // 16-bit integers at full size, every code of a 10- or 12-bit source kept (half-float
        // cannot hold all 12-bit codes above 0.5); scaled pictures take the float way below.
        let full = w == (*image).width && h == (*image).height;
        rgb.depth = 16;
        rgb.isFloat = if full { 0 } else { 1 };
        rgb.maxThreads = threads;
        let r = sys::avifRGBImageAllocatePixels(&mut rgb);
        if r != sys::avifResult_AVIF_RESULT_OK {
            return Err(DecodeError::Decode(result_str(r)));
        }
        let r = sys::avifImageYUVToRGB(image, &mut rgb);
        if r != sys::avifResult_AVIF_RESULT_OK {
            sys::avifRGBImageFreePixels(&mut rgb);
            return Err(DecodeError::Decode(result_str(r)));
        }

        let count = (w as usize) * (h as usize) * 4;
        if full && rgb.rowBytes as usize == w as usize * 8 {
            let codes = std::slice::from_raw_parts(rgb.pixels as *const u16, count);
            let img = crate::decode::from_codes_u16(w, h, codes, tf, primaries);
            sys::avifRGBImageFreePixels(&mut rgb);
            return Ok(crate::decode::orient(img, orientation));
        }
        if full {
            // Padded rows (libavif has not been seen to pad): the codes are gathered row by row.
            let mut codes = Vec::with_capacity(count);
            for y in 0..h as usize {
                let row = std::slice::from_raw_parts((rgb.pixels as *const u8).add(y * rgb.rowBytes as usize) as *const u16, w as usize * 4);
                codes.extend_from_slice(row);
            }
            sys::avifRGBImageFreePixels(&mut rgb);
            return Ok(crate::decode::orient(crate::decode::from_codes_u16(w, h, &codes, tf, primaries), orientation));
        }
        let halfs = std::slice::from_raw_parts(rgb.pixels as *const half::f16, count);
        let encoded: Vec<f32> = halfs.par_iter().map(|v| v.to_f32()).collect();
        sys::avifRGBImageFreePixels(&mut rgb);

        let linear = linearise_rgba(&encoded, tf, true);
        Ok(crate::decode::orient(HdrImage::from_linear_f32(w, h, &linear, tf, primaries), orientation))
    }
}

/// The EXIF-style orientation for a HEIF `irot` angle (quarter turns anticlockwise) and `imir`
/// axis (0 swaps top and bottom, 1 swaps left and right). MIAF fixes the order as rotation
/// first, then mirror; the `image` crate's combined variants likewise rotate clockwise first
/// and flip horizontally after, and a vertical flip is a horizontal one plus a half turn.
fn exif_orientation(angle: Option<u8>, axis: Option<u8>) -> Orientation {
    // Clockwise quarter turns, which is how the `image` crate counts.
    let cw = (4 - angle.unwrap_or(0) % 4) % 4;
    let (cw, flip) = match axis {
        None => (cw, false),
        Some(1) => (cw, true),
        Some(_) => ((cw + 2) % 4, true),
    };
    match (cw, flip) {
        (0, false) => Orientation::NoTransforms,
        (1, false) => Orientation::Rotate90,
        (2, false) => Orientation::Rotate180,
        (3, false) => Orientation::Rotate270,
        (0, true) => Orientation::FlipHorizontal,
        (1, true) => Orientation::Rotate90FlipH,
        (2, true) => Orientation::FlipVertical,
        _ => Orientation::Rotate270FlipH,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // libavif's own Exif-to-irot/imir table (avifImageSetMetadataExif) is the reference here.
    #[test]
    fn irot_imir_match_exif() {
        assert_eq!(exif_orientation(None, None), Orientation::NoTransforms);
        assert_eq!(exif_orientation(None, Some(1)), Orientation::FlipHorizontal); // Exif 2
        assert_eq!(exif_orientation(Some(2), None), Orientation::Rotate180); // Exif 3
        assert_eq!(exif_orientation(None, Some(0)), Orientation::FlipVertical); // Exif 4
        assert_eq!(exif_orientation(Some(1), Some(0)), Orientation::Rotate90FlipH); // Exif 5
        assert_eq!(exif_orientation(Some(3), None), Orientation::Rotate90); // Exif 6
        assert_eq!(exif_orientation(Some(3), Some(0)), Orientation::Rotate270FlipH); // Exif 7
        assert_eq!(exif_orientation(Some(1), None), Orientation::Rotate270); // Exif 8
    }
}

fn result_str(r: sys::avifResult) -> String {
    unsafe {
        let p = sys::avifResultToString(r);
        if p.is_null() {
            return format!("avif error {r}");
        }
        std::ffi::CStr::from_ptr(p).to_string_lossy().into_owned()
    }
}
