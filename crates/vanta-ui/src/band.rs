//! The rolling colour band.
//!
//! Every accent surface is a window into ONE band: a colour is sampled at the element's own
//! position, so a whole interface shows different hues at once and the band rolls across it as
//! `phase` advances.
//!
//! The coordinate space is the caller's choice, and the choice matters. An overlay that owns the
//! screen samples in its own coordinates. Something drawn across several surfaces, or several
//! monitors, has to sample in a space they share, or the band restarts at every boundary.

use vanta_config::Color;

/// One full cycle takes `12 / speed` seconds (speed 1 = ~12 s), matching the bar's band.
pub fn phase_at(elapsed_secs: f32, speed: f32) -> f32 {
    let p = elapsed_secs * speed / 12.0;
    p.rem_euclid(1.0)
}

pub struct Band {
    stops: Vec<egui::Color32>,
    period: f32,
    phase: f32,
    accent: egui::Color32,
    pub on: bool,
}

impl Band {
    pub fn new(stops: &[Color], period: f32, phase: f32, accent: egui::Color32, rainbow: bool) -> Self {
        let stops: Vec<egui::Color32> = stops.iter().map(|c| egui::Color32::from_rgb(c.r, c.g, c.b)).collect();
        Band {
            on: rainbow && stops.len() >= 2,
            stops,
            period: if period > 0.0 { period } else { 3000.0 },
            phase,
            accent,
        }
    }

    /// Where the band is in its roll, 0 to 1. For drawing a second band in step with this one.
    pub fn phase(&self) -> f32 {
        self.phase
    }

    /// Colour of the band at board-local position `px` (use `x + y` for the 45 degree axis).
    /// Returns the solid accent when the rainbow is off.
    pub fn at(&self, px: f32) -> egui::Color32 {
        if !self.on {
            return self.accent;
        }
        let n = self.stops.len();
        let t = (px / self.period + self.phase).rem_euclid(1.0) * n as f32;
        let i = (t as usize) % n;
        let j = (i + 1) % n;
        let f = t - t.floor();
        let (a, b) = (self.stops[i], self.stops[j]);
        let mix = |x: u8, y: u8| (x as f32 + (y as f32 - x as f32) * f).round() as u8;
        egui::Color32::from_rgb(mix(a.r(), b.r()), mix(a.g(), b.g()), mix(a.b(), b.b()))
    }

    /// Same, at a point (samples along x + y).
    pub fn at_pos(&self, p: egui::Pos2, origin: egui::Pos2) -> egui::Color32 {
        self.at((p.x - origin.x) + (p.y - origin.y))
    }

    /// Same, along whichever axis the caller reads the band on.
    pub fn at_axis(&self, p: egui::Pos2, origin: egui::Pos2, axis: Axis) -> egui::Color32 {
        match axis {
            Axis::X => self.at(p.x - origin.x),
            Axis::Diagonal => self.at_pos(p, origin),
        }
    }
}

/// Which way the band runs.
///
/// A surface that owns a rectangle of screen reads it diagonally, so the colour travels corner to
/// corner. A bar reads it on x alone: it is one strip spanning several monitors, and a diagonal
/// axis would shift two screens against each other by however far apart they sit vertically.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Axis {
    X,
    Diagonal,
}

/// Points along a rounded rectangle's outline.
///
/// The corners get a point roughly every screen pixel of arc. A fixed handful, which is the
/// obvious thing, turns a generous corner radius into a visible polygon: the soft edge hides the
/// staircase along each segment but not the angle where two segments meet.
fn rounded_outline(rect: egui::Rect, radius: f32, step: f32) -> Vec<egui::Pos2> {
    let r = radius.clamp(0.0, (rect.width().min(rect.height()) * 0.5).max(0.0));
    let step = step.max(4.0);
    let mut pts = Vec::new();
    let line = |from: egui::Pos2, to: egui::Pos2, pts: &mut Vec<egui::Pos2>| {
        let d = to - from;
        let n = ((d.length() / step).ceil() as usize).max(1);
        for k in 0..n {
            pts.push(from + d * (k as f32 / n as f32));
        }
    };
    // A quarter circle of radius r is about 1.57r long, so this is a segment per point of arc.
    let n_arc = ((r * 1.6).ceil() as usize).clamp(4, 96);
    let arc = |cx: f32, cy: f32, a0: f32, pts: &mut Vec<egui::Pos2>| {
        for k in 0..=n_arc {
            let a = a0 + std::f32::consts::FRAC_PI_2 * (k as f32 / n_arc as f32);
            pts.push(egui::pos2(cx + r * a.cos(), cy + r * a.sin()));
        }
    };
    let (l, t, rt, b) = (rect.min.x, rect.min.y, rect.max.x, rect.max.y);
    line(egui::pos2(l + r, t), egui::pos2(rt - r, t), &mut pts);
    arc(rt - r, t + r, -std::f32::consts::FRAC_PI_2, &mut pts);
    line(egui::pos2(rt, t + r), egui::pos2(rt, b - r), &mut pts);
    arc(rt - r, b - r, 0.0, &mut pts);
    line(egui::pos2(rt - r, b), egui::pos2(l + r, b), &mut pts);
    arc(l + r, b - r, std::f32::consts::FRAC_PI_2, &mut pts);
    line(egui::pos2(l, b - r), egui::pos2(l, t + r), &mut pts);
    arc(l + r, t + r, std::f32::consts::PI, &mut pts);
    pts.push(egui::pos2(l + r, t));
    pts
}

