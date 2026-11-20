#![allow(clippy::excessive_precision)] // standards constants, kept at spec precision
//! Transfer functions, primaries and the 203-nit convention.
//!
//! The constants
//! are load-bearing: PQ is absolute (10000 nits peak) and HLG is relative (1000 nits nominal),
//! and both are divided by 203 so everything lands on the shared reference white.

/// Reference white for the working format. libultrahdr's convention, adopted by all the
/// vanta apps: fp16 1.0 == 203 cd/m^2.
pub const REFERENCE_NITS: f32 = 203.0;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Tf {
    #[default]
    Srgb,
    Pq,
    Hlg,
    Linear,
}

/// Kept NATIVE through decode; the display shader converts. See the module docs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Primaries {
    #[default]
    Bt709,
    Bt2020,
    DisplayP3,
}

/// SMPTE ST 2084 EOTF. Returns a normalised 0..1 value where 1.0 = 10000 nits.
pub fn pq_eotf(c: f32) -> f32 {
    const M1: f32 = 2610.0 / 16384.0;
    const M2: f32 = 2523.0 / 4096.0 * 128.0;
    const C1: f32 = 3424.0 / 4096.0;
    const C2: f32 = 2413.0 / 4096.0 * 32.0;
    const C3: f32 = 2392.0 / 4096.0 * 32.0;
    // PQ is defined on [0,1]; a lossy codec can hand back a sample just above 1, and
    // extrapolating there turns a 1800-nit highlight into a 6800-nit one.
    let c = c.clamp(0.0, 1.0);
    let p = c.powf(1.0 / M2);
    let num = (p - C1).max(0.0);
    let den = C2 - C3 * p;
    if den.abs() < 1e-9 {
        return 0.0;
    }
    (num / den).powf(1.0 / M1)
}

/// ARIB STD-B67 (HLG) inverse OETF -> scene linear, normalised so 1.0 = nominal peak.
pub fn hlg_scene_linear(c: f32) -> f32 {
    const A: f32 = 0.17883277;
    const B: f32 = 0.28466892;
    const C: f32 = 0.55991073;
    let c = c.clamp(0.0, 1.0);
    if c <= 0.5 {
        (c * c) / 3.0
    } else {
        (((c - C) / A).exp() + B) / 12.0
    }
}

/// sRGB EOTF (encoded -> linear).
pub fn srgb_eotf(c: f32) -> f32 {
    if c <= 0.04045 {
        c / 12.92
    } else {
        ((c + 0.055) / 1.055).powf(2.4)
    }
}

/// sRGB OETF (linear -> encoded).
pub fn srgb_oetf(c: f32) -> f32 {
    if c <= 0.0031308 {
        12.92 * c
    } else {
        1.055 * c.max(0.0).powf(1.0 / 2.4) - 0.055
    }
}

/// One encoded channel -> linear on the 203-nit scale.
pub fn lin_chan(c: f32, tf: Tf) -> f32 {
    match tf {
        // PQ is absolute: full range is 10000 nits.
        Tf::Pq => pq_eotf(c) * 10000.0 / REFERENCE_NITS,
        // HLG is relative: nominal peak 1000 nits, with the display's system gamma of 1.2 at
        // that peak (BT.2100), which puts 75% signal on reference white. Applied per channel,
        // exact for greys.
        Tf::Hlg => hlg_scene_linear(c).powf(1.2) * 1000.0 / REFERENCE_NITS,
        // Already scene-linear (EXR, Radiance): 1.0 IS reference white.
        Tf::Linear => c,
        Tf::Srgb => srgb_eotf(c),
    }
}

/// 1024-entry LUT over the encoded range, with linear interpolation.
///
/// vantaviewer builds 65536- and 256-entry tables to avoid a per-pixel `pow`; interpolating a
/// smaller table gets the same accuracy for far less setup, which matters because we build one
/// per decode rather than once per process.
pub struct TfLut {
    table: Vec<f32>,
}

impl TfLut {
    const N: usize = 1024;

