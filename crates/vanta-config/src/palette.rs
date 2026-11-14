//! The palette every vanta app paints with: colours, font, the rainbow band and the shape of
//! things, as `colors.jsonc` (and any app file overriding it) writes them.
//!
//! ```jsonc
//! {
//!   "color": { "background": "#80000000", "text": "#ffffff", "accent": "#ffffff",
//!              "border": "#ffffff", "highlight": "#ffffff", "error": "#ff5555" },
//!   "font": { "family": "Poppins", "size": 16, "weight": 400 },
//!   "rainbow": { "enabled": false, "stops": ["#ff0000", "#00ff00"], "period": 3000,
//!                "speed": 3, "brightness": 1 },
//!   "shape": { "cornerRadius": 0, "borderWidth": 1 }
//! }
//! ```
//!
//! Colours are `#RRGGBB`, or `#AARRGGBB` with the alpha first. Every app has its own defaults for
//! all of it; a key the files leave out keeps the app's default.

use serde_json::Value;

use crate::{Color, ValueExt};

#[derive(Clone, Debug, PartialEq)]
pub struct Font {
    pub family: String,
    pub size: f32,
    pub weight: i32,
}

/// The slots every vanta interface paints with.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Colors {
    pub background: Color,
    pub text: Color,
    pub accent: Color,
    pub border: Color,
    pub highlight: Color,
    /// A wrong password, a failed operation.
    pub error: Color,
}

/// The colour band that runs through text and borders when a theme turns it on.
#[derive(Clone, Debug, PartialEq)]
pub struct Rainbow {
    pub enabled: bool,
    pub stops: Vec<Color>,
    /// Length of one cycle of the band, in logical pixels; 0 lets the app pick.
    pub period: f32,
    /// How fast the band rolls; 0 holds it still.
    pub speed: f32,
    /// Multiplier on the band's colours.
    pub brightness: f32,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Shape {
    pub corner_radius: f32,
    pub border_width: f32,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Palette {
    pub color: Colors,
    pub font: Font,
    pub rainbow: Rainbow,
    pub shape: Shape,
}

/// A colour as the vanta files write it: `#RRGGBB`, or `#AARRGGBB` with the alpha first.
/// (`Color::parse` reads the other order, `RRGGBBAA`, used by Hyprland's `rgba(...)`.)
pub fn parse_color(text: &str) -> Option<Color> {
    let h = text.trim().trim_start_matches('#');
    match h.len() {
        6 => Color::parse(h),
        8 => {
            let v = u32::from_str_radix(h, 16).ok()?;
            Some(Color::rgba((v >> 16) as u8, (v >> 8) as u8, v as u8, (v >> 24) as u8))
        }
        _ => None,
    }
}

impl Colors {
    /// Takes the slots `obj` names; the others keep what they were.
    pub fn apply(&mut self, obj: &Value) {
        for (key, slot) in [
            ("background", &mut self.background),
            ("text", &mut self.text),
            ("accent", &mut self.accent),
            ("border", &mut self.border),
            ("highlight", &mut self.highlight),
            ("error", &mut self.error),
        ] {
            if let Some(c) = obj.get(key).and_then(|v| v.as_str()).and_then(parse_color) {
                *slot = c;
            }
        }
    }
}

impl Font {
    pub fn apply(&mut self, obj: &Value) {
        self.family = obj.str_or("family", &self.family);
        self.size = obj.f32_or("size", self.size);
        self.weight = obj.i32_or("weight", self.weight);
    }
}

impl Rainbow {
    pub fn apply(&mut self, obj: &Value) {
        self.enabled = obj.bool_or("enabled", self.enabled);
        if let Some(list) = obj.get("stops").and_then(|v| v.as_array()) {
            let parsed: Vec<Color> = list.iter().filter_map(|v| v.as_str()).filter_map(parse_color).collect();
            if !parsed.is_empty() {
                self.stops = parsed;
            }
        }
        self.period = obj.f32_or("period", self.period);
        self.speed = obj.f32_or("speed", self.speed);
        self.brightness = obj.f32_or("brightness", self.brightness);
    }

    /// Whether the band is on and has colours to show.
    pub fn active(&self) -> bool {
        self.enabled && !self.stops.is_empty()
    }
}

impl Shape {
    pub fn apply(&mut self, obj: &Value) {
        self.corner_radius = obj.f32_or("cornerRadius", self.corner_radius);
        self.border_width = obj.f32_or("borderWidth", self.border_width);
    }
}

impl Palette {
    /// Takes the four sections from the layered settings.
    pub fn apply(&mut self, root: &Value) {
        self.color.apply(root.section("color"));
        self.font.apply(root.section("font"));
        self.rainbow.apply(root.section("rainbow"));
        self.shape.apply(root.section("shape"));
    }
}

/// The Vanta theme's own stops, violet at the foot to pink at the crown. The band is off by
/// default; they are the colours an app reaches for when something is worth marking.
pub const STOPS: [&str; 4] = ["#6a2cc4", "#a23be0", "#e04fd0", "#ff7ab8"];

impl Default for Palette {
    /// The Vanta theme: true black, lavender ink, and colour only where something happens.
    fn default() -> Self {
        let ink = Color::rgba(0xbd, 0x97, 0xf7, 255);
        Palette {
            color: Colors {
                background: Color::rgba(0, 0, 0, 0x80),
                text: ink,
                accent: ink,
                border: ink,
                highlight: ink,
                error: Color::rgba(0xff, 0x3d, 0x7f, 255),
            },
            font: Font { family: "Poppins".into(), size: 16.0, weight: 300 },
            rainbow: Rainbow {
                enabled: false,
                stops: STOPS.iter().filter_map(|s| parse_color(s)).collect(),
                period: 3000.0,
                speed: 3.0,
                brightness: 1.0,
            },
            shape: Shape { corner_radius: 24.0, border_width: 0.0 },
        }
    }
}

/// The palette a new user finds in `colors.jsonc` on first start: every setting shown, all of
/// it commented out, with the Vanta look's own values. Left out, each app keeps its own defaults
/// (the bar's pill is opaque where the organizer's board is not, and so on), so nothing changes
/// until a line is turned on.
pub const DEFAULT_TEXT: &str = r##"// The palette every Vanta app paints with (JSONC: comments and trailing commas are fine).
// A theme usually makes this file a link into its own folder. Any app's own file can override
// any of it for that app alone. Colours are "#RRGGBB" or "#AARRGGBB" (alpha first).
//
// Everything below is commented out: as long as it is, each app uses its own built-in look.
// Turn on what you want every app to share.
{
  // "color": {
  //   "background": "#80000000", // panels and the bar's pill
  //   "text": "#bd97f7",
  //   "accent": "#bd97f7",       // outlines, active marks, the password field
  //   "border": "#bd97f7",
  //   "highlight": "#bd97f7",    // hover and focus
  //   "error": "#ff3d7f",        // a wrong password, a failed operation
  // },
  // "font": {
  //   "family": "Poppins", // any installed family; resolved through fontconfig
  //   "size": 16,
  //   "weight": 300,
  // },
  // // A band of colours running through text and outlines.
  // "rainbow": {
  //   "enabled": true,
  //   "stops": ["#6a2cc4", "#a23be0", "#e04fd0", "#ff7ab8"], // two or more
  //   "period": 3000, // pixels of desktop per trip through the stops
  //   "speed": 3,     // how fast it rolls; 0 holds it still, negative runs it backwards
  // },
  // "shape": {
  //   "cornerRadius": 24,
  //   "borderWidth": 0,
  // },
}
"##;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_seeded_palette_changes_nothing_until_a_line_is_turned_on() {
        let v = crate::jsonc::parse(DEFAULT_TEXT).unwrap();
        assert_eq!(v, serde_json::json!({}));
        // And each commented line, turned on, is valid.
        let on: String = DEFAULT_TEXT
            .lines()
            .map(|l| match l.trim_start().strip_prefix("//").map(str::trim_start) {
                Some(r) if r.starts_with('"') || r.starts_with('}') => r,
                _ => l,
            })
            .collect::<Vec<_>>()
            .join("\n");
        let v = crate::jsonc::parse(&on).unwrap();
        let mut p = Palette::default();
        p.apply(&v);
        assert!(p.rainbow.active() && p.rainbow.stops.len() == 4);
    }