/// Strokes a rounded rectangle with the band.
///
/// The outline is one triangle strip whose vertices each carry their own colour, so the hardware
/// blends between them and the gradient is continuous. Drawing it as short segments of a single
/// colour each, which is the obvious way, gives a visibly stepped band and leaves a seam at every
/// joint: the eye picks both up immediately on a slow roll.
pub fn stroke_rect(p: &egui::Painter, rect: egui::Rect, radius: f32, width: f32, band: &Band, origin: egui::Pos2, axis: Axis, alpha: u8) {
    if width <= 0.0 {
        return;
    }
    if !band.on {
        let c = band.accent;
        let c = egui::Color32::from_rgba_unmultiplied(c.r(), c.g(), c.b(), alpha);
        p.rect_stroke(rect, radius, egui::Stroke::new(width, c), egui::StrokeKind::Inside);
        return;
    }
    let inset = rect.shrink(width * 0.5);
    let pts = rounded_outline(inset, radius, 3.0);
    // The outline comes back closed, so the repeated last point is dropped and the ring is
    // walked with wrap-around instead.
    let n = pts.len().saturating_sub(1);
    if n < 3 {
        return;
    }
    let ribs: Vec<Rib> = (0..=n)
        .map(|k| {
            let i = k % n;
            let prev = pts[(i + n - 1) % n];
            let next = pts[(i + 1) % n];
            // The normal comes from the neighbours rather than from the centre, which is what
            // keeps the band an even width along a long edge as well as around a corner.
            let t = (next - prev).normalized();
            let c = band.at_axis(pts[i], origin, axis);
            Rib {
                at: pts[i],
                across: egui::vec2(t.y, -t.x),
                color: egui::Color32::from_rgba_unmultiplied(c.r(), c.g(), c.b(), alpha),
            }
        })
        .collect();
    ribbon(p, &ribs, width * 0.5);
}

/// Draws a laid-out line of text with the band running through it.
///
/// The glyphs are re-emitted as one mesh with a colour per vertex, so the band crosses a letter
/// the same way it crosses the space beside it. Giving each glyph one flat colour, which is the
/// cheap way, turns a gradient into a row of blocks.
pub fn text(p: &egui::Painter, galley: &egui::Galley, pos: egui::Pos2, band: &Band, origin: egui::Pos2, axis: Axis, alpha: f32) {
    // On a whole physical pixel, as egui's own text is. The glyphs were rasterised with their
    // tops snapped to the pixel grid; placed between two pixels, each one is smeared over an extra
    // row, and a round digit then reads taller than a flat one beside it.
    let ppp = p.ctx().pixels_per_point().max(0.1);
    let pos = egui::pos2((pos.x * ppp).round() / ppp, (pos.y * ppp).round() / ppp);
    let [tw, th] = p.ctx().fonts(|f| f.font_image_size());
    let (tw, th) = (tw as f32, th as f32);
    let mut mesh = egui::Mesh::default();
    for row in &galley.rows {
        let src = &row.visuals.mesh;
        let base = mesh.vertices.len() as u32;
        let offset = pos.to_vec2() + row.pos.to_vec2();
        for v in &src.vertices {
            let at = v.pos + offset;
            let c = band.at_axis(at, origin, axis).gamma_multiply(alpha);
            // Row meshes carry texel coordinates, not normalised ones.
            mesh.vertices.push(egui::epaint::Vertex { pos: at, uv: egui::pos2(v.uv.x / tw, v.uv.y / th), color: c });
        }
        mesh.indices.extend(src.indices.iter().map(|i| i + base));
    }
    if !mesh.is_empty() {
        p.add(egui::Shape::Mesh(mesh.into()));
    }
}

fn vertex(pos: egui::Pos2, color: egui::Color32) -> egui::epaint::Vertex {
    egui::epaint::Vertex { pos, uv: egui::epaint::WHITE_UV, color }
}

