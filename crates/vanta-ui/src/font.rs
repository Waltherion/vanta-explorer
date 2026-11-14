//! Loading the theme's font into egui.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

/// The family name the icon glyphs live under, so a module can ask for an icon without knowing
/// which font ended up supplying it.
pub const ICONS: &str = "icons";

/// Fonts that carry the icon glyphs, in the order they are worth trying. A theme picks its text
/// font; the icons come from whatever symbol font the system has, because a display face like
/// Poppins has none of them.
const ICON_FONTS: [&str; 3] = ["Symbols Nerd Font", "JetBrainsMono Nerd Font", "Noto Sans Symbols"];

/// Fonts to fall back on for text the theme font has no glyph for. A display face covers letters
/// and not much else, and text from a script carries whatever the script put in it: the box
/// drawing in a heading, the triangle in front of a video's name. Without these those come out as
/// empty boxes.
const TEXT_FALLBACKS: [&str; 2] = ["DejaVu Sans", "Noto Sans Symbols 2"];

/// An egui context with `family` in front of the default families, found through fontconfig so a
/// theme can name a font the way a person would rather than by file path. An empty name, or a
/// name fontconfig cannot place, leaves egui's own font in place.
///
/// The context also carries an icon family. It is kept separate rather than merged into the text
/// font so that a glyph the theme font happens to have does not quietly win over the icon set,
/// which is how icons end up looking different from one theme to the next.
pub fn context_with_font(family: &str, weight: i32) -> egui::Context {
    context_with_fonts(family, weight, &[]).0
}

/// The name a font loaded as one of `extras` is reached by: its family and its weight, so a theme
/// asking for a light cut and another asking for a bold one each get their own.
pub fn named_family(family: &str, weight: i32) -> String {
    format!("font:{family}:{}", fc_weight(weight))
}

/// The same, with other fonts loaded beside the theme's, each as a family of its own under
/// `named_family`: for drawing something in another theme's font, such as a preview of that
/// theme. Returns the names that were actually found. A font the machine does not have is left
/// out rather than stood in for, so a caller can tell and use its own font instead.
pub fn context_with_fonts(family: &str, weight: i32, extras: &[(String, i32)]) -> (egui::Context, Vec<String>) {
    let ctx = egui::Context::default();
    let mut fonts = egui::FontDefinitions::default();
    let mut changed = false;

    if let Some(data) = font(&format!("{family}:weight={}", fc_weight(weight)), true) {
        // Unhinted. Display faces such as Poppins ship without hinting instructions, so egui falls
        // back on an automatic hinter, and that one snaps the flat tops of 1, 5 and 7 a pixel
        // lower while leaving the round tops of 0, 2 and 9 where they were: a clock whose digits
        // do not stand level. Measured in all four hinting modes, and only unhinted keeps them
        // level. At these sizes it costs no visible sharpness.
        fonts.font_data.insert("theme".into(), data);
        fonts.families.entry(egui::FontFamily::Proportional).or_default().insert(0, "theme".into());
        changed = true;
    }
    // At the back of the list, so they are only asked for what nothing in front of them has.
    for (n, name) in TEXT_FALLBACKS.iter().enumerate() {
        if let Some(data) = font(name, false) {
            let key = format!("fallback{n}");
            fonts.font_data.insert(key.clone(), data);
            fonts.families.entry(egui::FontFamily::Proportional).or_default().push(key);
            changed = true;
        }
    }
    for (n, name) in ICON_FONTS.iter().enumerate() {
        // The two Nerd Fonts carry the same icon set; the second is only wanted without the first.
        if n == 1 && fonts.font_data.contains_key("icons0") {
            continue;
        }
        if let Some(data) = font(name, false) {
            let key = format!("icons{n}");
            fonts.font_data.insert(key.clone(), data);
            fonts.families.entry(egui::FontFamily::Name(ICONS.into())).or_default().push(key);
            changed = true;
        }
    }
    // Other themes' fonts, each on its own, with the same fallbacks behind it and unhinted for
    // the same reason as the theme's.
    let mut found = Vec::new();
    for (extra, extra_weight) in extras {
        let name = named_family(extra, *extra_weight);
        if extra.is_empty() || found.contains(&name) {
            continue;
        }
        let Some(data) = font(&format!("{extra}:weight={}", fc_weight(*extra_weight)), true) else { continue };
        fonts.font_data.insert(name.clone(), data);
        let mut chain = vec![name.clone()];
        chain.extend((0..TEXT_FALLBACKS.len()).map(|n| format!("fallback{n}")).filter(|k| fonts.font_data.contains_key(k)));
        fonts.families.insert(egui::FontFamily::Name(name.clone().into()), chain);
        found.push(name);
        changed = true;
    }
    if changed {
        ctx.set_fonts(fonts);
    }
    (ctx, found)
}

/// A font found through fontconfig, read once per process and shared by every context after
/// that: an app that builds a context per menu or per open would otherwise ask fontconfig and
/// read the files again each time (six lookups, some 230 ms and 8 MB, per context). `unhinted`
/// is part of the key, since the same file can be wanted both ways. A font installed while the
/// app runs is seen after a restart.
fn font(pattern: &str, unhinted: bool) -> Option<Arc<egui::FontData>> {
    static CACHE: Mutex<Option<HashMap<(String, bool), Option<Arc<egui::FontData>>>>> = Mutex::new(None);
    let key = (pattern.to_string(), unhinted);
    if let Some(hit) = CACHE.lock().unwrap().get_or_insert_with(HashMap::new).get(&key) {
        return hit.clone();
    }
    let data = file_bytes(pattern).map(|bytes| {
        let data = egui::FontData::from_owned(bytes);
        Arc::new(if unhinted { data.tweak(egui::FontTweak { hinting: Some(false), ..Default::default() }) } else { data })
    });
    CACHE.lock().unwrap().get_or_insert_with(HashMap::new).insert(key, data.clone());
    data
}

