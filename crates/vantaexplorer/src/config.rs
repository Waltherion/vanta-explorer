//! Explorer's settings: `~/.config/vanta/explorer.jsonc` over the shared palette in
//! `colors.jsonc`, with `explorer.local.jsonc` over both (see `vanta_config::layers`). The
//! appearance comes out as a `Config`, the behaviour (date format, terminal, icon themes) as
//! `settings::Settings`; both are read from the same layered files and reloaded together.
//!
//! Reloading polls the files rather than watching them, which is deliberate: theme switchers
//! commonly work by moving a symlink, and an inotify watch dies silently on the old target when
//! that happens (see `vanta_config::watch`).

use std::time::{Duration, Instant};

use serde::Deserialize;
use vanta_config::hypr::SdrWhite;
use vanta_config::palette::Palette;
use vanta_config::{layers, Color};

use crate::settings::Settings;

const POLL_INTERVAL: Duration = Duration::from_millis(500);

pub const DEFAULT_CONFIG: &str = r##"// Vanta Explorer (JSONC: comments and trailing commas are fine).
//
// Colours and font come from colors.jsonc, shared by every Vanta app. Written here, they apply
// to Explorer alone. Behaviour (date format, terminal, icon themes) belongs to this machine and
// is in explorer.local.jsonc. Where the interface puts SDR white on an HDR screen is shared by
// every Vanta app, in display.jsonc. Changes are picked up within a second, without a restart.
//
// Lines starting with // are options left at their default: remove the // to use one.
{
  // "color": {
  //   // Window background: "#rrggbb", or "#aarrggbb" where the alpha makes the window
  //   // translucent and the compositor blurs what is behind it. Content and text stay opaque.
  //   "background": "#d1000000",
  //   "text": "#bd97f7",
  //   "accent": "#bd97f7",
  //   // The folders in Explorer's own icons. Unset: the accent.
  //   "folder": "#bd97f7",
  // },
  // Resolved through fontconfig; "sans-serif" asks for the system's own.
  // "font": {
  //   "family": "Poppins",
  //   "size": 15,
  // },
}
"##;

/// Written on first start next to it.
pub const DEFAULT_LOCAL: &str = r##"// Vanta Explorer, this machine only: kept when the theme changes.
//
// Lines starting with // are options left at their default: remove the // to use one.
{
  // Date/time format for the Modified column, strftime-style. For example:
  //   "%Y-%m-%d %H:%M"  ->  2026-08-22 21:30  (ISO 8601)
  //   "%d-%m-%Y %H:%M"  ->  22-08-2026 21:30
  //   "%m/%d/%Y %I:%M %p" -> 08/22/2026 09:30 PM
  // Always an absolute date, no "3 d ago".
  // "dateFormat": "%Y-%m-%d %H:%M",

  // "Open Terminal Here" (context menu, or F4). The program, and the arguments it is
  // started with; "{dir}" is replaced by the folder. Terminals disagree on the flag:
  //   kitty      ["--directory", "{dir}"]
  //   foot       ["--working-directory", "{dir}"]
  //   alacritty  ["--working-directory", "{dir}"]
  //   wezterm    ["start", "--cwd", "{dir}"]
  // The process is also started WITH that folder as its working directory, so a terminal
  // that takes no flag at all works with "terminalArgs": [].
  // Empty: $TERMINAL, else the first known terminal installed, with its own flag.
  // "terminal": "",
  // "terminalArgs": [],

  // Icon themes to look in, best first. Empty: Explorer's own icons (Breeze, with folders in
  // the accent or color.folder), then the desktop's theme for anything they lack.
  // "desktop" stands for the desktop's own theme (GTK, KDE or GNOME setting), followed when it
  // changes: ["desktop"] uses the desktop's icons only. Name a theme yourself and the last one
  // should stay "hicolor", which every desktop ships. A theme that is repainted in place (some
  // desktops recolor their icons when the look changes) is picked up live.
  // "iconThemes": [],

  // A copy keeps the original's modification date, as cp -a, Dolphin and Nautilus do, so
  // sorting by date still says when a photo was taken. false: a copy is dated when it was
  // made. A move always keeps the date.
  // "keepDates": true,
}
"##;

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct Font {
    pub family: String,
    pub size: f32,
}

