//! HDR image decoding and colour maths.
//!
//! Every decoder normalises into ONE representation, the invariant shared with vantaviewer,
//! vantapaper and vantalock:
//!
//!   tightly packed RGBA half-float (fp16), LINEAR, where 1.0 = 203 cd/m^2.
//!
//! Native primaries are deliberately PRESERVED rather than converted at decode time; the
//! shader converts for display. That keeps the working image gamut-correct and lets an export
//! path keep the source gamut.

#[cfg(feature = "avif")]
pub(crate) mod avif_sys;
pub mod color;
pub mod decode;
pub mod encode;

pub use color::{Primaries, Tf};
pub use decode::{decode_image, DecodeError, HdrImage, HdrKind};
pub use encode::{can_encode_extension, encode_image, EncodeError};
