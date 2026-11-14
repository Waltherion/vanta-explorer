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
