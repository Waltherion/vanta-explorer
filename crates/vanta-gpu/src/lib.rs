//! Raw ash Vulkan building blocks shared by the vanta apps.
//!
//! Conventions (shared by every app in the workspace):
//! - HDR output = `R16G16B16A16_SFLOAT` + `EXTENDED_SRGB_LINEAR_EXT` (scRGB, 1.0 = 80 nits);
//!   SDR output = an `SRGB_NONLINEAR` format. On NVIDIA the WSI tags the wl_surface itself,
//!   so nothing here talks to wp-color-management.
//! - Every GPU wait is bounded by [`GPU_WAIT_NS`]. A timeout drops the frame; it never blocks
//!   the caller's event loop (a hidden/occluded/DPMS-off surface looks exactly like that).
//! - Swapchain rebuilds hand the old chain over via `old_swapchain`, which keeps NVIDIA from
//!   requesting a second `wp_fifo_v1` on the same surface (Hyprland kills the connection).

pub mod buffer;
pub mod chain;
pub mod gpu;
pub mod readback;
pub mod shared;
pub mod texture;
pub mod util;

pub use ash;
pub use ash::vk;
pub use buffer::MappedBuffer;
pub use chain::{Frame, SurfaceChain};
pub use gpu::Gpu;
pub use texture::Texture;

/// The one number that must never be `u64::MAX`: 100 ms.
pub const GPU_WAIT_NS: u64 = 100_000_000;

/// scRGB scale so that fp16 content where 1.0 = 203 nits lands at 203 nits (scRGB 1.0 = 80).
pub const CONTENT_SCALE: f32 = 203.0 / 80.0;
