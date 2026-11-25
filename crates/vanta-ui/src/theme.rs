//! The palette types, from vanta-config, where every app (the ones without egui too) reads them,
//! and egui's own look made from them.

pub use vanta_config::palette::{parse_color as parse_theme_color, Colors, Font, Palette, Rainbow, Shape};

use egui::{Color32, Stroke, Visuals};

/// A palette color as egui takes it.
pub fn color32(c: vanta_config::Color) -> Color32 {
    Color32::from_rgba_unmultiplied(c.r, c.g, c.b, c.a)
}

/// Whether a palette writing in `text` is a light one: dark ink, meant for a light window. The
/// text decides rather than the background because a background may be translucent, with the
/// desktop showing through it, while the text is what the eye actually reads.
pub fn is_light(text: Color32) -> bool {
    0.2126 * text.r() as f32 + 0.7152 * text.g() as f32 + 0.0722 * text.b() as f32 <= 128.0
}

/// Relative luminance as WCAG defines it: 0 for black, 1 for white.
fn luminance(c: Color32) -> f32 {
    let linear = |v: u8| {
        let c = v as f32 / 255.0;
        if c <= 0.04045 { c / 12.92 } else { ((c + 0.055) / 1.055).powf(2.4) }
    };
    0.2126 * linear(c.r()) + 0.7152 * linear(c.g()) + 0.0722 * linear(c.b())
}

/// How far apart two colors are to the eye, from 1 (the same) to 21 (black on white).
pub fn contrast(a: Color32, b: Color32) -> f32 {
    let (la, lb) = (luminance(a), luminance(b));
    (la.max(lb) + 0.05) / (la.min(lb) + 0.05)
}

/// `a` moved toward `b` by `t`, opaque.
fn mix(a: Color32, b: Color32, t: f32) -> Color32 {
    let m = |x: u8, y: u8| (x as f32 + (y as f32 - x as f32) * t).round() as u8;
    Color32::from_rgb(m(a.r(), b.r()), m(a.g(), b.g()), m(a.b(), b.b()))
}

/// `c` made readable on the window of a light or a dark palette, by moving it toward black or
/// white until it is. A color that already reads is returned as it is, so a theme's own red
/// stays its red; only one too close to the window (a deep red on black, a pale one on white)
/// is changed.
pub fn legible(c: Color32, light: bool) -> Color32 {
    let c = Color32::from_rgb(c.r(), c.g(), c.b());
    let (toward, reads): (Color32, fn(f32) -> bool) =
        if light { (Color32::BLACK, |l| l <= 0.22) } else { (Color32::WHITE, |l| l >= 0.15) };
    (0..=20).map(|step| mix(c, toward, step as f32 / 20.0)).find(|m| reads(luminance(*m))).unwrap_or(toward)
}

/// egui's own look, for its buttons, menus, fields, tooltips and windows, made from a palette.
///
/// A dark palette gets egui's dark look exactly as it is: that is what every app had before
/// this existed, and what the dark themes were chosen against. A light palette gets egui's light
/// look recolored the way KDE's Breeze builds its widgets from a color scheme: the window in the
/// background color, fields and buttons a step lighter, frames in the text color at a quarter of
/// its strength, hover and press tinted with the accent, and the selection in the accent itself.
/// Popups and menus are opaque however translucent the window is.
pub fn visuals(colors: &Colors) -> Visuals {
    let text = Color32::from_rgb(colors.text.r, colors.text.g, colors.text.b);
    if !is_light(text) {
        return Visuals::dark();
    }
    let accent = Color32::from_rgb(colors.accent.r, colors.accent.g, colors.accent.b);
    let bg = Color32::from_rgb(colors.background.r, colors.background.g, colors.background.b);
    // A light theme that left the background at a dark default would put dark text on dark
    // menus; a plain light gray stands in for it.
    let bg = if luminance(bg) > luminance(text) && contrast(bg, text) >= 3.0 { bg } else { Color32::from_gray(248) };
    let field = mix(bg, Color32::WHITE, 0.9);
    let button = mix(bg, Color32::WHITE, 0.8);
    let header = mix(bg, text, 0.08);
    let frame = mix(bg, text, 0.25);
    let hover = mix(bg, accent, 0.2);
    let press = mix(bg, accent, 0.4);
    // The text stays the palette's on the selection, lightening the accent behind it if need
    // be, rather than turning white: egui draws the frame of a focused text field and the
    // percentage on a progress bar in the same color as selected text, and white would vanish
    // on a white field.
    let selection = (0..=10)
        .map(|step| mix(accent, bg, step as f32 / 10.0))
        .find(|fill| contrast(text, *fill) >= 4.5)
        .unwrap_or(bg);

    let mut v = Visuals::light();
    // Plain coverage as alpha. egui's dark-mode default fattens the edges of glyphs, which reads
    // well for light text on dark and smears dark text on light; Vanta Lock turns it off too.
    v.text_options.color_transfer_function = egui::epaint::FontColorTransferFunction::Off;

    let w = &mut v.widgets;
    w.noninteractive.bg_fill = bg;
    w.noninteractive.weak_bg_fill = bg;
    w.noninteractive.bg_stroke = Stroke::new(1.0, frame);
    w.noninteractive.fg_stroke = Stroke::new(1.0, text);
    // Buttons a step lighter than the window, and the boxes of checkboxes and the rails of
    // sliders a step darker: egui's menus take the frames off both, and a box as light as the
    // menu would disappear into it.
    w.inactive.bg_fill = header;
    w.inactive.weak_bg_fill = button;
    w.inactive.bg_stroke = Stroke::new(1.0, frame);
    w.inactive.fg_stroke = Stroke::new(1.0, text);
    w.hovered.bg_fill = hover;
    w.hovered.weak_bg_fill = hover;
    w.hovered.bg_stroke = Stroke::new(1.0, accent);
    w.hovered.fg_stroke = Stroke::new(1.5, text);
    w.active.bg_fill = press;
    w.active.weak_bg_fill = press;
    w.active.bg_stroke = Stroke::new(1.0, accent);
    w.active.fg_stroke = Stroke::new(2.0, text);
    w.open.bg_fill = bg;
    w.open.weak_bg_fill = header;
    w.open.bg_stroke = Stroke::new(1.0, frame);
    w.open.fg_stroke = Stroke::new(1.0, text);

    v.selection.bg_fill = selection;
    v.selection.stroke = Stroke::new(1.0, text);
    v.hyperlink_color = legible(accent, true);
    v.faint_bg_color = mix(bg, text, 0.04);
    v.extreme_bg_color = field;
    v.code_bg_color = header;
    v.warn_fg_color = legible(v.warn_fg_color, true);
    v.error_fg_color = legible(color32(colors.error), true);
    v.window_fill = bg;
    v.window_stroke = Stroke::new(1.0, frame);
    v.panel_fill = bg;
    v.text_cursor.stroke = Stroke::new(2.0, text);
    v
}

