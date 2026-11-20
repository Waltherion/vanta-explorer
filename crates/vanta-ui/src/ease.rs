//! Easing curves written the way Hyprland writes them: a cubic bezier from (0, 0) to (1, 1) given
//! by its two middle points, and a speed in tenths of a second.
//!
//! The point of copying Hyprland rather than inventing a curve is that the bar then moves the way
//! the windows around it move. A curve copied out of `hyprland.conf` (or `hyprland.lua`) behaves
//! the same here: the curve is baked into the same number of points and read back the same way,
//! by finding where along it x has got to and taking y there.
//!
//! A middle point's y may lie outside 0 to 1. Above 1, the motion runs past its end and comes
//! back, which is what a bounce is.

use std::fmt;
use std::sync::Arc;

/// How many points the curve is baked into, as in Hyprland.
const BAKED: usize = 255;

/// A cubic bezier from (0, 0) through two control points to (1, 1).
#[derive(Clone)]
pub struct Bezier {
    points: [f32; 4],
    /// (x, y) at evenly spaced t, the last one at t = 1.
    baked: Arc<[(f32, f32)]>,
}

impl Bezier {
    /// The two middle points, `(x1, y1)` and `(x2, y2)`. The x values are kept within 0 to 1, which
    /// is what keeps time running forward along the curve.
    pub fn new(x1: f32, y1: f32, x2: f32, y2: f32) -> Bezier {
        let (x1, x2) = (x1.clamp(0.0, 1.0), x2.clamp(0.0, 1.0));
        let at = |t: f32, a: f32, b: f32| {
            let u = 1.0 - t;
            3.0 * u * u * t * a + 3.0 * u * t * t * b + t * t * t
        };
        let baked = (1..=BAKED)
            .map(|i| {
                let t = i as f32 / BAKED as f32;
                (at(t, x1, x2), at(t, y1, y2))
            })
            .collect();
        Bezier { points: [x1, y1, x2, y2], baked }
    }

    /// Straight through: y is x.
    pub fn linear() -> Bezier {
        Bezier::new(0.0, 0.0, 1.0, 1.0)
    }

    /// Hyprland's `crazyshot`: most of the way in the first tenth of the time, a little past the
    /// end, and back.
    pub fn crazyshot() -> Bezier {
        Bezier::new(0.1, 1.5, 0.76, 0.92)
    }

    pub fn points(&self) -> [f32; 4] {
        self.points
    }

    /// How far along the motion is when `x` of its time has passed, 0 to 1 (and past 1 where
    /// the curve overshoots).
    pub fn y_at(&self, x: f32) -> f32 {
        if x >= 1.0 {
            return 1.0;
        }
        if x <= 0.0 {
            return 0.0;
        }
        // The first baked point at or past x, and the one before it (the origin before the first).
        let i = self.baked.partition_point(|p| p.0 < x);
        let (x1, y1) = self.baked.get(i).copied().unwrap_or((1.0, 1.0));
        let (x0, y0) = if i == 0 { (0.0, 0.0) } else { self.baked[i - 1] };
        if x1 - x0 <= f32::EPSILON {
            return y1;
        }
        y0 + (y1 - y0) * (x - x0) / (x1 - x0)
    }

    /// The furthest the curve goes, at least 1: how far past its end a motion along it runs.
    pub fn peak(&self) -> f32 {
        self.baked.iter().map(|p| p.1).fold(1.0, f32::max)
    }
}

impl PartialEq for Bezier {
    fn eq(&self, other: &Self) -> bool {
        self.points == other.points
    }
}

impl fmt::Debug for Bezier {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let [a, b, c, d] = self.points;
        write!(f, "Bezier({a}, {b}, {c}, {d})")
    }
}

/// A curve and how long a motion along it takes.
#[derive(Clone, Debug, PartialEq)]
pub struct Motion {
    pub curve: Bezier,
    /// In seconds. Zero means no motion at all: whatever moves is simply where it is going.
    pub duration: f32,
}

impl Motion {
    /// From Hyprland's speed, which is in tenths of a second.
    pub fn from_speed(curve: Bezier, speed: f32) -> Motion {
        Motion { curve, duration: (speed.max(0.0)) / 10.0 }
    }

    /// Hyprland's `crazyshot` at speed 2.
    pub fn crazyshot() -> Motion {
        Motion::from_speed(Bezier::crazyshot(), 2.0)
    }

    /// Nothing moves.
    pub fn instant() -> Motion {
        Motion { curve: Bezier::linear(), duration: 0.0 }
    }

    /// How far a linear `t` (0 to 1) has got along the curve.
    pub fn at(&self, t: f32) -> f32 {
        if self.duration <= 0.0 {
            return 1.0;
        }
        self.curve.y_at(t)
    }

    /// How much of the way `dt` seconds moves a linear `t`. All of it when nothing moves.
    pub fn step(&self, dt: f32) -> f32 {
        if self.duration <= 0.0 {
            1.0
        } else {
            dt / self.duration
        }
    }
}

impl Default for Motion {
    fn default() -> Self {
        Motion::crazyshot()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_curve_starts_at_nothing_and_ends_at_all_of_it() {
        for b in [Bezier::linear(), Bezier::crazyshot(), Bezier::new(0.05, 0.9, 0.1, 1.05)] {
            assert_eq!(b.y_at(0.0), 0.0);
            assert_eq!(b.y_at(1.0), 1.0);
            assert!((b.y_at(0.9999) - 1.0).abs() < 0.02, "{b:?} lands where it ends");
        }
    }

    #[test]
    fn the_linear_curve_is_the_identity() {
        let b = Bezier::linear();
        for i in 0..=20 {
            let x = i as f32 / 20.0;
            assert!((b.y_at(x) - x).abs() < 0.005, "at {x}: {}", b.y_at(x));
        }
    }

    #[test]
    fn crazyshot_runs_a_little_past_the_end_and_comes_back() {
        let b = Bezier::crazyshot();
        let peak = b.peak();
        assert!(peak > 1.03 && peak < 1.06, "peak {peak}");
        // Most of the way almost at once: that is what makes it feel quick.
        assert!(b.y_at(0.1) > 0.55, "{}", b.y_at(0.1));
        assert!(b.y_at(0.6) > 1.0, "past the end in the middle");
    }

    #[test]
    fn a_curve_without_overshoot_peaks_at_one() {
        assert_eq!(Bezier::new(0.16, 1.0, 0.3, 1.0).peak(), 1.0);
    }

    #[test]
    fn speed_is_in_tenths_of_a_second() {
        assert!((Motion::crazyshot().duration - 0.2).abs() < 1e-6);
        assert_eq!(Motion::instant().at(0.0), 1.0, "nothing moves: it is already there");
        assert_eq!(Motion::instant().step(0.001), 1.0);
    }
}