    #[test]
    fn alpha_comes_first_in_the_eight_digit_form() {
        assert_eq!(parse_color("#80ff0000"), Some(Color::rgba(255, 0, 0, 0x80)));
        assert_eq!(parse_color("#ff0000"), Some(Color::rgba(255, 0, 0, 255)));
        assert_eq!(parse_color("nope"), None);
    }

    #[test]
    fn what_the_files_leave_out_keeps_the_default() {
        let mut p = Palette::default();
        let v = crate::jsonc::parse(r##"{ "color": { "accent": "#00ff00" }, "rainbow": { "enabled": true, "stops": ["#ff0000", "#0000ff"] }, "shape": { "cornerRadius": 12 } }"##).unwrap();
        p.apply(&v);
        assert_eq!(p.color.accent, Color::rgba(0, 255, 0, 255));
        assert_eq!(p.color.text, Palette::default().color.text);
        assert!(p.rainbow.active());
        assert_eq!(p.rainbow.stops.len(), 2);
        assert_eq!(p.rainbow.speed, 3.0);
        assert_eq!(p.shape, Shape { corner_radius: 12.0, border_width: 0.0 });
        assert_eq!(p.font.family, "Poppins");
    }

    /// `docs/config.md` shows the files this writes on first run word for word; a change here
    /// has to be made there too.
    #[test]
    fn the_reference_shows_what_is_written_on_first_run() {
        let doc = include_str!("../../../docs/config.md");
        assert!(doc.contains(DEFAULT_TEXT.trim_end()), "docs/config.md no longer shows DEFAULT_TEXT as the code writes it");
    }
}
