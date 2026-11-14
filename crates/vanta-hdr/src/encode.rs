//! Writing an [`HdrImage`] back out, preserving what makes it HDR.
//!
//! The convention coming in is the pipeline's: fp16 linear, 1.0 = 203 cd/m^2, in the image's
//! NATIVE primaries. What each format does with that differs:
//!
//! - Formats that can declare their own colour space (AVIF, PNG via cICP, JXL, HEIC) keep the
//!   native primaries and say so in the file, so nothing is thrown away.
//! - Formats that cannot (JPEG, WebP, TIFF, BMP, and plain SDR PNG) are sRGB by definition, so
//!   wider primaries are converted to BT.709 on the way out.
//!
//! HDR is encoded as PQ: 10-bit for AVIF and HEIC, 16-bit for PNG and JXL.

use std::path::Path;

use half::f16;

use crate::color::{srgb_oetf, to_bt709, Primaries};
use crate::decode::HdrImage;

#[derive(Debug)]
pub enum EncodeError {
    /// The extension is known but cannot hold this image (HDR into an SDR-only format).
    NotHdrCapable(String),
    Unsupported(String),
    Empty,
    Io(std::io::Error),
    Backend(String),
}

impl std::fmt::Display for EncodeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            EncodeError::NotHdrCapable(e) => write!(f, "{} can't hold HDR: save as AVIF, PNG or JXL", e.to_uppercase()),
            EncodeError::Unsupported(e) => write!(f, "encoding to .{e} is not supported yet"),
            EncodeError::Empty => write!(f, "crop/rotate produced an empty image"),
            EncodeError::Io(e) => write!(f, "{e}"),
            EncodeError::Backend(m) => write!(f, "{m}"),
        }
    }
}

impl std::error::Error for EncodeError {}

/// A rectangle in image pixels. An empty one means the whole image.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Crop {
    pub x: u32,
    pub y: u32,
    pub w: u32,
    pub h: u32,
}

impl Crop {
    pub fn is_empty(&self) -> bool {
        self.w == 0 || self.h == 0
    }
}

/// Can this extension be written at all, and can it carry HDR?
pub fn can_encode_extension(ext: &str, hdr: bool) -> bool {
    match ext.to_lowercase().as_str() {
        "avif" | "png" | "jxl" | "heic" | "heif" => true,
        "jpg" | "jpeg" | "webp" | "tiff" | "tif" | "bmp" => !hdr,
        _ => false,
    }
}

/// Inverse PQ (SMPTE ST 2084): linear fraction of 10000 cd/m^2 -> encoded [0,1].
fn pq_inverse(l: f32) -> f32 {
    let l = l.clamp(0.0, 1.0);
    const M1: f32 = 0.1593017578125;
    const M2: f32 = 78.84375;
    const C1: f32 = 0.8359375;
    const C2: f32 = 18.8515625;
    const C3: f32 = 18.6875;
    let lp = l.powf(M1);
    ((C1 + C2 * lp) / (1.0 + C3 * lp)).powf(M2)
}

/// Crop, then rotate clockwise by `rot` quadrants, as pure pixel moves, so an edit costs
/// nothing in quality. Matches what the viewer shows, in that order.
pub fn crop_rotate(img: &HdrImage, crop: Crop, rot: u32) -> HdrImage {
    let full = Crop { x: 0, y: 0, w: img.w, h: img.h };
    let mut c = if crop.is_empty() { full } else { crop };
    c.x = c.x.min(img.w);
    c.y = c.y.min(img.h);
    c.w = c.w.min(img.w - c.x);
    c.h = c.h.min(img.h - c.y);
    if c.is_empty() {
        c = full;
    }
    let rot = rot & 3;
    let (ow, oh) = if rot & 1 == 1 { (c.h, c.w) } else { (c.w, c.h) };
    let mut out = vec![f16::ZERO; ow as usize * oh as usize * 4];
    for y in 0..c.h {
        for x in 0..c.w {
            let s = (((c.y + y) * img.w + c.x + x) * 4) as usize;
            let (dx, dy) = match rot {
                1 => (c.h - 1 - y, x),
                2 => (c.w - 1 - x, c.h - 1 - y),
                3 => (y, c.w - 1 - x),
                _ => (x, y),
            };
            let d = ((dy * ow + dx) * 4) as usize;
            out[d..d + 4].copy_from_slice(&img.rgba16f[s..s + 4]);
        }
    }
    HdrImage { w: ow, h: oh, rgba16f: out, hdr: img.hdr, kind: img.kind, primaries: img.primaries, pages: 1 }
}