    pub fn new(tf: Tf) -> Self {
        let table = (0..=Self::N)
            .map(|i| lin_chan(i as f32 / Self::N as f32, tf))
            .collect();
        Self { table }
    }

    pub fn apply(&self, c: f32) -> f32 {
        let c = c.clamp(0.0, 1.0) * Self::N as f32;
        let i = c as usize;
        if i >= Self::N {
            return self.table[Self::N];
        }
        let frac = c - i as f32;
        self.table[i] * (1.0 - frac) + self.table[i + 1] * frac
    }
}

/// Reinhard with a white point, matching the SDR branch of image.frag exactly (Lw = 6.0),
/// so a CPU-side thumbnail and the GPU preview agree.
pub fn tonemap_reinhard(rgb: [f32; 3]) -> [f32; 3] {
    const LW: f32 = 6.0;
    let l = 0.2126 * rgb[0] + 0.7152 * rgb[1] + 0.0722 * rgb[2];
    if l <= 1e-4 {
        return rgb;
    }
    let lt = l * (1.0 + l / (LW * LW)) / (1.0 + l);
    let s = lt / l;
    [rgb[0] * s, rgb[1] * s, rgb[2] * s]
}

/// BT.2020 -> BT.709. Negatives are clamped by the caller for safe display.
pub fn bt2020_to_bt709(rgb: [f32; 3]) -> [f32; 3] {
    [
        1.660491 * rgb[0] - 0.587641 * rgb[1] - 0.072850 * rgb[2],
        -0.124550 * rgb[0] + 1.132900 * rgb[1] - 0.008349 * rgb[2],
        -0.018151 * rgb[0] - 0.100579 * rgb[1] + 1.118730 * rgb[2],
    ]
}

/// Display-P3 -> BT.709.
pub fn p3_to_bt709(rgb: [f32; 3]) -> [f32; 3] {
    [
        1.224940 * rgb[0] - 0.224940 * rgb[1],
        -0.042056 * rgb[0] + 1.042056 * rgb[1],
        -0.019637 * rgb[0] - 0.078636 * rgb[1] + 1.098273 * rgb[2],
    ]
}

pub fn to_bt709(rgb: [f32; 3], p: Primaries) -> [f32; 3] {
    match p {
        Primaries::Bt709 => rgb,
        Primaries::Bt2020 => bt2020_to_bt709(rgb),
        Primaries::DisplayP3 => p3_to_bt709(rgb),
    }
}

/// Read transfer function and primaries out of an ICC profile description.
///
/// THE critical heuristic: real HDR photos out of Lightroom set
/// CICP = unspecified and describe the colour space ONLY in the ICC profile. Without this,
/// such exports decode as plain sRGB and look wrong.
#[cfg(feature = "icc")]
pub fn tf_from_icc(icc: &[u8]) -> Option<(Tf, Option<Primaries>)> {
    let profile = lcms2::Profile::new_icc(icc).ok()?;
    let desc = profile.info(lcms2::InfoType::Description, lcms2::Locale::none())?;
    let d = desc.to_lowercase();

    // HLG first: "BT.2100" names both, so "2100" alone only means PQ once HLG is ruled out.
    let tf = if d.contains("hlg") {
        Tf::Hlg
    } else if d.contains("pq") || d.contains("2084") || d.contains("2100") {
        Tf::Pq
    } else if d.contains("linear") {
        Tf::Linear
    } else {
        Tf::Srgb
    };
    let prim = if d.contains("2020") || d.contains("2100") {
        Some(Primaries::Bt2020)
    } else if d.contains("p3") {
        Some(Primaries::DisplayP3)
    } else {
        None
    };
    Some((tf, prim))
}

/// Without the `icc` feature embedded profiles are ignored (the CICP/nclx tags still apply).
#[cfg(not(feature = "icc"))]
pub fn tf_from_icc(_icc: &[u8]) -> Option<(Tf, Option<Primaries>)> {
    None
}

