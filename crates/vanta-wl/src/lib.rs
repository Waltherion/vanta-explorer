//! Wayland client helpers shared by the vanta apps.
//!
//! The re-exports pin one wayland stack for every app so the `Dispatch` impls, protocol types
//! and sctk states all agree on versions. `wayland-client` is built with the `system` feature
//! (libwayland backend) on purpose: Vulkan's WSI needs a real `wl_display*`/`wl_surface*`, and
//! only the system backend exposes them (`Backend::display_ptr`, `ObjectId::as_ptr`).

#[cfg(feature = "toolkit")]
pub use smithay_client_toolkit as sctk;
pub use wayland_client;
#[cfg(feature = "toolkit")]
pub use wayland_protocols;

pub mod dnd;
#[cfg(feature = "toolkit")]
pub mod input;
pub mod region;
#[cfg(feature = "toolkit")]
pub mod scale;
