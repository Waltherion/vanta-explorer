//! Where a surface accepts pointer input.
//!
//! A surface that reserves a strip of the screen but is taller than that strip would otherwise
//! swallow every click in the part hanging over the desktop. Setting the input region to the
//! reserved strip makes the rest click-through, so the windows underneath behave as though the
//! surface were only as big as it looks.

use wayland_client::protocol::{wl_compositor::WlCompositor, wl_region::WlRegion, wl_surface::WlSurface};
use wayland_client::{Dispatch, QueueHandle};

/// Limits pointer input to `rect`, in surface-local logical pixels. `None` restores the default,
/// which is the whole surface. Takes effect on the next commit.
pub fn set_input<S>(compositor: &WlCompositor, surface: &WlSurface, qh: &QueueHandle<S>, rect: Option<(i32, i32, i32, i32)>)
where
    S: Dispatch<WlRegion, ()> + 'static,
{
    match rect {
        Some((x, y, w, h)) => {
            let region = compositor.create_region(qh, ());
            region.add(x, y, w, h);
            surface.set_input_region(Some(&region));
            // The compositor copies the region on set; the object is ours to drop.
            region.destroy();
        }
        None => surface.set_input_region(None),
    }
}

/// Limits pointer input to several rectangles at once: a strip along the top and cards hanging
/// below it, say, with everything between them click-through. Takes effect on the next commit.
pub fn set_input_rects<S>(compositor: &WlCompositor, surface: &WlSurface, qh: &QueueHandle<S>, rects: &[(i32, i32, i32, i32)])
where
    S: Dispatch<WlRegion, ()> + 'static,
{
    let region = compositor.create_region(qh, ());
    for &(x, y, w, h) in rects {
        region.add(x, y, w, h);
    }
    surface.set_input_region(Some(&region));
    region.destroy();
}

/// Makes a surface take no pointer input at all, for something purely decorative.
pub fn set_none<S>(compositor: &WlCompositor, surface: &WlSurface, qh: &QueueHandle<S>)
where
    S: Dispatch<WlRegion, ()> + 'static,
{
    let region = compositor.create_region(qh, ());
    surface.set_input_region(Some(&region));
    region.destroy();
}