/// CSS weights and the fontconfig weights they are called there.
const WEIGHTS: [(i32, i32); 9] = [(100, 0), (200, 40), (300, 50), (400, 80), (500, 100), (600, 180), (700, 200), (800, 205), (900, 210)];

/// Fontconfig counts weight on a scale of its own, where 210 is black and 80 is regular. A theme
/// writes the CSS number, so 300 means light; handed straight to fontconfig it means black, and
/// the whole interface comes out in the heaviest cut the family has.
fn fc_weight(css: i32) -> i32 {
    WEIGHTS
        .iter()
        .min_by_key(|(w, _)| (w - css).abs())
        .map(|(_, fc)| *fc)
        .unwrap_or(80)
}

/// The file fontconfig would hand a program that asked for this pattern, read into memory. An
/// empty family, or one of fontconfig's generic names (`sans-serif`, `serif`, `monospace`),
/// takes whatever the system has set for it. Returns nothing when a named family is missing and
/// fontconfig substitutes something unrelated.
/// Poppins, built into every app in the four cuts the themes use, for a machine that does not
/// have it installed: it is the default face, and without it the interface would come out in
/// whatever the system substitutes. An installed Poppins is always asked for first and wins.
/// SIL Open Font License 1.1, in `fonts/OFL.txt`.
const POPPINS: [(i32, &[u8]); 4] = [
    (300, include_bytes!("../fonts/Poppins-Light.ttf")),
    (400, include_bytes!("../fonts/Poppins-Regular.ttf")),
    (500, include_bytes!("../fonts/Poppins-Medium.ttf")),
    (600, include_bytes!("../fonts/Poppins-SemiBold.ttf")),
];

/// The built-in cut of `family` nearest to the CSS `weight`, if the family is one that is built
/// in (Poppins).
pub fn embedded(family: &str, weight: i32) -> Option<&'static [u8]> {
    if family.trim().to_lowercase().replace(' ', "") != "poppins" {
        return None;
    }
    POPPINS.iter().min_by_key(|(w, _)| (w - weight).abs()).map(|(_, b)| *b)
}

/// The CSS weight nearest to a fontconfig one: the way back from `fc_weight`.
fn css_weight(fc: i32) -> i32 {
    WEIGHTS.iter().min_by_key(|(_, f)| (f - fc).abs()).map(|(css, _)| *css).unwrap_or(400)
}

/// A font file for a fontconfig pattern (`Poppins`, `Poppins:weight=180`): the file fontconfig
/// finds, or the built-in cut when the family is missing and built in. Nothing when neither: a
/// named family fontconfig would only substitute something unrelated for is not taken.
pub fn file_bytes(pattern: &str) -> Option<Vec<u8>> {
    if let Some(bytes) = load(pattern) {
        return Some(bytes);
    }
    let (family, rest) = pattern.split_once(':').map_or((pattern, ""), |(f, r)| (f, r));
    let fc = rest.split(':').find_map(|kv| kv.strip_prefix("weight=")).and_then(|w| w.trim().parse::<i32>().ok()).unwrap_or(80);
    embedded(family, css_weight(fc)).map(<[u8]>::to_vec)
}

fn load(pattern: &str) -> Option<Vec<u8>> {
    let (family, rest) = pattern.split_once(':').map_or((pattern, None), |(f, r)| (f, Some(r)));
    let family = if family.trim().is_empty() { "sans-serif" } else { family };
    let pattern = match rest {
        Some(r) => format!("{family}:{r}"),
        None => family.to_string(),
    };
    let out = std::process::Command::new("fc-match").args(["--format=%{file}|%{family}", &pattern]).output().ok()?;
    let text = String::from_utf8_lossy(&out.stdout);
    let (path, got) = text.trim().split_once('|')?;
    // fc-match always answers, so a miss comes back as some unrelated default. Comparing the
    // family it actually found is the only way to tell a hit from a polite substitution.
    let generic = ["sans-serif", "sans", "serif", "monospace", "mono"].contains(&family.trim().to_lowercase().as_str());
    let wanted = family.to_lowercase().replace(' ', "");
    if !generic && !got.to_lowercase().replace(' ', "").split(',').any(|f| f.contains(&wanted) || wanted.contains(f)) {
        return None;
    }
    std::fs::read(path).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_built_in_poppins_answers_for_its_family_only_and_in_the_nearest_weight() {
        assert!(embedded("Poppins", 300).is_some_and(|b| b.len() > 100_000));
        assert_eq!(embedded("poppins", 350).map(|b| b.as_ptr()), embedded("Poppins", 300).map(|b| b.as_ptr()));
        assert_eq!(embedded("Poppins", 700).map(|b| b.as_ptr()), embedded("Poppins", 600).map(|b| b.as_ptr()));
        assert!(embedded("Inter", 400).is_none());
        assert_eq!(css_weight(fc_weight(300)), 300);
        assert_eq!(css_weight(180), 600);
    }

    #[test]
    fn css_weights_land_on_fontconfigs_own_scale() {
        assert_eq!(fc_weight(300), 50, "light, not black");
        assert_eq!(fc_weight(400), 80);
        assert_eq!(fc_weight(700), 200);
    }
}
