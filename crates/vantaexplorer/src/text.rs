//! Text fitting helpers shared by the file list and the sidebar.

/// Fit `text` into `max_w`, measuring real glyphs rather than guessing from character counts
/// (theme fonts vary wildly in width, so a count-based guess over- or under-shoots badly).
pub fn elide_end(ui: &egui::Ui, text: &str, max_w: f32) -> String {
    fit(ui, text, max_w, false)
}

/// Same, but drops characters from the FRONT so the distinctive tail stays readable
/// better for long paths and drive names like "Backup-Drive-2TB".
pub fn elide_start(ui: &egui::Ui, text: &str, max_w: f32) -> String {
    fit(ui, text, max_w, true)
}

/// One layout of the whole text, then the cut read off its glyph positions. Trying one
/// character less at a time and laying each attempt out again cost a layout per character too
/// many, per row, per frame: paging through a folder of long names spent most of each frame
/// here.
fn fit(ui: &egui::Ui, text: &str, max_w: f32, from_start: bool) -> String {
    if max_w <= 0.0 {
        return String::new();
    }
    let font = egui::TextStyle::Body.resolve(ui.style());
    let (galley, ellipsis) = ui.ctx().fonts_mut(|f| {
        (
            f.layout_no_wrap(text.to_string(), font.clone(), egui::Color32::WHITE),
            f.layout_no_wrap("…".to_string(), font.clone(), egui::Color32::WHITE).size().x,
        )
    });
    let full = galley.size().x;
    if full <= max_w {
        return text.to_string();
    }
    // (character, left edge, right edge), in order.
    let glyphs: Vec<(char, f32, f32)> =
        galley.rows.iter().flat_map(|r| r.row.glyphs.iter().map(|g| (g.chr, g.pos.x, g.pos.x + g.advance_width))).collect();
    let room = max_w - ellipsis;
    let n = glyphs.len();
    // How many characters to keep, and the text that makes.
    let candidate = |keep: usize| -> String {
        if from_start {
            std::iter::once('…').chain(glyphs[n - keep..].iter().map(|g| g.0)).collect()
        } else {
            glyphs[..keep].iter().map(|g| g.0).chain(std::iter::once('…')).collect()
        }
    };
    let fits = |keep: usize| ui.ctx().fonts_mut(|f| f.layout_no_wrap(candidate(keep), font.clone(), egui::Color32::WHITE)).size().x <= max_w;
    // The glyph positions give the answer to within a character (positions are rounded to
    // pixels, so pieces do not add up exactly); one or two real measurements settle it.
    let mut keep = if from_start {
        n - glyphs.iter().position(|g| full - g.1 <= room).unwrap_or(n)
    } else {
        glyphs.iter().take_while(|g| g.2 <= room).count()
    }
    .min(n.saturating_sub(1));
    while keep > 0 && !fits(keep) {
        keep -= 1;
    }
    while keep + 1 < n && fits(keep + 1) {
        keep += 1;
    }
    if keep == 0 && !fits(0) {
        return String::new();
    }
    candidate(keep)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The old way, one character less at a time, kept as the reference.
    fn fit_slow(ui: &egui::Ui, text: &str, max_w: f32, from_start: bool) -> String {
        let font = egui::TextStyle::Body.resolve(ui.style());
        let width = |t: &str| ui.ctx().fonts_mut(|f| f.layout_no_wrap(t.to_string(), font.clone(), egui::Color32::WHITE)).size().x;
        if max_w <= 0.0 {
            return String::new();
        }
        if width(text) <= max_w {
            return text.to_string();
        }
        let mut chars: Vec<char> = text.chars().collect();
        while !chars.is_empty() {
            if from_start {
                chars.remove(0);
            } else {
                chars.pop();
            }
            let candidate: String = if from_start {
                std::iter::once('…').chain(chars.iter().copied()).collect()
            } else {
                chars.iter().copied().chain(std::iter::once('…')).collect()
            };
            if width(&candidate) <= max_w {
                return candidate;
            }
        }
        String::new()
    }

    #[test]
    fn cuts_where_the_old_way_did() {
        let ctx = egui::Context::default();
        let mut checked = 0;
        let mut differ = Vec::new();
        for _ in 0..2 {
            // No GPU here takes the font atlas the layouts fill, so its update is let go on purpose.
            // Dropped unhandled, egui stops a debug build with an assertion.
            ctx.run_ui(egui::RawInput::default(), |ui| {
                for text in ["et-ret-langt-filnavn-til-test-af-afkortning-nummer-0042.txt", "Backup-Drive-2TB", "æøå ÆØÅ mixed Widths WWWiii", "short"] {
                    for w in (0..400).step_by(7) {
                        for from_start in [false, true] {
                            let (a, b) = (fit(ui, text, w as f32, from_start), fit_slow(ui, text, w as f32, from_start));
                            checked += 1;
                            if a != b {
                                differ.push(format!("{text:?} at {w} ({from_start}): {a:?} vs {b:?}"));
                            }
                        }
                    }
                }
            })
            .textures_delta
            .clear();
        }
        assert!(checked > 0);
        assert!(differ.is_empty(), "{} of {checked} differ, e.g. {:?}", differ.len(), &differ[..differ.len().min(5)]);
    }
}