/// What the profile embedded in an ordinary picture (a JPEG, a WebP, a TIFF) means for its pixels.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Embedded {
    /// sRGB, or nothing that can be read: the pixels are taken as sRGB.
    Srgb,
    /// A color space the pipeline knows by name, Display P3 from a phone or a Mac above all:
    /// the pixels are kept and the primaries said.
    Known(Tf, Primaries),
    /// Anything else (Adobe RGB, ProPhoto, a camera's own): the profile itself is needed to
    /// read the pixels, and they are converted with it.
    Other,
}

/// Reads an embedded profile by its description, the same way as `tf_from_icc`, and says which
/// of the three it is.
#[cfg(feature = "icc")]
pub fn embedded(icc: &[u8]) -> Embedded {
    let Ok(profile) = lcms2::Profile::new_icc(icc) else { return Embedded::Srgb };
    if profile.color_space() != lcms2::ColorSpaceSignature::RgbData {
        return Embedded::Srgb;
    }
    let d = profile.info(lcms2::InfoType::Description, lcms2::Locale::none()).unwrap_or_default().to_lowercase();
    // Trusted by name only where the name settles it: PQ and HLG, and Display P3 (sRGB's curve).
    // DCI-P3 has a 2.6 gamma and a linear profile has none, so those go through the profile.
    match tf_from_icc(icc) {
        Some((tf @ (Tf::Pq | Tf::Hlg), p)) => Embedded::Known(tf, p.unwrap_or(Primaries::Bt709)),
        Some((Tf::Srgb, Some(Primaries::DisplayP3))) if !d.contains("dci") => Embedded::Known(Tf::Srgb, Primaries::DisplayP3),
        Some((Tf::Srgb, None)) if d.contains("srgb") || d.contains("61966") => Embedded::Srgb,
        _ => Embedded::Other,
    }
}

#[cfg(not(feature = "icc"))]
pub fn embedded(_icc: &[u8]) -> Embedded {
    Embedded::Srgb
}

/// Converts RGBA float pixels, encoded as the profile `icc` describes, to linear light in BT.2020
/// primaries, in place: wide enough that nothing an Adobe RGB or ProPhoto picture holds is lost
/// on the way. Alpha is left as it is. False when the profile cannot be used, and nothing changed.
#[cfg(feature = "icc")]
pub fn convert_to_linear_bt2020(icc: &[u8], rgba: &mut [f32]) -> bool {
    use lcms2::{CIExyY, CIExyYTRIPLE, Flags, Intent, PixelFormat, Profile, ToneCurve, Transform};
    let Ok(from) = Profile::new_icc(icc) else { return false };
    let d65 = CIExyY { x: 0.3127, y: 0.3290, Y: 1.0 };
    let bt2020 = CIExyYTRIPLE {
        Red: CIExyY { x: 0.708, y: 0.292, Y: 1.0 },
        Green: CIExyY { x: 0.170, y: 0.797, Y: 1.0 },
        Blue: CIExyY { x: 0.131, y: 0.046, Y: 1.0 },
    };
    let linear = ToneCurve::new(1.0);
    let Ok(to) = Profile::new_rgb(&d65, &bt2020, &[&linear, &linear, &linear]) else { return false };
    let Ok(t) = Transform::<[f32; 4], [f32; 4]>::new_flags(&from, PixelFormat::RGBA_FLT, &to, PixelFormat::RGBA_FLT, Intent::RelativeColorimetric, Flags::COPY_ALPHA)
    else {
        return false;
    };
    t.transform_in_place(rgba.as_chunks_mut::<4>().0);
    true
}