/// One pixel's three channels, encoded to the integer range the file format wants.
/// `hdr` selects PQ over sRGB; `convert` folds wider primaries into BT.709 first.
fn encode_px(px: &[f16], hdr: bool, convert: Option<Primaries>, max: f32) -> [u16; 3] {
    let mut rgb = [px[0].to_f32(), px[1].to_f32(), px[2].to_f32()];
    if let Some(p) = convert {
        rgb = to_bt709(rgb, p);
        for c in &mut rgb {
            *c = c.max(0.0);
        }
    }
    let mut out = [0u16; 3];
    for (i, c) in rgb.iter().enumerate() {
        let e = if hdr { pq_inverse(c * 203.0 / 10000.0) } else { srgb_oetf(*c) };
        out[i] = (e.clamp(0.0, 1.0) * max).round() as u16;
    }
    out
}

/// Writes `img` to `path`, cropped and rotated, in the format the extension names.
/// `quality` is 0..100 and ignored by the lossless paths.
pub fn encode_image(path: &Path, img: &HdrImage, crop: Crop, rot: u32, quality: u8) -> Result<(), EncodeError> {
    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or_default().to_lowercase();
    if !can_encode_extension(&ext, img.hdr) {
        if img.hdr && matches!(ext.as_str(), "jpg" | "jpeg" | "webp" | "tiff" | "tif" | "bmp") {
            return Err(EncodeError::NotHdrCapable(ext));
        }
        return Err(EncodeError::Unsupported(ext));
    }
    if !img.valid() {
        return Err(EncodeError::Empty);
    }
    let edited = crop_rotate(img, crop, rot);
    if !edited.valid() {
        return Err(EncodeError::Empty);
    }
    match ext.as_str() {
        "png" => png(path, &edited),
        "avif" => avif(path, &edited, quality),
        "jxl" => jxl(path, &edited, quality),
        "heic" | "heif" => heic(path, &edited, quality),
        _ => sdr_via_image_crate(path, &edited),
    }
}

/// HDR: 16-bit PQ with a cICP chunk naming the real primaries, and no ICC profile to argue
/// with it. SDR: plain 8-bit sRGB, converted to BT.709 since that is what an untagged PNG means.
fn png(path: &Path, img: &HdrImage) -> Result<(), EncodeError> {
    let (w, h) = (img.w as usize, img.h as usize);
    let mut bytes = Vec::new();
    {
        let mut enc = png::Encoder::new(&mut bytes, img.w, img.h);
        if img.hdr {
            enc.set_color(png::ColorType::Rgba);
            enc.set_depth(png::BitDepth::Sixteen);
        } else {
            enc.set_color(png::ColorType::Rgb);
            enc.set_depth(png::BitDepth::Eight);
        }
        let mut writer = enc.write_header().map_err(|e| EncodeError::Backend(e.to_string()))?;
        if img.hdr {
            let cicp = [primaries_cicp(img.primaries), 16, 0, 1];
            writer
                .write_chunk(png::chunk::ChunkType(*b"cICP"), &cicp)
                .map_err(|e| EncodeError::Backend(e.to_string()))?;
            let mut data = vec![0u8; w * h * 8];
            for (px, o) in img.rgba16f.chunks_exact(4).zip(data.chunks_exact_mut(8)) {
                let v = encode_px(px, true, None, 65535.0);
                for c in 0..3 {
                    o[c * 2..c * 2 + 2].copy_from_slice(&v[c].to_be_bytes());
                }
                o[6..8].copy_from_slice(&65535u16.to_be_bytes());
            }
            writer.write_image_data(&data).map_err(|e| EncodeError::Backend(e.to_string()))?;
        } else {
            let mut data = vec![0u8; w * h * 3];
            for (px, o) in img.rgba16f.chunks_exact(4).zip(data.chunks_exact_mut(3)) {
                let v = encode_px(px, false, Some(img.primaries), 255.0);
                for c in 0..3 {
                    o[c] = v[c] as u8;
                }
            }
            writer.write_image_data(&data).map_err(|e| EncodeError::Backend(e.to_string()))?;
        }
    }
    std::fs::write(path, &bytes).map_err(EncodeError::Io)
}

