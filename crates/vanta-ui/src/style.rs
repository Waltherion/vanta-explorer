//! One scalar, and every size derived from it.
//!
//! Hand-written paddings are how an interface ends up with its measurements scattered across a
//! dozen files, each needing a nudge when the font changes. Here the font size decides, and
//! everything else is a ratio of it.
//!
//! The rounding is not cosmetic. At a fractional scale a logical size lands between device
//! pixels, and an edge that falls on a half pixel shimmers as the band rolls past it. Every
//! dimension is snapped so it starts and ends on a whole device pixel.

#[derive(Clone, Copy, Debug)]
pub struct Style {
    /// The font size everything is derived from, in logical pixels.
    pub size: f32,
    /// Device pixels per logical pixel, from the compositor.
    pub scale: f32,
    pub pad: f32,
    pub gap: f32,
    pub icon: f32,
    pub radius: f32,
    pub border: f32,
}

impl Style {
    pub fn new(size: f32, scale: f32, radius: f32, border: f32) -> Self {
        let scale = if scale > 0.0 { scale } else { 1.0 };
        let snap = |v: f32| (v * scale).round() / scale;
        Style {
            size,
            scale,
            pad: snap(size * 0.9),
            gap: snap(size * 0.7),
            icon: snap(size * 1.05),
            radius,
            border,
        }
    }

    /// Rounds a length so it covers a whole number of device pixels.
    pub fn snap(&self, v: f32) -> f32 {
        (v * self.scale).round() / self.scale
    }

    /// The height of a single row of text with breathing room above and below.
    pub fn row_height(&self) -> f32 {
        self.snap(self.size * 2.6 + self.border * 2.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_size_lands_on_a_whole_device_pixel() {
        let st = Style::new(15.0, 1.25, 8.0, 2.0);
        for v in [st.pad, st.gap, st.icon, st.row_height()] {
            let device = v * st.scale;
            assert!((device - device.round()).abs() < 1e-4, "{v} logical is {device} device pixels");
        }
    }

    #[test]
    fn sizes_follow_the_font() {
        let small = Style::new(12.0, 1.0, 0.0, 0.0);
        let large = Style::new(24.0, 1.0, 0.0, 0.0);
        assert!(large.pad > small.pad && large.gap > small.gap && large.row_height() > small.row_height());
    }

    #[test]
    fn a_nonsense_scale_does_not_divide_by_zero() {
        let st = Style::new(15.0, 0.0, 0.0, 0.0);
        assert_eq!(st.scale, 1.0);
        assert!(st.pad.is_finite());
    }
}