/// Gives `ctx` the look `visuals` makes from `colors`, and has egui use it whatever the desktop
/// says it prefers: the palette decides between light and dark, not the system. Called again on
/// every config reload, which is how a switch from a light theme to a dark one and back is
/// followed without a restart.
pub fn apply_visuals(ctx: &egui::Context, colors: &Colors) {
    let v = visuals(colors);
    let theme = if v.dark_mode { egui::Theme::Dark } else { egui::Theme::Light };
    ctx.set_visuals_of(theme, v);
    ctx.set_theme(theme);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn colors(background: &str, text: &str, accent: &str, error: &str) -> Colors {
        let c = |s: &str| parse_theme_color(s).unwrap();
        Colors { background: c(background), text: c(text), accent: c(accent), border: c(accent), highlight: c(accent), error: c(error) }
    }

    fn breeze_light() -> Colors {
        colors("#ffeff0f1", "#232629", "#3daee9", "#da4453")
    }

    fn breeze_dark() -> Colors {
        colors("#ff202326", "#fcfcfc", "#3daee9", "#da4453")
    }

    #[test]
    fn breeze_light_gives_light_visuals_with_its_own_dark_text() {
        let v = visuals(&breeze_light());
        let text = Color32::from_rgb(0x23, 0x26, 0x29);
        assert!(!v.dark_mode);
        assert_eq!(v.text_color(), text);
        assert_eq!(v.window_fill, Color32::from_rgb(0xef, 0xf0, 0xf1));
        assert!(v.extreme_bg_color.r() >= 0xfc, "fields are white, as Breeze's views are");
        assert_eq!(v.text_options.color_transfer_function, egui::epaint::FontColorTransferFunction::Off);
        for (what, fill, ink) in [
            ("labels", v.window_fill, v.text_color()),
            ("buttons", v.widgets.inactive.weak_bg_fill, v.widgets.inactive.fg_stroke.color),
            ("hovered", v.widgets.hovered.weak_bg_fill, v.widgets.hovered.fg_stroke.color),
            ("pressed", v.widgets.active.weak_bg_fill, v.widgets.active.fg_stroke.color),
            ("fields", v.extreme_bg_color, v.text_color()),
            ("selection", v.selection.bg_fill, v.selection.stroke.color),
            ("errors", v.window_fill, v.error_fg_color),
            ("links", v.window_fill, v.hyperlink_color),
        ] {
            assert!(contrast(fill, ink) >= 3.0, "{what}: {ink:?} on {fill:?} does not read");
        }
        assert_eq!(v.selection.bg_fill, Color32::from_rgb(0x3d, 0xae, 0xe9), "the accent reads with the text on it as it is");
        assert_eq!(v.error_fg_color, Color32::from_rgb(0xda, 0x44, 0x53), "a red that reads is left alone");
    }

    #[test]
    fn breeze_dark_gives_egui_s_own_dark_look() {
        let v = visuals(&breeze_dark());
        assert!(v.dark_mode);
        assert_eq!(v, Visuals::dark());
        assert!(visuals(&Palette::default().color).dark_mode, "and so does the Vanta theme");
    }

    #[test]
    fn switching_back_and_forth_follows_the_palette() {
        let ctx = egui::Context::default();
        for (colors, dark) in [(breeze_light(), false), (breeze_dark(), true), (breeze_light(), false)] {
            apply_visuals(&ctx, &colors);
            assert_eq!(ctx.theme(), if dark { egui::Theme::Dark } else { egui::Theme::Light });
            assert_eq!(ctx.global_style().visuals, visuals(&colors));
        }
    }

    #[test]
    fn a_light_theme_on_a_dark_background_still_gets_light_menus() {
        let v = visuals(&colors("#d1000000", "#232629", "#3daee9", "#da4453"));
        assert!(contrast(v.window_fill, v.text_color()) >= 4.5);
    }

    #[test]
    fn colors_that_do_not_read_are_moved_until_they_do() {
        let deep = Color32::from_rgb(0x80, 0, 0);
        assert!(luminance(legible(deep, false)) >= 0.15);
        let pale = Color32::from_rgb(255, 128, 128);
        assert!(luminance(legible(pale, true)) <= 0.22);
        assert_eq!(legible(pale, false), pale);
    }
}