impl Default for Font {
    fn default() -> Self {
        Self {
            family: "Poppins".into(),
            size: 15.0,
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
#[serde(rename_all = "camelCase")]
pub struct Ui {
    /// Follow Hyprland's sdrBrightness/sdrSaturation instead of the fixed values below.
    pub match_desktop: bool,
    pub nits: f32,
    pub saturation: f32,
}

impl Default for Ui {
    fn default() -> Self {
        Self {
            match_desktop: true,
            nits: 203.0,
            saturation: 1.0,
        }
    }
}

/// The appearance, colours as `#aarrggbb`.
#[derive(Debug, Clone)]
pub struct Config {
    pub background: String,
    pub text: String,
    pub accent: String,
    /// `color.folder`, else the accent: the folders in Explorer's own icons.
    pub folder: String,
    pub font: Font,
    pub ui: Ui,
}

/// Explorer's own starting point, as the Vanta theme has it: the shared palette on a black window
/// that lets a little of the desktop through (82 %, which the compositor blurs), in a slightly
/// smaller type.
fn default_palette() -> Palette {
    let mut p = Palette::default();
    p.color.background = Color::rgba(0, 0, 0, 0xd1);
    p.font.size = 15.0;
    p
}

/// `#aarrggbb`, the form `parse_hex` reads back.
fn hex(c: Color) -> String {
    format!("#{:02x}{:02x}{:02x}{:02x}", c.a, c.r, c.g, c.b)
}

impl Default for Config {
    fn default() -> Self {
        Config::from_value(&serde_json::Value::Null)
    }
}

impl Config {
    /// The appearance from the layered files, defaults for whatever they leave out.
    pub fn from_value(root: &serde_json::Value) -> Self {
        let mut p = default_palette();
        p.apply(root);
        let ui = match layers::sdr_white_for(root) {
            SdrWhite::Desktop => Ui { match_desktop: true, ..Ui::default() },
            SdrWhite::Nits(n) => Ui { match_desktop: false, nits: n, saturation: 1.0 },
        };
        Config {
            background: hex(p.color.background),
            text: hex(p.color.text),
            accent: hex(p.color.accent),
            folder: root
                .pointer("/color/folder")
                .and_then(|v| v.as_str())
                .and_then(Color::parse)
                .map_or_else(|| hex(p.color.accent), hex),
            font: Font { family: p.font.family, size: p.font.size },
            ui,
        }
    }

    /// Straight (non-premultiplied) linear RGB plus alpha.
    pub fn background_linear(&self) -> [f32; 4] {
        parse_hex(&self.background).unwrap_or([0.0, 0.0, 0.0, 1.0])
    }

    /// windows-scRGB has 1.0 = 80 nits, so UI white at `nits` needs this multiplier.
    pub fn ui_scale_hdr(&self) -> f32 {
        self.ui.nits / 80.0
    }
}

/// Appearance and behaviour, from the files as they are now. Written with their commented
/// defaults on first run.
pub fn load() -> (Config, Settings) {
    layers::seed_app("explorer", DEFAULT_CONFIG, DEFAULT_LOCAL);
    let loaded = layers::load("explorer");
    for e in &loaded.errors {
        log::warn!("{e}");
    }
    (Config::from_value(&loaded.value), Settings::from_value(&loaded.value))
}

/// Notices a change to any of the files (a theme switch included) and reads
/// them again.
pub struct Watcher {
    last_check: Instant,
    stamp: vanta_config::watch::Stamp,
}

impl Watcher {
    pub fn new() -> Self {
        Self { last_check: Instant::now(), stamp: layers::stamp("explorer") }
    }

    pub fn poll(&mut self) -> Option<(Config, Settings)> {
        if self.last_check.elapsed() < POLL_INTERVAL {
            return None;
        }
        self.last_check = Instant::now();
        if !self.stamp.changed() {
            return None;
        }
        log::info!("config reloaded");
        Some(load())
    }
}

/// "#rrggbb" / "#aarrggbb" -> straight linear RGBA.
fn parse_hex(s: &str) -> Option<[f32; 4]> {
    let h = s.trim().strip_prefix('#')?;
    let (a, r, g, b) = match h.len() {
        6 => (
            255u8,
            u8::from_str_radix(&h[0..2], 16).ok()?,
            u8::from_str_radix(&h[2..4], 16).ok()?,
            u8::from_str_radix(&h[4..6], 16).ok()?,
        ),
        8 => (
            u8::from_str_radix(&h[0..2], 16).ok()?,
            u8::from_str_radix(&h[2..4], 16).ok()?,
            u8::from_str_radix(&h[4..6], 16).ok()?,
            u8::from_str_radix(&h[6..8], 16).ok()?,
        ),
        _ => return None,
    };
    Some([
        srgb_to_linear(r),
        srgb_to_linear(g),
        srgb_to_linear(b),
        a as f32 / 255.0,
    ])
}

fn srgb_to_linear(v: u8) -> f32 {
    let c = v as f32 / 255.0;
    if c <= 0.04045 {
        c / 12.92
    } else {
        ((c + 0.055) / 1.055).powf(2.4)
    }
}

/// Same formula as applySaturation() in composite.frag. The background is a clear value
/// rather than a sampled texture, so it needs the CPU-side equivalent, otherwise the chrome
/// and the backdrop drift apart as soon as ui.saturation leaves 1.0.
pub fn apply_saturation(rgb: [f32; 3], s: f32) -> [f32; 3] {
    let l = 0.2126 * rgb[0] + 0.7152 * rgb[1] + 0.0722 * rgb[2];
    [
        l + (rgb[0] - l) * s,
        l + (rgb[1] - l) * s,
        l + (rgb[2] - l) * s,
    ]
}

/// sRGB OETF, for when the swapchain is a plain UNORM and we must encode ourselves.
pub fn linear_to_srgb(c: f32) -> f32 {
    if c <= 0.0031308 {
        12.92 * c
    } else {
        1.055 * c.max(0.0).powf(1.0 / 2.4) - 0.055
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The defaults are written on first run, so if they do not parse, the app is broken for
    /// everyone who has never configured it. That is the one config failure nobody can work
    /// around, and it would only show up on someone else's machine.
    #[test]
    fn default_config_parses() {
        let mut v = vanta_config::jsonc::parse(&vanta_config::jsonc::uncommented(DEFAULT_CONFIG)).expect("DEFAULT_CONFIG must parse; first run writes it verbatim");
        layers::merge(&mut v, vanta_config::jsonc::parse(&vanta_config::jsonc::uncommented(DEFAULT_LOCAL)).expect("DEFAULT_LOCAL must parse"));
        let cfg = Config::from_value(&v);
        let bg = cfg.background_linear();
        assert_eq!(&bg[..3], &[0.0, 0.0, 0.0], "black by default");
        assert!((bg[3] - 0xd1 as f32 / 255.0).abs() < 1e-6, "and translucent, as the Vanta theme has it");
        assert!(cfg.font.size > 0.0);
        assert_eq!(Settings::from_value(&v).date_format, Settings::default().date_format);
    }

    /// Colours are hand-edited, so a typo must degrade rather than crash.
    #[test]
    fn bad_colour_falls_back() {
        let v = vanta_config::jsonc::parse(r#"{ "color": { "background": "not a colour" } }"#).unwrap();
        let bg = Config::from_value(&v).background_linear();
        assert!(bg.iter().all(|c| c.is_finite()));
    }

    #[test]
    fn a_translucent_background_keeps_its_alpha() {
        let v = vanta_config::jsonc::parse(r##"{ "color": { "background": "#cc101014" } }"##).unwrap();
        let bg = Config::from_value(&v).background_linear();
        assert!((bg[3] - 0.8).abs() < 0.01);
    }

    /// `docs/config.md` shows the files this writes on first run word for word; a change here
    /// has to be made there too.
    #[test]
    fn the_reference_shows_what_is_written_on_first_run() {
        let doc = include_str!("../../../docs/config.md");
        assert!(doc.contains(DEFAULT_CONFIG.trim_end()), "docs/config.md no longer shows DEFAULT_CONFIG as the code writes it");
        assert!(doc.contains(DEFAULT_LOCAL.trim_end()), "docs/config.md no longer shows DEFAULT_LOCAL as the code writes it");
    }

    /// The files written on first run set nothing: every option in them is commented out with
    /// its built-in default. Turned on all together they must give exactly what Explorer has
    /// with nothing set, or a file would show a default it does not have.
    #[test]
    fn every_commented_option_is_the_default() {
        let on = |t: &str| vanta_config::jsonc::parse(&vanta_config::jsonc::uncommented(t)).unwrap();
        let mut v = on(DEFAULT_CONFIG);
        layers::merge(&mut v, on(DEFAULT_LOCAL));
        assert_eq!(format!("{:?}", Config::from_value(&v)), format!("{:?}", Config::from_value(&serde_json::json!({}))));
    }
}
