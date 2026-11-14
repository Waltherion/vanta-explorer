//! Format dispatch. Every path produces the same fp16 linear 203-nit image.

use crate::color::{self, Primaries, Tf, TfLut};
use half::f16;
use rayon::prelude::*;
use std::path::Path;

#[cfg(feature = "avif")]
mod avif;
#[cfg(feature = "heif")]
mod heif;
#[cfg(feature = "jxl")]
mod jxl;
#[cfg(feature = "raw")]
mod raw;
mod plain;
mod png_cicp;
#[cfg(feature = "video")]
mod video;

#[allow(dead_code)]
fn not_compiled(what: &str) -> DecodeError {
    DecodeError::Unsupported(format!("{what} support is not compiled into this build"))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum HdrKind {
    #[default]
    Sdr,
    Pq,
    Hlg,
    GainMap,
    LinearHdr,
}

#[derive(Debug)]
pub struct HdrImage {
    pub w: u32,
    pub h: u32,
    /// w*h*4 half-floats, linear, 1.0 = 203 nits, in `primaries`.
    pub rgba16f: Vec<f16>,
    /// Gates the shader's SDR tone-map branch.
    pub hdr: bool,
    pub kind: HdrKind,
    /// NATIVE primaries; converted by the shader, not here.
    pub primaries: Primaries,
    pub pages: u32,
}

impl HdrImage {
    pub fn valid(&self) -> bool {
        self.w > 0 && self.h > 0 && self.rgba16f.len() as u64 == self.w as u64 * self.h as u64 * 4
    }

    /// Build from f32 linear RGBA already on the 203-nit scale.
    pub(crate) fn from_linear_f32(
        w: u32,
        h: u32,
        rgba: &[f32],
        tf: Tf,
        primaries: Primaries,
    ) -> Self {
        let kind = match tf {
            Tf::Pq => HdrKind::Pq,
            Tf::Hlg => HdrKind::Hlg,
            Tf::Linear => HdrKind::LinearHdr,
            Tf::Srgb => HdrKind::Sdr,
        };
        Self {
            w,
            h,
            rgba16f: rgba.par_iter().map(|v| f16::from_f32(*v)).collect(),
            hdr: !matches!(tf, Tf::Srgb),
            kind,
            primaries, pages: 1,
        }
    }
}

#[derive(Debug)]
pub enum DecodeError {
    Unsupported(String),
    Io(String),
    Decode(String),
}

impl std::fmt::Display for DecodeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DecodeError::Unsupported(s) => write!(f, "unsupported: {s}"),
            DecodeError::Io(s) => write!(f, "io: {s}"),
            DecodeError::Decode(s) => write!(f, "decode: {s}"),
        }
    }
}

impl std::error::Error for DecodeError {}

/// Decode `path`, optionally downscaling so neither side exceeds `max_dim`.
///
/// Downscaling happens BEFORE linearisation where the decoder supports it (vantaviewer does
/// the same with avifImageScale): linearising a 60-megapixel image only to throw most of it
/// away is the single biggest waste in a thumbnailer.
pub fn decode_image(path: &Path, max_dim: u32, page: u32) -> Result<HdrImage, DecodeError> {
    // Follow symlinks first: vantapaper's state links (`wallpapers/<output>`) carry no extension,
    // and routing them to the generic decoder silently drops the PNG's cICP chunk.
    let resolved = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    let mut ext = resolved
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    let sniffed = sniff_extension(&resolved);
    if ext.is_empty() {
        ext = sniffed.unwrap_or_default();
    } else if sniffed.as_deref() == Some("png") {
        // A real PNG under another name still needs the cICP-aware decoder.
        ext = "png".into();
    } else if ext == "png" {
        // Named .png but without the PNG signature: a JPEG saved with the wrong extension is
        // common. The generic decoder picks the format from the content, so let it try
        // instead of failing on "Invalid PNG signature". Only PNG gets this treatment: its
        // signature is unambiguous, whereas an ftyp box can be AVIF, HEIC or MP4.
        ext.clear();
    }
    let path = resolved.as_path();

    match ext.as_str() {
        #[cfg(feature = "avif")]
        "avif" => avif::decode(path, max_dim),
        #[cfg(not(feature = "avif"))]
        "avif" => Err(not_compiled("AVIF")),
        #[cfg(feature = "jxl")]
        "jxl" => jxl::decode(path, max_dim),
        #[cfg(not(feature = "jxl"))]
        "jxl" => Err(not_compiled("JPEG XL")),
        #[cfg(feature = "heif")]
        "heic" | "heif" | "avci" => heif::decode(path, max_dim),
        #[cfg(not(feature = "heif"))]
        "heic" | "heif" | "avci" => Err(not_compiled("HEIF")),
        "png" => png_cicp::decode(path, max_dim),
        #[cfg(feature = "raw")]
        ext if raw::EXTENSIONS.contains(&ext) => raw::decode(path, max_dim),
        #[cfg(feature = "video")]
        ext if VIDEO_EXTS.contains(&ext) => video::decode_first_frame(path, max_dim, page),
        #[cfg(not(feature = "video"))]
        ext if VIDEO_EXTS.contains(&ext) => {
            let _ = page;
            Err(not_compiled("video"))
        }
        // EXR / Radiance / everything the `image` crate handles.
        _ => plain::decode(path, max_dim),
    }
}

