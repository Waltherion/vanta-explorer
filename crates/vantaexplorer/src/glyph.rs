//! Painter-drawn UI glyphs.
//!
//! Theme fonts are arbitrary, decorative ones included, and many of them reliably carry arrow, chevron or split code points. Any glyph we picked
//! rendered as a tofu square in at least one theme. Drawing the shapes ourselves is the only
//! option that survives every theme.

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Glyph {
    ChevronDown,
    ChevronRight,
    ChevronLeft,
    Up,
    Down,
    Cross,
    Plus,
    Reload,
    SplitH,
    SplitV,
    /// Triangle over a bar: the standard eject mark.
    Eject,
    /// A pencil, for editing the path as text.
    Edit,
}

pub fn paint(p: &egui::Painter, rect: egui::Rect, glyph: Glyph, col: egui::Color32) {
    let c = rect.center();
    let r = rect.width() * 0.26;
    let stroke = egui::Stroke::new(1.6, col);
    let tri = |pts: Vec<egui::Pos2>| egui::Shape::convex_polygon(pts, col, egui::Stroke::NONE);

    match glyph {
        Glyph::ChevronDown | Glyph::Down => {
            p.add(tri(vec![
                egui::pos2(c.x - r, c.y - r * 0.6),
                egui::pos2(c.x + r, c.y - r * 0.6),
                egui::pos2(c.x, c.y + r * 0.9),
            ]));
        }
        Glyph::Up => {
            p.add(tri(vec![
                egui::pos2(c.x - r, c.y + r * 0.6),
                egui::pos2(c.x + r, c.y + r * 0.6),
                egui::pos2(c.x, c.y - r * 0.9),
            ]));
        }
        Glyph::Eject => {
            p.add(tri(vec![
                egui::pos2(c.x - r, c.y + r * 0.3),
                egui::pos2(c.x + r, c.y + r * 0.3),
                egui::pos2(c.x, c.y - r * 1.0),
            ]));
            p.line_segment(
                [egui::pos2(c.x - r, c.y + r * 0.85), egui::pos2(c.x + r, c.y + r * 0.85)],
                stroke,
            );
        }
        Glyph::ChevronRight => {
            p.add(tri(vec![
                egui::pos2(c.x - r * 0.6, c.y - r),
                egui::pos2(c.x - r * 0.6, c.y + r),
                egui::pos2(c.x + r * 0.9, c.y),
            ]));
        }
        Glyph::ChevronLeft => {
            p.add(tri(vec![
                egui::pos2(c.x + r * 0.6, c.y - r),
                egui::pos2(c.x + r * 0.6, c.y + r),
                egui::pos2(c.x - r * 0.9, c.y),
            ]));
        }
        Glyph::Cross => {
            p.line_segment(
                [egui::pos2(c.x - r, c.y - r), egui::pos2(c.x + r, c.y + r)],
                stroke,
            );
            p.line_segment(
                [egui::pos2(c.x + r, c.y - r), egui::pos2(c.x - r, c.y + r)],
                stroke,
            );
        }
        Glyph::Plus => {
            p.line_segment([egui::pos2(c.x - r, c.y), egui::pos2(c.x + r, c.y)], stroke);
            p.line_segment([egui::pos2(c.x, c.y - r), egui::pos2(c.x, c.y + r)], stroke);
        }
        Glyph::Edit => {
            // A pencil lying corner to corner: a thick body with a pointed tip at the lower left.
            let d = r * 0.9;
            let (top, low) = (egui::pos2(c.x + d, c.y - d), egui::pos2(c.x - d * 0.45, c.y + d * 0.45));
            p.add(egui::Shape::line_segment([top, low], egui::Stroke::new(r * 0.62, col)));
            // Across the body (perpendicular to it), half its thickness each way.
            let n = egui::vec2(0.707, 0.707) * r * 0.31;
            p.add(tri(vec![low + n, egui::pos2(c.x - d, c.y + d), low - n]));
        }
        Glyph::Reload => {
            // Three-quarter arc plus an arrowhead, approximated with line segments.
            let steps = 14;
            let mut pts = Vec::with_capacity(steps + 1);
            for i in 0..=steps {
                let t = i as f32 / steps as f32;
                let a = std::f32::consts::PI * 0.35 + t * std::f32::consts::PI * 1.55;
                pts.push(egui::pos2(c.x + r * a.cos(), c.y + r * a.sin()));
            }
            p.add(egui::Shape::line(pts.clone(), stroke));
            if let Some(end) = pts.last() {
                let head = r * 0.5;
                p.add(tri(vec![
                    egui::pos2(end.x - head * 0.5, end.y - head * 0.2),
                    egui::pos2(end.x + head * 0.6, end.y - head * 0.1),
                    egui::pos2(end.x, end.y + head * 0.8),
                ]));
            }
        }
        Glyph::SplitH => {
            // Two panes side by side.
            let h = r * 1.6;
            let w = r * 0.75;
            let gap = 1.5;
            for dx in [-(w + gap * 0.5), gap * 0.5] {
                p.rect_stroke(
                    egui::Rect::from_min_size(
                        egui::pos2(c.x + dx, c.y - h * 0.5),
                        egui::vec2(w, h),
                    ),
                    1.0,
                    stroke,
                    egui::StrokeKind::Middle,
                );
            }
        }
        Glyph::SplitV => {
            // Two panes stacked.
            let w = r * 1.9;
            let h = r * 0.65;
            let gap = 1.5;
            for dy in [-(h + gap * 0.5), gap * 0.5] {
                p.rect_stroke(
                    egui::Rect::from_min_size(
                        egui::pos2(c.x - w * 0.5, c.y + dy),
                        egui::vec2(w, h),
                    ),
                    1.0,
                    stroke,
                    egui::StrokeKind::Middle,
                );
            }
        }
    }
}

/// A compact icon-only button. `tint` supplies the theme's normal/hover colours.
pub fn button(
    ui: &mut egui::Ui,
    glyph: Glyph,
    enabled: bool,
    tip: &str,
    normal: egui::Color32,
    hover: egui::Color32,
) -> bool {
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(18.0, 18.0), egui::Sense::click());
    let col = if !enabled {
        normal.gamma_multiply(0.35)
    } else if resp.hovered() {
        hover
    } else {
        normal
    };
    if enabled && resp.hovered() {
        ui.painter()
            .rect_filled(rect, 2.0, egui::Color32::from_white_alpha(20));
    }
    paint(ui.painter(), rect, glyph, col);
    if !tip.is_empty() {
        let _ = resp.clone().on_hover_text(tip);
    }
    enabled && resp.clicked()
}