#[cfg(not(feature = "icc"))]
pub fn convert_to_linear_bt2020(_icc: &[u8], _rgba: &mut [f32]) -> bool {
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pq_hits_reference_and_peak() {
        // PQ code value 1.0 is 10000 nits, which on the 203-nit scale is ~49.26.
        let peak = lin_chan(1.0, Tf::Pq);
        assert!((peak - 10000.0 / 203.0).abs() < 0.01, "peak was {peak}");
        assert!(lin_chan(0.0, Tf::Pq).abs() < 1e-6);
        // Monotonic.
        assert!(lin_chan(0.5, Tf::Pq) > lin_chan(0.4, Tf::Pq));
    }

    #[test]
    fn hlg_peak_is_relative_1000_nits() {
        let peak = lin_chan(1.0, Tf::Hlg);
        assert!((peak - 1000.0 / 203.0).abs() < 0.05, "peak was {peak}");
    }

    #[test]
    fn srgb_roundtrips() {
        for i in 0..=10 {
            let v = i as f32 / 10.0;
            assert!((srgb_oetf(srgb_eotf(v)) - v).abs() < 1e-4);
        }
    }

    #[test]
    fn lut_matches_direct_within_tolerance() {
        let lut = TfLut::new(Tf::Pq);
        for i in 0..=100 {
            let v = i as f32 / 100.0;
            let (a, b) = (lut.apply(v), lin_chan(v, Tf::Pq));
            assert!((a - b).abs() < 0.05, "at {v}: lut {a} vs direct {b}");
        }
    }

    #[test]
    fn primaries_conversions_preserve_neutral() {
        // A neutral grey must stay neutral through either matrix.
        for f in [bt2020_to_bt709, p3_to_bt709] {
            let out = f([0.5, 0.5, 0.5]);
            for c in out {
                assert!((c - 0.5).abs() < 1e-3, "neutral drifted: {out:?}");
            }
        }
    }

    #[test]
    fn reinhard_compresses_highlights_but_keeps_shadows() {
        let dark = tonemap_reinhard([0.1, 0.1, 0.1]);
        assert!((dark[0] - 0.1).abs() < 0.02, "shadows moved: {dark:?}");
        let bright = tonemap_reinhard([8.0, 8.0, 8.0]);
        assert!(
            bright[0] < 8.0 && bright[0] > 1.0,
            "highlight was {}",
            bright[0]
        );
    }

    #[test]
    fn hlg_reference_white_is_reference_white() {
        // BT.2408: 75% HLG signal is reference white on a 1000-nit display.
        let v = lin_chan(0.75, Tf::Hlg);
        assert!((v - 1.0).abs() < 0.02, "75% HLG decoded to {v}");
    }
}

#[cfg(all(test, feature = "icc"))]
mod embedded_tests {
    use super::*;
    use lcms2::{CIExyY, CIExyYTRIPLE, Profile, ToneCurve};

    /// Adobe RGB (1998) as lcms2 builds it: its primaries, D65 and a 2.2 gamma.
    fn adobe_rgb() -> Vec<u8> {
        let d65 = CIExyY { x: 0.3127, y: 0.3290, Y: 1.0 };
        let p = CIExyYTRIPLE {
            Red: CIExyY { x: 0.64, y: 0.33, Y: 1.0 },
            Green: CIExyY { x: 0.21, y: 0.71, Y: 1.0 },
            Blue: CIExyY { x: 0.15, y: 0.06, Y: 1.0 },
        };
        let g = ToneCurve::new(2.2);
        Profile::new_rgb(&d65, &p, &[&g, &g, &g]).unwrap().icc().unwrap()
    }

    #[test]
    fn a_profile_not_known_by_name_converts_through_itself() {
        let icc = adobe_rgb();
        assert_eq!(embedded(&icc), Embedded::Other);
        assert_eq!(embedded(&Profile::new_srgb().icc().unwrap()), Embedded::Srgb);
        // White, mid grey and the profile's own green, with alpha that must stay put.
        let mut px = vec![1.0, 1.0, 1.0, 1.0, 0.5, 0.5, 0.5, 0.25, 0.0, 1.0, 0.0, 1.0];
        assert!(convert_to_linear_bt2020(&icc, &mut px));
        for c in &px[0..3] {
            assert!((c - 1.0).abs() < 0.01, "white stays white: {px:?}");
        }
        assert!((px[4] - 0.5f32.powf(2.2)).abs() < 0.01, "a 2.2 gamma undone: {px:?}");
        assert_eq!(px[7], 0.25, "alpha untouched");
        // Adobe green is inside BT.2020, short of its corner: some red mixed in, still positive.
        assert!(px[9] > 0.5 && px[8] > 0.0 && px[8] < px[9], "{px:?}");
    }
}