/// CICP code point for the primaries (BT.709 = 1, BT.2020 = 9, Display-P3 = 12).
fn primaries_cicp(p: Primaries) -> u8 {
    match p {
        Primaries::Bt2020 => 9,
        Primaries::DisplayP3 => 12,
        Primaries::Bt709 => 1,
    }
}

/// SDR-only formats: everything becomes 8-bit sRGB in BT.709.
fn sdr_via_image_crate(path: &Path, img: &HdrImage) -> Result<(), EncodeError> {
    let mut data = vec![0u8; img.w as usize * img.h as usize * 3];
    for (px, o) in img.rgba16f.chunks_exact(4).zip(data.chunks_exact_mut(3)) {
        let v = encode_px(px, false, Some(img.primaries), 255.0);
        for c in 0..3 {
            o[c] = v[c] as u8;
        }
    }
    let buf = image::RgbImage::from_raw(img.w, img.h, data).ok_or(EncodeError::Empty)?;
    buf.save(path).map_err(|e| EncodeError::Backend(e.to_string()))
}

/// AVIF: 10-bit PQ for HDR, 8-bit sRGB otherwise, YUV 4:4:4 full range, with the image's own
/// primaries written into the file rather than converted away.
#[cfg(feature = "avif")]
fn avif(path: &Path, img: &HdrImage, quality: u8) -> Result<(), EncodeError> {
    use crate::avif_sys as sys;

    let depth = if img.hdr { 10 } else { 8 };
    let max = ((1u32 << depth) - 1) as f32;
    unsafe {
        let image = sys::avifImageCreate(img.w, img.h, depth, sys::avifPixelFormat_AVIF_PIXEL_FORMAT_YUV444);
        if image.is_null() {
            return Err(EncodeError::Backend("avifImageCreate failed".into()));
        }
        let _img_guard = Guard(Box::new(move || sys::avifImageDestroy(image)));
        match img.primaries {
            Primaries::Bt2020 => {
                (*image).colorPrimaries = sys::AVIF_COLOR_PRIMARIES_BT2020 as u16;
                (*image).matrixCoefficients = sys::AVIF_MATRIX_COEFFICIENTS_BT2020_NCL as u16;
            }
            Primaries::DisplayP3 => {
                (*image).colorPrimaries = sys::AVIF_COLOR_PRIMARIES_SMPTE432 as u16;
                (*image).matrixCoefficients = sys::AVIF_MATRIX_COEFFICIENTS_BT709 as u16;
            }
            Primaries::Bt709 => {
                (*image).colorPrimaries = sys::AVIF_COLOR_PRIMARIES_BT709 as u16;
                (*image).matrixCoefficients = sys::AVIF_MATRIX_COEFFICIENTS_BT709 as u16;
            }
        }
        (*image).transferCharacteristics = if img.hdr {
            sys::AVIF_TRANSFER_CHARACTERISTICS_PQ as u16
        } else {
            sys::AVIF_TRANSFER_CHARACTERISTICS_SRGB as u16
        };
        (*image).yuvRange = sys::avifRange_AVIF_RANGE_FULL;

        let mut rgb: sys::avifRGBImage = std::mem::zeroed();
        sys::avifRGBImageSetDefaults(&mut rgb, image);
        rgb.format = sys::avifRGBFormat_AVIF_RGB_FORMAT_RGB;
        rgb.depth = depth;
        rgb.ignoreAlpha = 1;
        if sys::avifRGBImageAllocatePixels(&mut rgb) != sys::avifResult_AVIF_RESULT_OK {
            return Err(EncodeError::Backend("avifRGBImageAllocatePixels failed".into()));
        }
        for y in 0..img.h {
            let row = rgb.pixels.add(y as usize * rgb.rowBytes as usize);
            for x in 0..img.w {
                let s = ((y * img.w + x) * 4) as usize;
                let v = encode_px(&img.rgba16f[s..s + 4], img.hdr, None, max);
                if depth > 8 {
                    let d = (row as *mut u16).add(x as usize * 3);
                    for c in 0..3 {
                        *d.add(c) = v[c];
                    }
                } else {
                    let d = row.add(x as usize * 3);
                    for c in 0..3 {
                        *d.add(c) = v[c] as u8;
                    }
                }
            }
        }
        let r = sys::avifImageRGBToYUV(image, &rgb);
        sys::avifRGBImageFreePixels(&mut rgb);
        if r != sys::avifResult_AVIF_RESULT_OK {
            return Err(EncodeError::Backend(avif_err(r)));
        }

        let enc = sys::avifEncoderCreate();
        if enc.is_null() {
            return Err(EncodeError::Backend("avifEncoderCreate failed".into()));
        }
        (*enc).quality = quality as i32;
        (*enc).speed = 6;
        let mut out: sys::avifRWData = std::mem::zeroed();
        let r = sys::avifEncoderWrite(enc, image, &mut out);
        sys::avifEncoderDestroy(enc);
        if r != sys::avifResult_AVIF_RESULT_OK {
            sys::avifRWDataFree(&mut out);
            return Err(EncodeError::Backend(avif_err(r)));
        }
        let bytes = std::slice::from_raw_parts(out.data, out.size);
        let written = std::fs::write(path, bytes);
        sys::avifRWDataFree(&mut out);
        written.map_err(EncodeError::Io)
    }
}