/// How wide the soft edge is, in the logical units everything is drawn in.
///
/// One physical pixel, which is the area a pixel can actually be partly covered over.
///
/// Read from the painter rather than passed around, so there is one answer and no call site can
/// get it wrong.
fn feather(p: &egui::Painter) -> f32 {
    1.0 / p.ctx().pixels_per_point().max(0.1)
}

/// A point on a ribbon: where it is, which way is across it, and what colour it is there.
#[derive(Clone, Copy)]
pub struct Rib {
    pub at: egui::Pos2,
    /// Unit vector across the ribbon. Its length is not used.
    pub across: egui::Vec2,
    pub color: egui::Color32,
}

/// Draws a ribbon along a line, `half` wide either side, with a soft edge.
///
/// The soft edge is the whole point. A strip built from plain triangles has hard edges, and a
/// hard edge on anything but a horizontal or vertical line is a visible staircase: on a moving
/// curve it is the first thing the eye finds. egui's own shapes do not have the problem because
/// its tessellator fades every outline out over one screen pixel, which is exactly what the extra
/// pair of transparent vertices here does.
///
pub fn ribbon(p: &egui::Painter, ribs: &[Rib], half: f32) {
    if ribs.len() < 2 {
        return;
    }
    let feather = feather(p);
    // The soft rim sits ASTRIDE the edge, not outside it. Laid outside, a line covers its own
    // width at full strength and then fades on top of that, so it puts down half again as much
    // colour as it should and leaves a row of fully saturated pixels down the middle. A rasteriser
    // never does that: a line a pixel and a quarter wide can cover at most a pixel and a quarter,
    // so the ink is spread and almost nothing comes out solid.
    //
    // Thinner than the rim itself and there is no solid part left at all. Then the width is spent
    // on how strong the line is instead of how wide it is, which is the only way a hairline can
    // stay a hairline.
    let (inner, outer, strength) = if half * 2.0 >= feather {
        (half - feather * 0.5, half + feather * 0.5, 1.0)
    } else {
        (0.0, feather * 0.5, (half * 2.0) / feather)
    };
    let mut mesh = egui::Mesh::default();
    let clear = egui::Color32::from_rgba_premultiplied(0, 0, 0, 0);
    for r in ribs {
        let color = r.color.gamma_multiply(strength);
        mesh.vertices.push(vertex(r.at + r.across * outer, clear));
        mesh.vertices.push(vertex(r.at + r.across * inner, color));
        mesh.vertices.push(vertex(r.at - r.across * inner, color));
        mesh.vertices.push(vertex(r.at - r.across * outer, clear));
    }
    for i in 0..(ribs.len() as u32 - 1) {
        let a = i * 4;
        for k in 0..3u32 {
            mesh.indices.extend_from_slice(&[a + k, a + k + 1, a + k + 4, a + k + 1, a + k + 5, a + k + 4]);
        }
    }
    p.add(egui::Shape::Mesh(mesh.into()));
}