/// Best-effort container detection from the first bytes, for extension-less files.
fn sniff_extension(path: &Path) -> Option<String> {
    use std::io::Read;
    let mut head = [0u8; 32];
    let n = std::fs::File::open(path).ok()?.read(&mut head).ok()?;
    let head = &head[..n];
    if head.starts_with(b"\x89PNG\r\n\x1a\n") {
        return Some("png".into());
    }
    if head.starts_with(&[0xFF, 0x0A]) || head.starts_with(b"\0\0\0\x0CJXL ") {
        return Some("jxl".into());
    }
    if n >= 12 && &head[4..8] == b"ftyp" {
        let brand = &head[8..12];
        if brand.starts_with(b"avi") {
            return Some("avif".into());
        }
        if brand.starts_with(b"hei") || brand.starts_with(b"mif") || brand.starts_with(b"msf") {
            return Some("heic".into());
        }
        return Some("mp4".into());
    }
    if head.starts_with(b"\x1aE\xdf\xa3") {
        return Some("mkv".into());
    }
    None
}

const VIDEO_EXTS: &[&str] = &[
    "mp4", "mkv", "webm", "mov", "avi", "m4v", "wmv", "flv", "ts", "mts", "m2ts", "mpg", "mpeg",
    "ogv", "3gp",
];

#[cfg(feature = "raw")]
fn raw_ext(e: &str) -> bool {
    raw::EXTENSIONS.contains(&e)
}

#[cfg(not(feature = "raw"))]
fn raw_ext(_: &str) -> bool {
    false
}

/// Extensions this build will attempt. Used by the preview pane to avoid pointless work.
pub fn is_supported(path: &Path) -> bool {
    const EXTS: &[&str] = &[
        "png", "jpg", "jpeg", "webp", "gif", "bmp", "tif", "tiff", "tga", "ico", "exr", "hdr",
        "pnm", "ppm", "pgm", "qoi", "dds", "ff",
    ];
    path.extension()
        .and_then(|e| e.to_str())
        .map(|e| {
            let e = e.to_ascii_lowercase();
            EXTS.contains(&e.as_str())
                || raw_ext(&e)
                || (cfg!(feature = "avif") && e == "avif")
                || (cfg!(feature = "jxl") && e == "jxl")
                || (cfg!(feature = "heif") && matches!(e.as_str(), "heic" | "heif" | "avci"))
                || (cfg!(feature = "video") && VIDEO_EXTS.contains(&e.as_str()))
        })
        .unwrap_or(false)
}

/// Shared helper: linearise interleaved encoded RGBA rows in parallel.
pub(crate) fn linearise_rgba(src: &[f32], tf: Tf, alpha_is_linear: bool) -> Vec<f32> {
    // Already linear, and not limited to 0..1: a float image's highlights sit above 1.0, and the
    // table below (made for encoded values) would clip them to reference white.
    if tf == Tf::Linear && alpha_is_linear {
        return src.to_vec();
    }
    let lut = TfLut::new(tf);
    let mut out = vec![0.0f32; src.len()];
    out.par_chunks_mut(4)
        .zip(src.par_chunks(4))
        .for_each(|(dst, s)| {
            dst[0] = lut.apply(s[0]);
            dst[1] = lut.apply(s[1]);
            dst[2] = lut.apply(s[2]);
            dst[3] = if alpha_is_linear {
                s[3]
            } else {
                color::srgb_eotf(s[3])
            };
        });
    out
}

