//! Fractional scale + viewport bookkeeping for one surface.
//!
//! The lesson from vantalock: the FIRST configure must be parked until `wp_fractional_scale`
//! has reported the preferred scale, so the first swapchain is already native resolution and
//! is never rebuilt (a rebuild after a presented frame is where NVIDIA's WSI once asked
//! Hyprland for a second wp_fifo and got the connection killed). If no scale arrives within
//! [`SCALE_WAIT`], 1.0 is assumed.

use std::time::{Duration, Instant};

use wayland_client::protocol::wl_surface::WlSurface;
use wayland_client::{Dispatch, QueueHandle};
use wayland_protocols::wp::fractional_scale::v1::client::{
    wp_fractional_scale_manager_v1::WpFractionalScaleManagerV1, wp_fractional_scale_v1::WpFractionalScaleV1,
};
use wayland_protocols::wp::viewporter::client::{wp_viewport::WpViewport, wp_viewporter::WpViewporter};

pub const SCALE_WAIT: Duration = Duration::from_millis(300);

pub struct ScaledSurface {
    pub viewport: WpViewport,
    pub fractional: WpFractionalScaleV1,
    pub scale: f64,
    pub scale_known: bool,
    pending: Option<(u32, u32)>,
    pending_since: Option<Instant>,
    /// Logical size of the last realised configure.
    pub logical: (u32, u32),
}

impl Drop for ScaledSurface {
    /// The protocol objects go with the surface they belong to; otherwise every menu or overlay
    /// that comes and goes leaves two behind in the client and the compositor.
    fn drop(&mut self) {
        self.viewport.destroy();
        self.fractional.destroy();
    }
}

/// A configure that is ready to be realised.
pub struct Realize {
    pub logical: (u32, u32),
    pub buffer: (u32, u32),
    pub scale: f64,
}

impl ScaledSurface {
    /// `udata` is delivered back with the fractional-scale events, so the app's `Dispatch`
    /// impl can find the right surface (an index is the usual choice).
    pub fn new<State, U>(
        viewporter: &WpViewporter,
        frac_mgr: &WpFractionalScaleManagerV1,
        surface: &WlSurface,
        qh: &QueueHandle<State>,
        udata: U,
    ) -> Self
    where
        State: Dispatch<WpViewport, ()> + Dispatch<WpFractionalScaleV1, U> + 'static,
        U: Send + Sync + 'static,
    {
        ScaledSurface {
            viewport: viewporter.get_viewport(surface, qh, ()),
            fractional: frac_mgr.get_fractional_scale(surface, qh, udata),
            scale: 1.0,
            scale_known: false,
            pending: None,
            pending_since: None,
            logical: (0, 0),
        }
    }

    /// Pre-seeds the scale from a cheaper source (hyprctl) so that if `preferred_scale` is late
    /// the fallback is the right size instead of 1.0. A real `preferred_scale` still wins.
    pub fn seed_scale(&mut self, scale: f64) {
        if !self.scale_known && scale > 0.0 {
            self.scale = scale;
        }
    }

    /// Records a configure. Returns whether it can be realised right away.
    pub fn configure(&mut self, w: u32, h: u32) -> bool {
        self.pending = Some((w.max(1), h.max(1)));
        if self.pending_since.is_none() {
            self.pending_since = Some(Instant::now());
        }
        self.scale_known
    }

    /// Records a `preferred_scale` event (in 120ths). Returns whether a realise is due: either
    /// a parked configure is waiting, or the scale changed under an already realised surface
    /// (in which case the last logical size is re-queued so the caller rebuilds).
    pub fn preferred_scale(&mut self, scale120: u32) -> bool {
        let new = scale120 as f64 / 120.0;
        let changed = (self.scale - new).abs() > 1e-6;
        self.scale = new;
        self.scale_known = true;
        if self.pending.is_some() {
            true
        } else if changed && self.logical != (0, 0) {
            self.pending = Some(self.logical);
            true
        } else {
            false
        }
    }

    /// When [`tick`](Self::tick) next has something to do, for a loop that sleeps until then.
    pub fn deadline(&self) -> Option<Instant> {
        if self.pending.is_some() && !self.scale_known {
            self.pending_since.map(|s| s + SCALE_WAIT + Duration::from_millis(1))
        } else {
            None
        }
    }

    /// Call from the event loop's idle tick: gives up waiting for a scale after [`SCALE_WAIT`].
    /// Returns whether a realise is now due.
    pub fn tick(&mut self, now: Instant) -> bool {
        if self.pending.is_some()
            && !self.scale_known
            && self.pending_since.map_or(false, |s| now - s > SCALE_WAIT)
        {
            self.scale_known = true;
            return true;
        }
        false
    }

    /// Takes the pending configure, applies the viewport destination and returns the sizes.
    pub fn take_realize(&mut self) -> Option<Realize> {
        let (w, h) = self.pending.take()?;
        self.logical = (w, h);
        self.viewport.set_destination(w as i32, h as i32);
        let buffer = ((w as f64 * self.scale).round() as u32, (h as f64 * self.scale).round() as u32);
        Some(Realize { logical: (w, h), buffer, scale: self.scale })
    }

    pub fn has_pending(&self) -> bool {
        self.pending.is_some()
    }
}