#[cfg(not(feature = "avif"))]
fn avif(_: &Path, _: &HdrImage, _: u8) -> Result<(), EncodeError> {
    Err(EncodeError::Unsupported("avif".into()))
}

#[cfg(feature = "avif")]
fn avif_err(r: crate::avif_sys::avifResult) -> String {
    unsafe { std::ffi::CStr::from_ptr(crate::avif_sys::avifResultToString(r)).to_string_lossy().into_owned() }
}

/// Runs a closure on the way out, so an early return still frees the C object.
struct Guard(Box<dyn FnMut()>);

impl Drop for Guard {
    fn drop(&mut self) {
        (self.0)();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::decode::HdrKind;

    /// A gradient that reaches well past SDR white (1.0 = 203 nits), so a transfer curve that
    /// clips or mis-scales shows up immediately.
    fn sample(hdr: bool, primaries: Primaries) -> HdrImage {
        sample_with(hdr, primaries, false)
    }

    /// `grey` makes every pixel neutral, which is how a format that subsamples chroma gets
    /// tested on what we actually control: the transfer curve, the bit packing and the tagging.
    fn sample_with(hdr: bool, primaries: Primaries, grey: bool) -> HdrImage {
        let (w, h) = (64u32, 32u32);
        let mut px = Vec::with_capacity((w * h * 4) as usize);
        for y in 0..h {
            for x in 0..w {
                let t = x as f32 / (w - 1) as f32;
                let peak = if hdr { 8.0 } else { 1.0 };
                let v = if grey {
                    [t * peak; 3]
                } else {
                    [t * peak, (1.0 - t) * peak, (y as f32 / (h - 1) as f32) * peak]
                };
                for c in v {
                    px.push(f16::from_f32(c));
                }
                px.push(f16::ONE);
            }
        }
        HdrImage { w, h, rgba16f: px, hdr, kind: if hdr { HdrKind::Pq } else { HdrKind::Sdr }, primaries, pages: 1 }
    }

    /// Compares in the space the file stores: PQ for HDR, sRGB for SDR. Linear differences are
    /// the wrong unit: one 10-bit PQ step near 1600 nits is a tenth of SDR white, which says
    /// nothing about whether the encoding is right. `codes` is the allowed error in quantisation
    /// steps of `depth` bits.
    fn round_trip(ext: &str, hdr: bool, primaries: Primaries, quality: u8, depth: u32, codes: f32) {
        round_trip_with_mean(ext, hdr, primaries, quality, depth, codes, codes)
    }

    #[allow(clippy::too_many_arguments)]
    fn round_trip_with_mean(ext: &str, hdr: bool, primaries: Primaries, quality: u8, depth: u32, codes: f32, mean_codes: f32) {
        round_trip_full(ext, hdr, primaries, quality, depth, codes, mean_codes, false)
    }

    #[allow(clippy::too_many_arguments)]
    fn round_trip_full(ext: &str, hdr: bool, primaries: Primaries, quality: u8, depth: u32, codes: f32, mean_codes: f32, grey: bool) {
        let img = sample_with(hdr, primaries, grey);
        let path = std::env::temp_dir().join(format!("vanta-hdr-roundtrip-{ext}-{}.{ext}", if hdr { "hdr" } else { "sdr" }));
        encode_image(&path, &img, Crop::default(), 0, quality).expect("encode");
        let back = crate::decode_image(&path, 0, 0).expect("decode");
        assert_eq!((back.w, back.h), (img.w, img.h), "{ext}: size");
        assert_eq!(back.hdr, hdr, "{ext}: hdr flag");
        if hdr {
            assert_eq!(back.primaries, primaries, "{ext}: primaries survived the file");
        }
        let encode = |v: f32| if hdr { pq_inverse(v * 203.0 / 10000.0) } else { srgb_oetf(v) };
        let step = 1.0 / ((1u32 << depth) - 1) as f32;
        let (mut worst, mut sum, mut n) = (0.0f32, 0.0f64, 0u64);
        for (a, b) in img.rgba16f.chunks_exact(4).zip(back.rgba16f.chunks_exact(4)) {
            for c in 0..3 {
                let d = (encode(a[c].to_f32()) - encode(b[c].to_f32())).abs();
                worst = worst.max(d);
                sum += d as f64;
                n += 1;
            }
        }
        let mean = (sum / n as f64) as f32;
        assert!(worst <= codes * step, "{ext}: worst error {:.2} codes of {depth}-bit, allowed {codes}", worst / step);
        assert!(mean <= mean_codes * step, "{ext}: mean error {:.2} codes of {depth}-bit, allowed {mean_codes}", mean / step);
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn png_hdr_round_trip_keeps_pq_and_primaries() {
        round_trip("png", true, Primaries::Bt2020, 100, 16, 2.0);
    }

    #[test]
    fn png_sdr_round_trip() {
        round_trip("png", false, Primaries::Bt709, 100, 8, 1.0);
    }

    #[cfg(feature = "avif")]
    #[test]
    fn avif_hdr_round_trip_keeps_pq_and_primaries() {
        // Three codes rather than one: AVIF stores YUV, so the values are rounded on the way
        // in and again on the way back, with the chroma matrix in between.
        round_trip("avif", true, Primaries::Bt2020, 100, 10, 3.0);
    }

    #[cfg(feature = "jxl")]
    #[test]
    fn jxl_hdr_round_trip_keeps_pq_and_primaries() {
        round_trip("jxl", true, Primaries::Bt2020, 100, 16, 3.0);
    }

    #[cfg(feature = "heif")]
    #[test]
    fn heic_hdr_round_trip_keeps_pq_and_primaries() {
        // A neutral ramp: HEVC subsamples chroma, and measuring that would say more about x265
        // than about this encoder. On a real 4K photograph the peak lands within one percent of
        // the source.
        round_trip_full("heic", true, Primaries::Bt2020, 100, 10, 8.0, 3.0, true);
    }

    #[cfg(feature = "jxl")]
    #[test]
    fn jxl_sdr_round_trip() {
        round_trip("jxl", false, Primaries::Bt709, 100, 8, 2.0);
    }

    #[cfg(feature = "avif")]
    #[test]
    fn avif_sdr_round_trip() {
        round_trip("avif", false, Primaries::Bt709, 100, 8, 2.0);
    }

    #[test]
    fn crop_then_rotate_matches_the_viewer() {
        let img = sample(false, Primaries::Bt709);
        let c = Crop { x: 4, y: 2, w: 8, h: 6 };
        let r = crop_rotate(&img, c, 1);
        assert_eq!((r.w, r.h), (6, 8), "90 degrees swaps the axes");
        // Top-left of the crop lands top-right after one clockwise quadrant.
        let src = &img.rgba16f[(((2) * img.w + 4) * 4) as usize..][..3];
        let dst = &r.rgba16f[((r.w - 1) * 4) as usize..][..3];
        assert_eq!(src[0], dst[0]);
        assert_eq!(src[1], dst[1]);
    }

    #[test]
    fn refuses_hdr_into_formats_that_cannot_hold_it() {
        assert!(!can_encode_extension("jpg", true));
        assert!(can_encode_extension("jpg", false));
        assert!(can_encode_extension("avif", true));
        assert!(!can_encode_extension("gif", false));
    }
}

/// JXL keeps our own encoding rather than re-interpreting it: the basic info says 16-bit
/// (PQ) or 8-bit (sRGB) integers, the colour encoding declares the transfer curve and the
/// image's native primaries, and `uses_original_profile` tells libjxl not to convert.
#[cfg(feature = "jxl")]
fn jxl(path: &Path, img: &HdrImage, quality: u8) -> Result<(), EncodeError> {
    use jpegxl_sys::color::color_encoding::{JxlColorEncoding, JxlColorSpace, JxlPrimaries, JxlRenderingIntent, JxlTransferFunction, JxlWhitePoint};
    use jpegxl_sys::common::types::{JxlDataType, JxlEndianness, JxlPixelFormat};
    use jpegxl_sys::encoder::encode as jx;
    use jpegxl_sys::metadata::codestream_header::JxlBasicInfo;

    // libjxl judges quality with a metric built for display-referred images, so it spends far
    // too little on PQ highlights: at quality 92 a measured 1808-nit specular reflection came
    // back at 6788. 98 lands within two nits of the source for a fifth of the size of 100.
    let quality = if img.hdr { quality.max(98) } else { quality };
    let hdr = img.hdr;
    unsafe {
        let enc = jx::JxlEncoderCreate(std::ptr::null());
        if enc.is_null() {
            return Err(EncodeError::Backend("JxlEncoderCreate failed".into()));
        }
        let _guard = Guard(Box::new(move || jx::JxlEncoderDestroy(enc)));

        // Filled by the library rather than zeroed: the struct holds enums with no zero
        // variant, so zeroing it is not a valid value of the type.
        let mut info = std::mem::MaybeUninit::<JxlBasicInfo>::uninit();
        jx::JxlEncoderInitBasicInfo(info.as_mut_ptr());
        let mut info = info.assume_init();
        info.xsize = img.w;
        info.ysize = img.h;
        info.num_color_channels = 3;
        info.alpha_bits = 0;
        info.bits_per_sample = if hdr { 16 } else { 8 };
        info.exponent_bits_per_sample = 0;
        info.uses_original_profile = true.into();
        if jx::JxlEncoderSetBasicInfo(enc, &info) != jx::JxlEncoderStatus::Success {
            return Err(EncodeError::Backend("JxlEncoderSetBasicInfo failed".into()));
        }

        // Named primaries and transfer, so the xy coordinates and gamma below are unused.
        let color = JxlColorEncoding {
            color_space: JxlColorSpace::Rgb,
            white_point: JxlWhitePoint::D65,
            white_point_xy: [0.0, 0.0],
            primaries: match img.primaries {
                Primaries::Bt2020 => JxlPrimaries::Rec2100,
                Primaries::DisplayP3 => JxlPrimaries::P3,
                Primaries::Bt709 => JxlPrimaries::SRgb,
            },
            primaries_red_xy: [0.0, 0.0],
            primaries_green_xy: [0.0, 0.0],
            primaries_blue_xy: [0.0, 0.0],
            transfer_function: if hdr { JxlTransferFunction::PQ } else { JxlTransferFunction::SRGB },
            gamma: 0.0,
            rendering_intent: JxlRenderingIntent::Relative,
        };
        if jx::JxlEncoderSetColorEncoding(enc, &color) != jx::JxlEncoderStatus::Success {
            return Err(EncodeError::Backend("JxlEncoderSetColorEncoding failed".into()));
        }

        let fs = jx::JxlEncoderFrameSettingsCreate(enc, std::ptr::null());
        jx::JxlEncoderSetFrameDistance(fs, jx::JxlEncoderDistanceFromQuality(quality as f32));

        let max = if hdr { 65535.0 } else { 255.0 };
        let mut pixels = vec![0u8; img.w as usize * img.h as usize * 3 * if hdr { 2 } else { 1 }];
        for (px, o) in img.rgba16f.chunks_exact(4).zip(pixels.chunks_exact_mut(if hdr { 6 } else { 3 })) {
            let v = encode_px(px, hdr, None, max);
            for c in 0..3 {
                if hdr {
                    o[c * 2..c * 2 + 2].copy_from_slice(&v[c].to_ne_bytes());
                } else {
                    o[c] = v[c] as u8;
                }
            }
        }
        let pf = JxlPixelFormat {
            num_channels: 3,
            data_type: if hdr { JxlDataType::Uint16 } else { JxlDataType::Uint8 },
            endianness: JxlEndianness::Native,
            align: 0,
        };
        if jx::JxlEncoderAddImageFrame(fs, &pf, pixels.as_ptr().cast(), pixels.len()) != jx::JxlEncoderStatus::Success {
            return Err(EncodeError::Backend("JxlEncoderAddImageFrame failed".into()));
        }
        jx::JxlEncoderCloseInput(enc);

        let mut out = vec![0u8; 1 << 20];
        let mut used = 0usize;
        loop {
            let mut next = out.as_mut_ptr().add(used);
            let mut avail = out.len() - used;
            let st = jx::JxlEncoderProcessOutput(enc, &mut next, &mut avail);
            used = out.len() - avail;
            match st {
                jx::JxlEncoderStatus::NeedMoreOutput => out.resize(out.len() * 2, 0),
                jx::JxlEncoderStatus::Success => break,
                _ => return Err(EncodeError::Backend("JXL encode failed".into())),
            }
        }
        out.truncate(used);
        std::fs::write(path, &out).map_err(EncodeError::Io)
    }
}

#[cfg(not(feature = "jxl"))]
fn jxl(_: &Path, _: &HdrImage, _: u8) -> Result<(), EncodeError> {
    Err(EncodeError::Unsupported("jxl".into()))
}

/// HEIC through HEVC: 10-bit PQ or 8-bit sRGB interleaved, with an nclx profile stating the
/// primaries, transfer and matrix. The Rust wrapper can only set primaries, so the profile is
/// built against the library directly.
#[cfg(feature = "heif")]
fn heic(path: &Path, img: &HdrImage, quality: u8) -> Result<(), EncodeError> {
    use libheif_sys as lh;

    let hdr = img.hdr;
    let depth = if hdr { 10 } else { 8 };
    let max = ((1u32 << depth) - 1) as f32;
    let check = |e: lh::heif_error, what: &str| -> Result<(), EncodeError> {
        if e.code == lh::heif_error_code_heif_error_Ok {
            Ok(())
        } else {
            let msg = unsafe { std::ffi::CStr::from_ptr(e.message) }.to_string_lossy().into_owned();
            Err(EncodeError::Backend(format!("{what}: {msg}")))
        }
    };

    unsafe {
        let ctx = lh::heif_context_alloc();
        if ctx.is_null() {
            return Err(EncodeError::Backend("heif_context_alloc failed".into()));
        }
        let _ctx_guard = Guard(Box::new(move || lh::heif_context_free(ctx)));

        let mut encoder = std::ptr::null_mut();
        check(
            lh::heif_context_get_encoder_for_format(ctx, lh::heif_compression_format_heif_compression_HEVC, &mut encoder),
            "no HEVC encoder available",
        )?;
        let _enc_guard = Guard(Box::new(move || lh::heif_encoder_release(encoder)));
        lh::heif_encoder_set_lossy_quality(encoder, quality as i32);

        let chroma = if hdr {
            lh::heif_chroma_heif_chroma_interleaved_RRGGBB_LE
        } else {
            lh::heif_chroma_heif_chroma_interleaved_RGB
        };
        let mut image = std::ptr::null_mut();
        check(
            lh::heif_image_create(img.w as i32, img.h as i32, lh::heif_colorspace_heif_colorspace_RGB, chroma, &mut image),
            "heif_image_create",
        )?;
        let _img_guard = Guard(Box::new(move || lh::heif_image_release(image)));
        check(
            lh::heif_image_add_plane(image, lh::heif_channel_heif_channel_interleaved, img.w as i32, img.h as i32, depth as i32),
            "heif_image_add_plane",
        )?;

        let mut stride = 0i32;
        let plane = lh::heif_image_get_plane(image, lh::heif_channel_heif_channel_interleaved, &mut stride);
        if plane.is_null() {
            return Err(EncodeError::Backend("heif_image_get_plane returned nothing".into()));
        }
        for y in 0..img.h {
            let row = plane.add(y as usize * stride as usize);
            for x in 0..img.w {
                let s = ((y * img.w + x) * 4) as usize;
                let v = encode_px(&img.rgba16f[s..s + 4], hdr, None, max);
                if hdr {
                    let d = (row as *mut u16).add(x as usize * 3);
                    for c in 0..3 {
                        *d.add(c) = v[c];
                    }
                } else {
                    let d = row.add(x as usize * 3);
                    for c in 0..3 {
                        *d.add(c) = v[c] as u8;
                    }
                }
            }
        }

        let nclx = lh::heif_nclx_color_profile_alloc();
        if nclx.is_null() {
            return Err(EncodeError::Backend("heif_nclx_color_profile_alloc failed".into()));
        }
        let _nclx_guard = Guard(Box::new(move || lh::heif_nclx_color_profile_free(nclx)));
        (*nclx).color_primaries = match img.primaries {
            Primaries::Bt2020 => lh::heif_color_primaries_heif_color_primaries_ITU_R_BT_2020_2_and_2100_0,
            Primaries::DisplayP3 => lh::heif_color_primaries_heif_color_primaries_SMPTE_EG_432_1,
            Primaries::Bt709 => lh::heif_color_primaries_heif_color_primaries_ITU_R_BT_709_5,
        };
        (*nclx).transfer_characteristics = if hdr {
            lh::heif_transfer_characteristics_heif_transfer_characteristic_ITU_R_BT_2100_0_PQ
        } else {
            lh::heif_transfer_characteristics_heif_transfer_characteristic_IEC_61966_2_1
        };
        (*nclx).matrix_coefficients = match img.primaries {
            Primaries::Bt2020 => lh::heif_matrix_coefficients_heif_matrix_coefficients_ITU_R_BT_2020_2_non_constant_luminance,
            _ => lh::heif_matrix_coefficients_heif_matrix_coefficients_ITU_R_BT_709_5,
        };
        (*nclx).full_range_flag = 1;
        check(lh::heif_image_set_nclx_color_profile(image, nclx), "heif_image_set_nclx_color_profile")?;

        let opts = lh::heif_encoding_options_alloc();
        if opts.is_null() {
            return Err(EncodeError::Backend("heif_encoding_options_alloc failed".into()));
        }
        let _opt_guard = Guard(Box::new(move || lh::heif_encoding_options_free(opts)));
        (*opts).output_nclx_profile = nclx;

        let mut handle = std::ptr::null_mut();
        check(lh::heif_context_encode_image(ctx, image, encoder, opts, &mut handle), "encode")?;
        if !handle.is_null() {
            lh::heif_image_handle_release(handle);
        }
        let cpath = std::ffi::CString::new(path.to_string_lossy().as_bytes()).map_err(|_| EncodeError::Backend("path".into()))?;
        check(lh::heif_context_write_to_file(ctx, cpath.as_ptr()), "write")
    }
}

#[cfg(not(feature = "heif"))]
fn heic(_: &Path, _: &HdrImage, _: u8) -> Result<(), EncodeError> {
    Err(EncodeError::Unsupported("heic".into()))
}
