//! What the vanta apps share above the pixels: one rolling colour band, the theme colour slots
//! they all read, the font they all load, and the scale their sizes derive from.
//!
//! The band is the reason this crate exists. Every surface that draws part of it has to sample
//! the same function at the same coordinates, or the colours stop lining up where two surfaces
//! meet. Keeping one implementation here is what makes that possible.

pub mod band;
pub mod ease;
pub mod font;
pub mod style;
pub mod theme;

pub use band::{phase_at, Band};
pub use font::{context_with_font, ICONS};
pub use style::Style;
pub use theme::{parse_theme_color, Colors, Font};