/// Fills a rounded rectangle with the band, as one piece.
///
/// The same idea as `stroke_rect`: a colour per vertex, so the hardware blends from one to the
/// next and the gradient is continuous. Filling it as a row of narrow flat rectangles, which is the
/// obvious way, reads as blocks: each rectangle gets its own soft edge, and the edges between them
/// show as a seam every few pixels.
///
/// Built as columns across the shape. The corners are walked by angle, since near the rounded end
/// the outline runs almost straight up and stepping along x would skip most of it; the straight
/// part is walked along x, finely enough that the band has no visible corners of its own.
pub fn fill_rounded(p: &egui::Painter, rect: egui::Rect, radius: f32, band: &Band, origin: egui::Pos2, axis: Axis, alpha: u8) {
    if rect.width() <= 0.0 || rect.height() <= 0.0 {
        return;
    }
    if !band.on {
        let c = band.accent;
        p.rect_filled(rect, radius, egui::Color32::from_rgba_unmultiplied(c.r(), c.g(), c.b(), alpha));
        return;
    }
    let feather = feather(p);
    let r = radius.clamp(0.0, rect.width().min(rect.height()) * 0.5);
    let n_arc = if r > 0.0 { ((r / feather * 0.8).ceil() as usize).clamp(4, 64) } else { 4 };
    let step = feather * 3.0;

    // Each column: where it crosses the top and the bottom of the outline, and which way is out
    // at each of those two points.
    let mut cols: Vec<(egui::Pos2, egui::Vec2, egui::Pos2, egui::Vec2)> = Vec::new();
    let (top_c, bot_c) = (rect.min.y + r, rect.max.y - r);
    for k in 0..=n_arc {
        let a = std::f32::consts::FRAC_PI_2 * k as f32 / n_arc as f32;
        let (c, s) = (a.cos(), a.sin());
        let x = rect.min.x + r - r * c;
        cols.push((egui::pos2(x, top_c - r * s), egui::vec2(-c, -s), egui::pos2(x, bot_c + r * s), egui::vec2(-c, s)));
    }
    let (x0, x1) = (rect.min.x + r, rect.max.x - r);
    let n_flat = ((x1 - x0) / step).ceil() as usize;
    for k in 1..n_flat {
        let x = x0 + (x1 - x0) * k as f32 / n_flat as f32;
        cols.push((egui::pos2(x, rect.min.y), egui::vec2(0.0, -1.0), egui::pos2(x, rect.max.y), egui::vec2(0.0, 1.0)));
    }
    for k in (0..=n_arc).rev() {
        let a = std::f32::consts::FRAC_PI_2 * k as f32 / n_arc as f32;
        let (c, s) = (a.cos(), a.sin());
        let x = rect.max.x - r + r * c;
        cols.push((egui::pos2(x, top_c - r * s), egui::vec2(c, -s), egui::pos2(x, bot_c + r * s), egui::vec2(c, s)));
    }

    let clear = egui::Color32::from_rgba_premultiplied(0, 0, 0, 0);
    let tint = |at: egui::Pos2| {
        let c = band.at_axis(at, origin, axis);
        egui::Color32::from_rgba_unmultiplied(c.r(), c.g(), c.b(), alpha)
    };
    let half = feather * 0.5;
    let mut mesh = egui::Mesh::default();
    for (top, tn, bot, bn) in &cols {
        let mut ti = *top - *tn * half;
        let mut bi = *bot - *bn * half;
        // Where the shape is thinner than the soft edge, the two inner points would pass each
        // other and the column would cover itself twice.
        if ti.y > bi.y {
            let mid = (ti.y + bi.y) * 0.5;
            ti.y = mid;
            bi.y = mid;
        }
        mesh.vertices.push(vertex(*top + *tn * half, clear));
        mesh.vertices.push(vertex(ti, tint(*top)));
        mesh.vertices.push(vertex(bi, tint(*bot)));
        mesh.vertices.push(vertex(*bot + *bn * half, clear));
    }
    let n = cols.len() as u32;
    for i in 0..n - 1 {
        let a = i * 4;
        for k in 0..3u32 {
            mesh.indices.extend_from_slice(&[a + k, a + k + 1, a + k + 4, a + k + 1, a + k + 5, a + k + 4]);
        }
    }
    // The two ends. A straight end has a height of its own that no column covers the edge of.
    for a in [0, (n - 1) * 4] {
        mesh.indices.extend_from_slice(&[a, a + 1, a + 2, a, a + 2, a + 3]);
    }
    p.add(egui::Shape::Mesh(mesh.into()));
}

/// A soft light along a line: full strength on the line itself, fading evenly to nothing `half`
/// out on either side. Laid under a line it makes the line read as something lit rather than
/// something drawn, the way a neon tube lights the air around it.
pub fn halo(p: &egui::Painter, ribs: &[Rib], half: f32) {
    if ribs.len() < 2 || half <= 0.0 {
        return;
    }
    let clear = egui::Color32::from_rgba_premultiplied(0, 0, 0, 0);
    let mut mesh = egui::Mesh::default();
    for r in ribs {
        mesh.vertices.push(vertex(r.at + r.across * half, clear));
        mesh.vertices.push(vertex(r.at, r.color));
        mesh.vertices.push(vertex(r.at - r.across * half, clear));
    }
    for i in 0..(ribs.len() as u32 - 1) {
        let a = i * 3;
        for k in 0..2u32 {
            mesh.indices.extend_from_slice(&[a + k, a + k + 1, a + k + 3, a + k + 1, a + k + 4, a + k + 3]);
        }
    }
    p.add(egui::Shape::Mesh(mesh.into()));
}

/// Fills the band between two lines, with a soft edge along the first one.
///
/// The second line is the flat bottom an area chart sits on, which is horizontal and needs no
/// softening; the first is the curve, which does.
pub fn area(p: &egui::Painter, ribs: &[Rib], bottom: f32) {
    if ribs.len() < 2 {
        return;
    }
    let feather = feather(p);
    let clear = egui::Color32::from_rgba_premultiplied(0, 0, 0, 0);
    let mut mesh = egui::Mesh::default();
    for r in ribs {
        mesh.vertices.push(vertex(r.at - r.across * (feather * 0.5), clear));
        mesh.vertices.push(vertex(r.at + r.across * (feather * 0.5), r.color));
        mesh.vertices.push(vertex(egui::pos2(r.at.x, bottom), r.color));
    }
    for i in 0..(ribs.len() as u32 - 1) {
        let a = i * 3;
        for k in 0..2u32 {
            mesh.indices.extend_from_slice(&[a + k, a + k + 1, a + k + 3, a + k + 1, a + k + 4, a + k + 3]);
        }
    }
    p.add(egui::Shape::Mesh(mesh.into()));
}
