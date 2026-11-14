//! Per-output colour state, polled from Hyprland (`hyprctl monitors -j`, asked over its socket).
//!
//! Beyond the HDR on/off flag this also reads the numbers Hyprland applies to *untagged* SDR
//! content in HDR mode. Vanta Explorer's surface is tagged scRGB and therefore bypasses them, so to sit
//! level with kitty/Dolphin it has to reproduce them itself:
//!
//!   sdrMaxLuminance = 80   -> SDR white references 80 nits (the scRGB 1.0 point)
//!   sdrBrightness   = 5    -> e.g. untagged SDR white is lifted to 5 x 80 = 400 nits
//!   sdrSaturation   = 1.2  -> e.g. untagged SDR colour is saturated by 1.2
//!
//! So the chrome's scRGB multiplier is simply sdrBrightness * sdrMaxLuminance / 80.
//!
//! The other vanta apps read the same fields through `vanta_config::hypr`. This is read-only;
//! nothing here writes Hyprland settings.

use std::sync::mpsc::{channel, Receiver};
use std::sync::{Arc, Mutex};
use std::time::Duration;

const POLL_INTERVAL: Duration = Duration::from_secs(2);
/// windows-scRGB defines 1.0 as 80 nits.
const SCRGB_REFERENCE_NITS: f32 = 80.0;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct MonitorColor {
    pub hdr: bool,
    pub sdr_brightness: f32,
    pub sdr_saturation: f32,
    pub sdr_max_luminance: f32,
}

impl Default for MonitorColor {
    fn default() -> Self {
        // Default to SDR when unknown: an sRGB surface displays correctly on any monitor,
        // whereas an scRGB surface on an SDR monitor washes out.
        Self {
            hdr: false,
            sdr_brightness: 1.0,
            sdr_saturation: 1.0,
            sdr_max_luminance: SCRGB_REFERENCE_NITS,
        }
    }
}

impl MonitorColor {
    /// The scRGB multiplier that puts our UI white where Hyprland puts everyone else's.
    pub fn desktop_ui_scale(&self) -> f32 {
        let reference = if self.sdr_max_luminance > 0.0 {
            self.sdr_max_luminance
        } else {
            SCRGB_REFERENCE_NITS
        };
        self.sdr_brightness * reference / SCRGB_REFERENCE_NITS
    }
}

/// Polls Hyprland's monitors on a background thread.
///
/// Kept off the event loop even though the socket answers quickly: a compositor that is slow
/// to answer must not hold up a frame.
pub struct HdrMonitor {
    color: MonitorColor,
    rx: Receiver<MonitorColor>,
    output: Arc<Mutex<Option<String>>>,
}

impl HdrMonitor {
    pub fn new() -> Self {
        let (tx, rx) = channel();
        let output: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
        let thread_output = Arc::clone(&output);
        std::thread::Builder::new()
            .name("vx-hdr".into())
            .spawn(move || {
                let mut last: Option<MonitorColor> = None;
                loop {
                    let name = thread_output.lock().ok().and_then(|g| g.clone());
                    let next = query(name.as_deref()).unwrap_or_default();
                    if last != Some(next) {
                        last = Some(next);
                        if tx.send(next).is_err() {
                            return; // main thread gone
                        }
                    }
                    std::thread::sleep(POLL_INTERVAL);
                }
            })
            .expect("spawn hdr monitor thread");
        Self {
            color: MonitorColor::default(),
            rx,
            output,
        }
    }

    pub fn is_hdr(&self) -> bool {
        self.color.hdr
    }

    pub fn color(&self) -> MonitorColor {
        self.color
    }

    /// Tell the poller which output the window sits on.
    pub fn set_output(&self, name: Option<String>) {
        if let Ok(mut g) = self.output.lock() {
            if *g != name {
                *g = name;
            }
        }
    }

    /// Non-blocking: returns the new state when the poller reported a change.
    pub fn poll(&mut self) -> Option<MonitorColor> {
        let mut latest = None;
        while let Ok(v) = self.rx.try_recv() {
            latest = Some(v);
        }
        if let Some(v) = latest {
            self.color = v;
        }
        latest
    }

    /// Blocking one-shot for startup, before the window exists.
    pub fn query_now(&mut self, output: Option<&str>) -> MonitorColor {
        let v = query(output).unwrap_or_default();
        self.color = v;
        v
    }
}

fn query(output: Option<&str>) -> Option<MonitorColor> {
    // Over Hyprland's socket, which answers in a fraction of a millisecond, rather than by
    // starting `hyprctl` every couple of seconds for as long as the window is open.
    let json = vanta_config::hypr::request_json("j/monitors", Duration::from_secs(2))?;
    let monitors = json.as_array()?;

    let m = monitors.iter().find(|m| match output {
        Some(name) => m.get("name").and_then(|v| v.as_str()) == Some(name),
        None => m.get("focused").and_then(|v| v.as_bool()).unwrap_or(false),
    })?;

    let num = |key: &str, fallback: f32| {
        m.get(key)
            .and_then(|v| v.as_f64())
            .map(|v| v as f32)
            .unwrap_or(fallback)
    };

    Some(MonitorColor {
        hdr: m.get("colorManagementPreset").and_then(|v| v.as_str()).is_some_and(|p| p.starts_with("hdr")),
        sdr_brightness: num("sdrBrightness", 1.0),
        sdr_saturation: num("sdrSaturation", 1.0),
        sdr_max_luminance: num("sdrMaxLuminance", SCRGB_REFERENCE_NITS),
    })
}