/// 16-bit encoded RGBA straight to the finished image, for a decoder that can hand over integer
/// samples at full size: one table from code to linear half-float per transfer function (made
/// once per process), and one pass over the pixels on every core. The long way round (to f32,
/// a copy, linearised, then to f16) holds three full-size f32 copies of a 4K picture at once
/// and takes several times as long. Alpha is taken as linear.
pub(crate) fn from_codes_u16(w: u32, h: u32, codes: &[u16], tf: Tf, primaries: Primaries) -> HdrImage {
    let table = code_table(tf);
    let mut rgba16f = vec![f16::ZERO; codes.len()];
    rgba16f.par_chunks_mut(4 * w as usize).zip(codes.par_chunks(4 * w as usize)).for_each(|(dst, src)| {
        for (d, s) in dst.chunks_exact_mut(4).zip(src.chunks_exact(4)) {
            d[0] = table[s[0] as usize];
            d[1] = table[s[1] as usize];
            d[2] = table[s[2] as usize];
            d[3] = f16::from_f32(s[3] as f32 / 65535.0);
        }
    });
    let mut img = HdrImage::from_linear_f32(0, 0, &[], tf, primaries);
    img.w = w;
    img.h = h;
    img.rgba16f = rgba16f;
    img
}

/// Every 16-bit code's linear value, as half-float, for one transfer function.
fn code_table(tf: Tf) -> std::sync::Arc<Vec<f16>> {
    use std::sync::{Arc, Mutex};
    static TABLES: Mutex<Vec<(Tf, Arc<Vec<f16>>)>> = Mutex::new(Vec::new());
    if let Some((_, t)) = TABLES.lock().unwrap().iter().find(|(t, _)| *t == tf) {
        return t.clone();
    }
    let t: Arc<Vec<f16>> = Arc::new((0..=u16::MAX).into_par_iter().map(|c| f16::from_f32(color::lin_chan(c as f32 / 65535.0, tf))).collect());
    TABLES.lock().unwrap().push((tf, t.clone()));
    t
}

/// Nearest-neighbour box scale on interleaved RGBA f32, used when a decoder cannot scale.
pub(crate) fn downscale_rgba(src: &[f32], w: u32, h: u32, max_dim: u32) -> (Vec<f32>, u32, u32) {
    if max_dim == 0 || (w <= max_dim && h <= max_dim) {
        return (src.to_vec(), w, h);
    }
    let scale = (max_dim as f32 / w.max(h) as f32).min(1.0);
    let nw = ((w as f32 * scale).round() as u32).max(1);
    let nh = ((h as f32 * scale).round() as u32).max(1);
    let mut out = vec![0.0f32; (nw as usize) * (nh as usize) * 4];
    out.par_chunks_mut(nw as usize * 4)
        .enumerate()
        .for_each(|(y, row)| {
            let sy = (y as u64 * h as u64 / nh as u64).min(h as u64 - 1) as usize;
            for x in 0..nw as usize {
                let sx = (x as u64 * w as u64 / nw as u64).min(w as u64 - 1) as usize;
                let si = (sy * w as usize + sx) * 4;
                row[x * 4..x * 4 + 4].copy_from_slice(&src[si..si + 4]);
            }
        });
    (out, nw, nh)
}

#[cfg(test)]
mod misnamed_tests {
    use super::*;

    fn tiny(format: image::ImageFormat, name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join("vanta-hdr-misnamed");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(name);
        let img = image::RgbImage::from_pixel(4, 3, image::Rgb([200, 30, 90]));
        img.save_with_format(&path, format).unwrap();
        path
    }

    #[test]
    fn jpeg_named_png_decodes() {
        let path = tiny(image::ImageFormat::Jpeg, "really-a-jpeg.png");
        let img = decode_image(&path, 0, 0).expect("content decides, not the extension");
        assert_eq!((img.w, img.h), (4, 3));
    }

    #[test]
    fn png_named_jpg_decodes() {
        let path = tiny(image::ImageFormat::Png, "really-a-png.jpg");
        let img = decode_image(&path, 0, 0).expect("real PNG under another name");
        assert_eq!((img.w, img.h), (4, 3));
    }
}
