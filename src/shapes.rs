//! The mark-up shapes and how they are drawn onto the picture.
//!
//! ONE drawing path for the screen and for the saved file: the frozen screen shows a layer made by `render()` and
//! Copy / Save use the same `render()` on the whole box - what you see is what you get. Blur / pixelate change the
//! pixels themselves (no "blur filter" on top that a viewer could take off) and every export is a brand-new picture
//! (no old pixels left in the file - the 2023 "aCropalypse" lesson).

use ab_glyph::{Font, FontRef, PxScale, ScaleFont};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::{Arc, OnceLock};
use tiny_skia::{
    BlendMode, FillRule, LineCap, LineJoin, Paint, PathBuilder, Pixmap, PixmapMut, PixmapPaint, Stroke, Transform,
};

/// A picture in memory: premultiplied RGBA (tiny-skia's own format; screenshots are opaque, so it's plain RGBA too).
pub struct Pic {
    pub w: u32,
    pub h: u32,
    pub px: Vec<u8>,
}

impl Pic {
    pub fn new(w: u32, h: u32) -> Pic {
        Pic { w, h, px: vec![0; (w as usize) * (h as usize) * 4] }
    }
    pub fn at(&self, x: i32, y: i32) -> [u8; 4] {
        if x < 0 || y < 0 || x >= self.w as i32 || y >= self.h as i32 {
            return [0, 0, 0, 0];
        }
        let i = (y as usize * self.w as usize + x as usize) * 4;
        [self.px[i], self.px[i + 1], self.px[i + 2], self.px[i + 3]]
    }
    /// A copy of one part (clamped to the picture).
    pub fn crop(&self, r: IR) -> Pic {
        let r = r.clamp(self.w as i32, self.h as i32);
        let mut out = Pic::new(r.w.max(0) as u32, r.h.max(0) as u32);
        for y in 0..r.h {
            let s = ((r.y + y) as usize * self.w as usize + r.x as usize) * 4;
            let d = (y as usize) * (r.w as usize) * 4;
            out.px[d..d + r.w as usize * 4].copy_from_slice(&self.px[s..s + r.w as usize * 4]);
        }
        out
    }
}

/// Rectangle in picture pixels (float), always w, h >= 0.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct R {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

impl R {
    pub fn new(x: f32, y: f32, w: f32, h: f32) -> R {
        R { x, y, w, h }
    }
    pub fn from_pts(a: (f32, f32), b: (f32, f32)) -> R {
        R { x: a.0.min(b.0), y: a.1.min(b.1), w: (a.0 - b.0).abs(), h: (a.1 - b.1).abs() }
    }
    pub fn r(&self) -> f32 {
        self.x + self.w
    }
    pub fn b(&self) -> f32 {
        self.y + self.h
    }
    pub fn contains(&self, p: (f32, f32)) -> bool {
        p.0 >= self.x && p.0 <= self.r() && p.1 >= self.y && p.1 <= self.b()
    }
    pub fn inflate(&self, d: f32) -> R {
        R { x: self.x - d, y: self.y - d, w: self.w + 2.0 * d, h: self.h + 2.0 * d }
    }
    pub fn union(&self, o: &R) -> R {
        let x = self.x.min(o.x);
        let y = self.y.min(o.y);
        R { x, y, w: self.r().max(o.r()) - x, h: self.b().max(o.b()) - y }
    }
    pub fn moved(&self, dx: f32, dy: f32) -> R {
        R { x: self.x + dx, y: self.y + dy, ..*self }
    }
    pub fn ir(&self) -> IR {
        let x = self.x.round() as i32;
        let y = self.y.round() as i32;
        IR { x, y, w: (self.r().round() as i32 - x).max(0), h: (self.b().round() as i32 - y).max(0) }
    }
}

/// Rectangle in whole pixels.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct IR {
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
}

impl IR {
    pub fn clamp(&self, w: i32, h: i32) -> IR {
        let x0 = self.x.clamp(0, w);
        let y0 = self.y.clamp(0, h);
        let x1 = (self.x + self.w).clamp(0, w);
        let y1 = (self.y + self.h).clamp(0, h);
        IR { x: x0, y: y0, w: (x1 - x0).max(0), h: (y1 - y0).max(0) }
    }
    pub fn intersect(&self, o: &IR) -> IR {
        let x0 = self.x.max(o.x);
        let y0 = self.y.max(o.y);
        let x1 = (self.x + self.w).min(o.x + o.w);
        let y1 = (self.y + self.h).min(o.y + o.h);
        IR { x: x0, y: y0, w: (x1 - x0).max(0), h: (y1 - y0).max(0) }
    }
    pub fn f(&self) -> R {
        R::new(self.x as f32, self.y as f32, self.w as f32, self.h as f32)
    }
    pub fn is_empty(&self) -> bool {
        self.w <= 0 || self.h <= 0
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Tool {
    Select,
    Crop,
    Pen,
    Highlight,
    Line,
    Arrow,
    Rect,
    Ellipse,
    Text,
    Step,
    Blur,
    Spotlight,
    Picker,
}

impl Tool {
    /// The order of the tool strip (overlay.html): select, crop | drawing tools | blur, spotlight, picker
    pub const ALL: [Tool; 13] = [
        Tool::Select,
        Tool::Crop,
        Tool::Pen,
        Tool::Highlight,
        Tool::Line,
        Tool::Arrow,
        Tool::Rect,
        Tool::Ellipse,
        Tool::Text,
        Tool::Step,
        Tool::Blur,
        Tool::Spotlight,
        Tool::Picker,
    ];
    pub fn index(self) -> usize {
        Tool::ALL.iter().position(|t| *t == self).unwrap_or(0)
    }
    pub fn id(self) -> &'static str {
        match self {
            Tool::Select => "select",
            Tool::Crop => "crop",
            Tool::Pen => "pen",
            Tool::Highlight => "highlight",
            Tool::Line => "line",
            Tool::Arrow => "arrow",
            Tool::Rect => "rect",
            Tool::Ellipse => "ellipse",
            Tool::Text => "text",
            Tool::Step => "step",
            Tool::Blur => "blur",
            Tool::Spotlight => "spotlight",
            Tool::Picker => "picker",
        }
    }
    pub fn name(self) -> &'static str {
        match self {
            Tool::Select => "Select",
            Tool::Crop => "Crop",
            Tool::Pen => "Pen",
            Tool::Highlight => "Highlighter",
            Tool::Line => "Line",
            Tool::Arrow => "Arrow",
            Tool::Rect => "Rectangle",
            Tool::Ellipse => "Ellipse",
            Tool::Text => "Text",
            Tool::Step => "Number step",
            Tool::Blur => "Blur",
            Tool::Spotlight => "Spotlight",
            Tool::Picker => "Colour picker",
        }
    }
    pub fn from_id(s: &str) -> Option<Tool> {
        Tool::ALL.iter().copied().find(|t| t.id() == s)
    }
    /// (min, max, default) of the size slider. Lines: stroke px; text: font px; step: circle px; blur: strength.
    pub fn size_range(self) -> (f32, f32, f32) {
        match self {
            Tool::Text => (10.0, 120.0, 24.0),
            Tool::Step => (16.0, 80.0, 28.0),
            Tool::Blur => (2.0, 40.0, 10.0),
            Tool::Highlight => (8.0, 60.0, 18.0),
            _ => (1.0, 40.0, 4.0),
        }
    }
    pub fn has_size(self) -> bool {
        !matches!(self, Tool::Select | Tool::Crop | Tool::Picker | Tool::Spotlight)
    }
    pub fn has_color(self) -> bool {
        !matches!(self, Tool::Select | Tool::Crop | Tool::Picker | Tool::Blur | Tool::Spotlight)
    }
    pub fn has_fill(self) -> bool {
        matches!(self, Tool::Rect | Tool::Ellipse)
    }
    pub fn has_shadow(self) -> bool {
        matches!(self, Tool::Pen | Tool::Line | Tool::Arrow | Tool::Rect | Tool::Ellipse | Tool::Text | Tool::Step)
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub enum Kind {
    Pen { pts: Vec<(f32, f32)> },
    Highlight { pts: Vec<(f32, f32)> },
    Line { a: (f32, f32), b: (f32, f32) },
    Arrow { a: (f32, f32), b: (f32, f32) },
    Rect { r: R },
    Ellipse { r: R },
    Text { at: (f32, f32), text: String, bold: bool, italic: bool },
    Step { at: (f32, f32), n: u32 },
    Blur { r: R, pixelate: bool },
    Spotlight { r: R },
    /// A pasted picture; `id` = key in Doc::images
    Image { r: R, id: String },
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Shape {
    pub kind: Kind,
    /// straight RGBA
    pub color: [u8; 4],
    pub size: f32,
    pub fill: bool,
    pub shadow: bool,
}

/// What's drawn: the frozen picture + the shapes (+ pasted pictures).
#[derive(Clone)]
pub struct Doc {
    pub base: Arc<Pic>,
    pub shapes: Vec<Shape>,
    pub images: HashMap<String, Arc<Pic>>,
}

// ---------------------------------------------------------------------------------------------------------------
// Fonts (Noto Sans, SIL Open Font License - assets/fonts/OFL.txt)

pub struct Fonts {
    pub regular: FontRef<'static>,
    pub bold: FontRef<'static>,
    pub italic: FontRef<'static>,
    pub bold_italic: FontRef<'static>,
}

pub fn fonts() -> &'static Fonts {
    static F: OnceLock<Fonts> = OnceLock::new();
    F.get_or_init(|| Fonts {
        regular: FontRef::try_from_slice(include_bytes!("../assets/fonts/NotoSans-Regular.ttf")).expect("font"),
        bold: FontRef::try_from_slice(include_bytes!("../assets/fonts/NotoSans-Bold.ttf")).expect("font"),
        italic: FontRef::try_from_slice(include_bytes!("../assets/fonts/NotoSans-Italic.ttf")).expect("font"),
        bold_italic: FontRef::try_from_slice(include_bytes!("../assets/fonts/NotoSans-BoldItalic.ttf")).expect("font"),
    })
}

fn font_for(bold: bool, italic: bool) -> &'static FontRef<'static> {
    let f = fonts();
    match (bold, italic) {
        (true, true) => &f.bold_italic,
        (true, false) => &f.bold,
        (false, true) => &f.italic,
        _ => &f.regular,
    }
}

/// Size of a text block: (width, height, line height, ascent).
pub fn text_metrics(text: &str, px: f32, bold: bool, italic: bool) -> (f32, f32, f32, f32) {
    let f = font_for(bold, italic).as_scaled(PxScale::from(px));
    let lh = f.height() + f.line_gap();
    let mut w: f32 = 0.0;
    let mut lines = 0;
    for line in text.split('\n') {
        lines += 1;
        w = w.max(line_width(&f, line));
    }
    (w.max(px * 0.3), lh * lines.max(1) as f32, lh, f.ascent())
}

fn line_width<F: Font>(f: &ab_glyph::PxScaleFont<&F>, line: &str) -> f32 {
    let mut x = 0.0;
    let mut prev = None;
    for c in line.chars() {
        let g = f.glyph_id(c);
        if let Some(p) = prev {
            x += f.kern(p, g);
        }
        x += f.h_advance(g);
        prev = Some(g);
    }
    x
}

// ---------------------------------------------------------------------------------------------------------------
// Geometry helpers

impl Shape {
    /// Area the shape covers (picture px), shadows / halos / arrow heads included.
    pub fn bbox(&self) -> R {
        let pad = self.size / 2.0 + if self.shadow { 8.0 } else { 1.0 };
        match &self.kind {
            Kind::Pen { pts } | Kind::Highlight { pts } => {
                let mut r = R::new(pts[0].0, pts[0].1, 0.0, 0.0);
                for p in pts {
                    r = r.union(&R::new(p.0, p.1, 0.0, 0.0));
                }
                r.inflate(pad)
            }
            Kind::Line { a, b } => R::from_pts(*a, *b).inflate(pad),
            Kind::Arrow { a, b } => R::from_pts(*a, *b).inflate(pad + head_len(self.size)),
            Kind::Rect { r } | Kind::Ellipse { r } => r.inflate(pad),
            Kind::Blur { r, .. } | Kind::Spotlight { r } | Kind::Image { r, .. } => *r,
            Kind::Text { at, text, bold, italic } => {
                let (w, h, _, _) = text_metrics(text, self.size, *bold, *italic);
                R::new(at.0, at.1, w, h).inflate(self.size / 8.0 + 2.0)
            }
            Kind::Step { at, .. } => R::new(at.0 - self.size / 2.0, at.1 - self.size / 2.0, self.size, self.size).inflate(4.0),
        }
    }

    /// The rectangle its handles act on (rect-like shapes), None for the others.
    pub fn frame(&self) -> Option<R> {
        match &self.kind {
            Kind::Rect { r } | Kind::Ellipse { r } | Kind::Blur { r, .. } | Kind::Spotlight { r } | Kind::Image { r, .. } => Some(*r),
            _ => None,
        }
    }
    pub fn set_frame(&mut self, n: R) {
        match &mut self.kind {
            Kind::Rect { r } | Kind::Ellipse { r } | Kind::Blur { r, .. } | Kind::Spotlight { r } | Kind::Image { r, .. } => *r = n,
            _ => {}
        }
    }
    pub fn ends(&self) -> Option<((f32, f32), (f32, f32))> {
        match &self.kind {
            Kind::Line { a, b } | Kind::Arrow { a, b } => Some((*a, *b)),
            _ => None,
        }
    }
    pub fn set_ends(&mut self, na: (f32, f32), nb: (f32, f32)) {
        if let Kind::Line { a, b } | Kind::Arrow { a, b } = &mut self.kind {
            *a = na;
            *b = nb;
        }
    }

    pub fn translate(&mut self, dx: f32, dy: f32) {
        let mv = |p: &mut (f32, f32)| {
            p.0 += dx;
            p.1 += dy;
        };
        match &mut self.kind {
            Kind::Pen { pts } | Kind::Highlight { pts } => pts.iter_mut().for_each(mv),
            Kind::Line { a, b } | Kind::Arrow { a, b } => {
                mv(a);
                mv(b);
            }
            Kind::Rect { r } | Kind::Ellipse { r } | Kind::Blur { r, .. } | Kind::Spotlight { r } | Kind::Image { r, .. } => {
                *r = r.moved(dx, dy)
            }
            Kind::Text { at, .. } | Kind::Step { at, .. } => mv(at),
        }
    }

    /// Is the point on the shape? `tol` = extra slack in picture px (a few screen px).
    pub fn hit(&self, p: (f32, f32), tol: f32) -> bool {
        let t = self.size / 2.0 + tol;
        match &self.kind {
            Kind::Pen { pts } | Kind::Highlight { pts } => {
                let t = if matches!(self.kind, Kind::Highlight { .. }) { self.size / 2.0 + tol } else { t };
                if pts.len() == 1 {
                    return dist(p, pts[0]) <= t;
                }
                pts.windows(2).any(|w| seg_dist(p, w[0], w[1]) <= t)
            }
            Kind::Line { a, b } | Kind::Arrow { a, b } => seg_dist(p, *a, *b) <= t + 2.0,
            Kind::Rect { r } => {
                if self.fill {
                    r.inflate(t).contains(p)
                } else {
                    r.inflate(t).contains(p) && !r.inflate(-t).contains(p)
                }
            }
            Kind::Ellipse { r } => {
                let (cx, cy) = (r.x + r.w / 2.0, r.y + r.h / 2.0);
                let (rx, ry) = (r.w / 2.0 + t, r.h / 2.0 + t);
                if rx <= 0.0 || ry <= 0.0 {
                    return false;
                }
                let d = ((p.0 - cx) / rx).powi(2) + ((p.1 - cy) / ry).powi(2);
                if self.fill {
                    d <= 1.0
                } else {
                    let (ix, iy) = ((r.w / 2.0 - t).max(0.1), (r.h / 2.0 - t).max(0.1));
                    d <= 1.0 && ((p.0 - cx) / ix).powi(2) + ((p.1 - cy) / iy).powi(2) >= 1.0
                }
            }
            Kind::Blur { r, .. } | Kind::Spotlight { r } | Kind::Image { r, .. } => r.inflate(tol).contains(p),
            Kind::Text { .. } => self.bbox().inflate(tol).contains(p),
            Kind::Step { at, .. } => dist(p, *at) <= self.size / 2.0 + tol,
        }
    }
}

pub fn dist(a: (f32, f32), b: (f32, f32)) -> f32 {
    ((a.0 - b.0).powi(2) + (a.1 - b.1).powi(2)).sqrt()
}

fn seg_dist(p: (f32, f32), a: (f32, f32), b: (f32, f32)) -> f32 {
    let (dx, dy) = (b.0 - a.0, b.1 - a.1);
    let l2 = dx * dx + dy * dy;
    if l2 < 1e-6 {
        return dist(p, a);
    }
    let t = (((p.0 - a.0) * dx + (p.1 - a.1) * dy) / l2).clamp(0.0, 1.0);
    dist(p, (a.0 + t * dx, a.1 + t * dy))
}

pub fn head_len(size: f32) -> f32 {
    (size * 3.2).max(12.0)
}

// ---------------------------------------------------------------------------------------------------------------
// Drawing

fn paint(c: [u8; 4], alpha: f32) -> Paint<'static> {
    let mut p = Paint::default();
    p.set_color_rgba8(c[0], c[1], c[2], ((c[3] as f32) * alpha).round() as u8);
    p.anti_alias = true;
    p
}

fn stroke(w: f32) -> Stroke {
    Stroke { width: w.max(0.5), line_cap: LineCap::Round, line_join: LineJoin::Round, ..Stroke::default() }
}

fn smooth_path(pts: &[(f32, f32)]) -> Option<tiny_skia::Path> {
    let mut pb = PathBuilder::new();
    pb.move_to(pts[0].0, pts[0].1);
    if pts.len() == 1 {
        pb.line_to(pts[0].0 + 0.01, pts[0].1);
    } else if pts.len() == 2 {
        pb.line_to(pts[1].0, pts[1].1);
    } else {
        // quadratic curves through the midpoints: smooth pen lines without a point every pixel
        for i in 1..pts.len() - 1 {
            let (c, n) = (pts[i], pts[i + 1]);
            pb.quad_to(c.0, c.1, (c.0 + n.0) / 2.0, (c.1 + n.1) / 2.0);
        }
        let l = pts[pts.len() - 1];
        pb.line_to(l.0, l.1);
    }
    pb.finish()
}

fn rect_path(r: &R, radius: f32) -> Option<tiny_skia::Path> {
    let rad = radius.min(r.w / 2.0).min(r.h / 2.0).max(0.0);
    let mut pb = PathBuilder::new();
    if rad < 0.5 {
        pb.push_rect(tiny_skia::Rect::from_xywh(r.x, r.y, r.w.max(0.5), r.h.max(0.5))?);
        return pb.finish();
    }
    let (x0, y0, x1, y1) = (r.x, r.y, r.r(), r.b());
    let k = 0.4477 * rad; // cubic circle approximation
    pb.move_to(x0 + rad, y0);
    pb.line_to(x1 - rad, y0);
    pb.cubic_to(x1 - k, y0, x1, y0 + k, x1, y0 + rad);
    pb.line_to(x1, y1 - rad);
    pb.cubic_to(x1, y1 - k, x1 - k, y1, x1 - rad, y1);
    pb.line_to(x0 + rad, y1);
    pb.cubic_to(x0 + k, y1, x0, y1 - k, x0, y1 - rad);
    pb.line_to(x0, y0 + rad);
    pb.cubic_to(x0, y0 + k, x0 + k, y0, x0 + rad, y0);
    pb.close();
    pb.finish()
}

fn arrow_paths(a: (f32, f32), b: (f32, f32), size: f32) -> (Option<tiny_skia::Path>, Option<tiny_skia::Path>) {
    let len = dist(a, b).max(0.001);
    let (ux, uy) = ((b.0 - a.0) / len, (b.1 - a.1) / len);
    let hl = head_len(size).min(len * 0.9).max(4.0);
    let hw = hl * 0.62;
    let base = (b.0 - ux * hl, b.1 - uy * hl);
    let mut shaft = PathBuilder::new();
    shaft.move_to(a.0, a.1);
    // the line stops inside the head, so the round cap never sticks out of the tip
    shaft.line_to(base.0 + ux * hl * 0.3, base.1 + uy * hl * 0.3);
    let mut head = PathBuilder::new();
    head.move_to(b.0, b.1);
    head.line_to(base.0 - uy * hw, base.1 + ux * hw);
    head.line_to(base.0 + ux * hl * 0.12, base.1 + uy * hl * 0.12);
    head.line_to(base.0 + uy * hw, base.1 - ux * hw);
    head.close();
    (shaft.finish(), head.finish())
}

/// Draws the frozen picture's `clip` part + every shape into `out` (same size as `clip`).
pub fn render(doc: &Doc, clip: IR, out: &mut PixmapMut) {
    let base = &doc.base;
    let c = clip.clamp(base.w as i32, base.h as i32);
    let ow = out.width() as usize;
    // 1. the picture (outside it: dark, for library pictures smaller than the box)
    out.fill(tiny_skia::Color::from_rgba8(24, 24, 28, 255));
    {
        let data = out.data_mut();
        for y in 0..c.h {
            let s = ((c.y + y) as usize * base.w as usize + c.x as usize) * 4;
            let dy = (c.y + y - clip.y) as usize;
            let dx = (c.x - clip.x) as usize;
            let d = (dy * ow + dx) * 4;
            data[d..d + c.w as usize * 4].copy_from_slice(&base.px[s..s + c.w as usize * 4]);
        }
    }
    let tf = Transform::from_translate(-clip.x as f32, -clip.y as f32);
    let cr = clip.f();
    for s in &doc.shapes {
        let bb = s.bbox();
        if bb.r() < cr.x || bb.b() < cr.y || bb.x > cr.r() || bb.y > cr.b() {
            if !matches!(s.kind, Kind::Spotlight { .. }) {
                continue;
            }
        }
        draw_shape(doc, s, clip, tf, out);
    }
}

/// Only shape `s` on a transparent pixmap covering `area` (picture px): the live part while it's being dragged
pub fn render_one(doc: &Doc, s: &Shape, area: IR, out: &mut PixmapMut) {
    let tf = Transform::from_translate(-area.x as f32, -area.y as f32);
    draw_shape(doc, s, area, tf, out);
}

fn draw_shape(doc: &Doc, s: &Shape, clip: IR, tf: Transform, out: &mut PixmapMut) {
    if s.shadow && !matches!(s.kind, Kind::Text { .. } | Kind::Blur { .. } | Kind::Spotlight { .. } | Kind::Image { .. }) {
        draw_soft_shadow(s, clip, out);
    }
    match &s.kind {
        Kind::Pen { pts } => {
            if let Some(p) = smooth_path(pts) {
                out.stroke_path(&p, &paint(s.color, 1.0), &stroke(s.size), tf, None);
            }
        }
        Kind::Highlight { pts } => {
            if let Some(p) = smooth_path(pts) {
                let mut pt = paint(s.color, 0.45);
                pt.blend_mode = BlendMode::Multiply;
                let st = Stroke { width: s.size, line_cap: LineCap::Square, line_join: LineJoin::Round, ..Stroke::default() };
                out.stroke_path(&p, &pt, &st, tf, None);
            }
        }
        Kind::Line { a, b } => {
            let mut pb = PathBuilder::new();
            pb.move_to(a.0, a.1);
            pb.line_to(b.0, b.1);
            if let Some(p) = pb.finish() {
                out.stroke_path(&p, &paint(s.color, 1.0), &stroke(s.size), tf, None);
            }
        }
        Kind::Arrow { a, b } => {
            let (shaft, head) = arrow_paths(*a, *b, s.size);
            if let Some(p) = shaft {
                out.stroke_path(&p, &paint(s.color, 1.0), &stroke(s.size), tf, None);
            }
            if let Some(h) = head {
                out.fill_path(&h, &paint(s.color, 1.0), FillRule::Winding, tf, None);
                let st = Stroke { width: (s.size * 0.5).max(1.0), line_join: LineJoin::Round, ..Stroke::default() };
                out.stroke_path(&h, &paint(s.color, 1.0), &st, tf, None);
            }
        }
        Kind::Rect { r } => {
            if let Some(p) = rect_path(r, s.size.min(10.0)) {
                if s.fill {
                    out.fill_path(&p, &paint(s.color, 1.0), FillRule::Winding, tf, None);
                } else {
                    out.stroke_path(&p, &paint(s.color, 1.0), &stroke(s.size), tf, None);
                }
            }
        }
        Kind::Ellipse { r } => {
            if let Some(rect) = tiny_skia::Rect::from_xywh(r.x, r.y, r.w.max(0.5), r.h.max(0.5)) {
                if let Some(p) = PathBuilder::from_oval(rect) {
                    if s.fill {
                        out.fill_path(&p, &paint(s.color, 1.0), FillRule::Winding, tf, None);
                    } else {
                        out.stroke_path(&p, &paint(s.color, 1.0), &stroke(s.size), tf, None);
                    }
                }
            }
        }
        Kind::Text { at, text, bold, italic } => {
            if s.shadow {
                // halo in the opposite brightness (white around dark-ish colours, black around light ones)
                let lum = 0.3 * s.color[0] as f32 + 0.59 * s.color[1] as f32 + 0.11 * s.color[2] as f32;
                let halo = if lum > 150.0 { [0, 0, 0, 200] } else { [255, 255, 255, 230] };
                let d = (s.size / 14.0).max(1.2);
                for (ox, oy) in [(-d, 0.0), (d, 0.0), (0.0, -d), (0.0, d), (-d * 0.7, -d * 0.7), (d * 0.7, -d * 0.7), (-d * 0.7, d * 0.7), (d * 0.7, d * 0.7)] {
                    draw_text(out, clip, (at.0 + ox, at.1 + oy), text, s.size, *bold, *italic, halo);
                }
            }
            draw_text(out, clip, *at, text, s.size, *bold, *italic, s.color);
        }
        Kind::Step { at, n } => {
            let r = s.size / 2.0;
            if let Some(p) = PathBuilder::from_circle(at.0, at.1, r) {
                out.fill_path(&p, &paint(s.color, 1.0), FillRule::Winding, tf, None);
                let st = Stroke { width: (s.size / 14.0).max(1.5), ..Stroke::default() };
                out.stroke_path(&p, &paint([255, 255, 255, 255], 1.0), &st, tf, None);
            }
            let label = n.to_string();
            let px = s.size * if label.len() > 1 { 0.46 } else { 0.55 };
            let (w, h, _, _) = text_metrics(&label, px, true, false);
            let lum = 0.3 * s.color[0] as f32 + 0.59 * s.color[1] as f32 + 0.11 * s.color[2] as f32;
            let fg = if lum > 170.0 { [17, 17, 17, 255] } else { [255, 255, 255, 255] };
            draw_text(out, clip, (at.0 - w / 2.0, at.1 - h / 2.0), &label, px, true, false, fg);
        }
        Kind::Blur { r, pixelate } => {
            let ir = r.ir().intersect(&clip);
            if !ir.is_empty() {
                let local = IR { x: ir.x - clip.x, y: ir.y - clip.y, w: ir.w, h: ir.h };
                if *pixelate {
                    pixelate_px(out, local, s.size.max(2.0) as i32);
                } else {
                    redact_blur(out, local, s.size);
                }
            }
        }
        Kind::Spotlight { r } => {
            // dim everything in the picture except the spot
            let full = tiny_skia::Rect::from_xywh(clip.x as f32 - 1.0, clip.y as f32 - 1.0, clip.w as f32 + 2.0, clip.h as f32 + 2.0);
            let mut pb = PathBuilder::new();
            if let Some(f) = full {
                pb.push_rect(f);
            }
            if let Some(hole) = rect_path(r, 10.0) {
                // the hole: the rounded rect path, appended (even-odd fill cuts it out)
                let segs = hole;
                pb.push_path(&segs);
            }
            if let Some(p) = pb.finish() {
                out.fill_path(&p, &paint([0, 0, 0, 255], 0.6), FillRule::EvenOdd, tf, None);
            }
        }
        Kind::Image { r, id } => {
            if let Some(pic) = doc.images.get(id) {
                if let Some(src) = tiny_skia::PixmapRef::from_bytes(&pic.px, pic.w, pic.h) {
                    let sx = r.w / pic.w.max(1) as f32;
                    let sy = r.h / pic.h.max(1) as f32;
                    let t = tf.pre_translate(r.x, r.y).pre_scale(sx, sy);
                    let pp = PixmapPaint { quality: tiny_skia::FilterQuality::Bilinear, ..PixmapPaint::default() };
                    out.draw_pixmap(0, 0, src, &pp, t, None);
                }
            }
        }
    }
}

/// A soft drop shadow under a shape: the shape in black into a small pixmap, blurred, drawn 2 px right / 3 px down.
fn draw_soft_shadow(s: &Shape, clip: IR, out: &mut PixmapMut) {
    let bb = s.bbox().inflate(6.0).ir();
    if bb.w <= 0 || bb.h <= 0 || bb.w > 8000 || bb.h > 8000 {
        return;
    }
    let Some(mut tmp) = Pixmap::new(bb.w as u32, bb.h as u32) else { return };
    let mut sh = s.clone();
    sh.shadow = false;
    sh.color = [0, 0, 0, 255];
    let empty = Doc { base: Arc::new(Pic::new(1, 1)), shapes: vec![], images: HashMap::new() };
    let tf = Transform::from_translate(-bb.x as f32, -bb.y as f32);
    {
        let mut pm = tmp.as_mut();
        draw_shape(&empty, &sh, bb, tf, &mut pm);
    }
    // black shape: colour channels are all 0 (premultiplied), so only the alpha needs the blur - same result as
    // blur_px(.., 3), ~5x faster (dragging a shape with a shadow stayed smooth only this way, 20 Nov 2026)
    blur_alpha(tmp.data_mut(), bb.w as usize, bb.h as usize, 3);
    let pp = PixmapPaint { opacity: 0.45, ..PixmapPaint::default() };
    out.draw_pixmap(bb.x - clip.x + 2, bb.y - clip.y + 3, tmp.as_ref(), &pp, Transform::identity(), None);
}

/// Text onto the pixmap (glyph coverage blended by hand - tiny-skia has no text).
#[allow(clippy::too_many_arguments)]
pub fn draw_text(out: &mut PixmapMut, clip: IR, at: (f32, f32), text: &str, px: f32, bold: bool, italic: bool, color: [u8; 4]) {
    let font = font_for(bold, italic);
    let f = font.as_scaled(PxScale::from(px));
    let lh = f.height() + f.line_gap();
    let (w, h) = (out.width() as i32, out.height() as i32);
    let data = out.data_mut();
    let mut y = at.1 + f.ascent();
    for line in text.split('\n') {
        let mut x = at.0;
        let mut prev = None;
        for ch in line.chars() {
            let gid = f.glyph_id(ch);
            if let Some(p) = prev {
                x += f.kern(p, gid);
            }
            let g = gid.with_scale_and_position(PxScale::from(px), ab_glyph::point(x, y));
            if let Some(o) = font.outline_glyph(g) {
                let b = o.px_bounds();
                o.draw(|gx, gy, cov| {
                    let tx = b.min.x as i32 + gx as i32 - clip.x;
                    let ty = b.min.y as i32 + gy as i32 - clip.y;
                    if tx < 0 || ty < 0 || tx >= w || ty >= h {
                        return;
                    }
                    let a = (cov.min(1.0) * color[3] as f32 / 255.0).clamp(0.0, 1.0);
                    if a <= 0.0 {
                        return;
                    }
                    let i = ((ty * w + tx) * 4) as usize;
                    for k in 0..3 {
                        data[i + k] = (color[k] as f32 * a + data[i + k] as f32 * (1.0 - a)).round() as u8;
                    }
                    data[i + 3] = (255.0 * a + data[i + 3] as f32 * (1.0 - a)).round() as u8;
                });
            }
            x += f.h_advance(gid);
            prev = Some(gid);
        }
        y += lh;
    }
}

/// Pixelate: average blocks of `block` px (the pixels are replaced - nothing of the original is left).
pub fn pixelate_px(out: &mut PixmapMut, r: IR, block: i32) {
    let w = out.width() as i32;
    let r = r.clamp(w, out.height() as i32);
    let data = out.data_mut();
    let block = block.max(2);
    let mut by = r.y;
    while by < r.y + r.h {
        let bh = block.min(r.y + r.h - by);
        let mut bx = r.x;
        while bx < r.x + r.w {
            let bw = block.min(r.x + r.w - bx);
            let mut sum = [0u32; 4];
            for y in by..by + bh {
                for x in bx..bx + bw {
                    let i = ((y * w + x) * 4) as usize;
                    for k in 0..4 {
                        sum[k] += data[i + k] as u32;
                    }
                }
            }
            let n = (bw * bh) as u32;
            let avg = [(sum[0] / n) as u8, (sum[1] / n) as u8, (sum[2] / n) as u8, (sum[3] / n) as u8];
            for y in by..by + bh {
                for x in bx..bx + bw {
                    let i = ((y * w + x) * 4) as usize;
                    data[i..i + 4].copy_from_slice(&avg);
                }
            }
            bx += block;
        }
        by += block;
    }
}

/// The Blur tool: a safe redaction, not just a soft look - the area is pixelated first (blocks of at least 6 px and a
/// third of the area's height, so a line of text loses its letters), then blurred (radius at least 4).
pub fn redact_blur(out: &mut PixmapMut, r: IR, strength: f32) {
    let block = (strength as i32).max(6).max(r.h.min(r.w) / 3).max(2);
    pixelate_px(out, r, block);
    blur_px(out, r, (strength as i32).max(4));
}

/// Blur: 3 box-blur passes each way (close to a Gaussian). Also mixes in a coarse pixelation first for strong
/// settings, so big text can't be read back through a weak blur.
pub fn blur_px(out: &mut PixmapMut, r: IR, radius: i32) {
    let w = out.width() as i32;
    let r = r.clamp(w, out.height() as i32);
    if r.is_empty() {
        return;
    }
    let rad = radius.max(1);
    let data = out.data_mut();
    let (rw, rh) = (r.w as usize, r.h as usize);
    let mut buf = vec![0u8; rw * rh * 4];
    for y in 0..rh {
        let s = (((r.y as usize + y) * w as usize) + r.x as usize) * 4;
        buf[y * rw * 4..(y + 1) * rw * 4].copy_from_slice(&data[s..s + rw * 4]);
    }
    let mut tmp = vec![0u8; buf.len()];
    for _ in 0..3 {
        box_pass(&buf, &mut tmp, rw, rh, rad as usize, true);
        box_pass(&tmp, &mut buf, rw, rh, rad as usize, false);
    }
    for y in 0..rh {
        let s = (((r.y as usize + y) * w as usize) + r.x as usize) * 4;
        data[s..s + rw * 4].copy_from_slice(&buf[y * rw * 4..(y + 1) * rw * 4]);
    }
}

/// blur_px's 3 box passes each way, on the alpha channel only (RGBA data whose colour channels are 0). Identical
/// numbers: same clamped edges, same integer division.
fn blur_alpha(data: &mut [u8], w: usize, h: usize, r: usize) {
    if w == 0 || h == 0 {
        return;
    }
    let mut a: Vec<u8> = data.chunks_exact(4).map(|p| p[3]).collect();
    let mut t = vec![0u8; a.len()];
    let win = (2 * r + 1) as u32;
    for _ in 0..3 {
        // rows
        for y in 0..h {
            let row = &a[y * w..(y + 1) * w];
            let out = &mut t[y * w..(y + 1) * w];
            let mut acc: u32 = (0..=2 * r).map(|k| row[(k as isize - r as isize).clamp(0, w as isize - 1) as usize] as u32).sum();
            for x in 0..w {
                out[x] = (acc / win) as u8;
                let o = (x as isize - r as isize).clamp(0, w as isize - 1) as usize;
                let i = (x as isize + r as isize + 1).clamp(0, w as isize - 1) as usize;
                acc = acc + row[i] as u32 - row[o] as u32;
            }
        }
        // columns, all at once row by row (cache friendly)
        let mut acc = vec![0u32; w];
        for k in 0..=2 * r {
            let yy = (k as isize - r as isize).clamp(0, h as isize - 1) as usize;
            for x in 0..w {
                acc[x] += t[yy * w + x] as u32;
            }
        }
        for y in 0..h {
            let o = (y as isize - r as isize).clamp(0, h as isize - 1) as usize;
            let i = (y as isize + r as isize + 1).clamp(0, h as isize - 1) as usize;
            for x in 0..w {
                a[y * w + x] = (acc[x] / win) as u8;
                acc[x] = acc[x] + t[i * w + x] as u32 - t[o * w + x] as u32;
            }
        }
    }
    for (p, v) in data.chunks_exact_mut(4).zip(a) {
        p[3] = v;
    }
}

fn box_pass(src: &[u8], dst: &mut [u8], w: usize, h: usize, r: usize, horiz: bool) {
    let (n, lines) = if horiz { (w, h) } else { (h, w) };
    let idx = |line: usize, i: usize| -> usize { if horiz { (line * w + i) * 4 } else { (i * w + line) * 4 } };
    let win = (2 * r + 1) as u32;
    for line in 0..lines {
        let mut acc = [0u32; 4];
        // window [-r, r] with clamped edges
        for k in 0..=2 * r {
            let i = (k as isize - r as isize).clamp(0, n as isize - 1) as usize;
            let p = idx(line, i);
            for c in 0..4 {
                acc[c] += src[p + c] as u32;
            }
        }
        for i in 0..n {
            let p = idx(line, i);
            for c in 0..4 {
                dst[p + c] = (acc[c] / win) as u8;
            }
            let out_i = (i as isize - r as isize).clamp(0, n as isize - 1) as usize;
            let in_i = (i as isize + r as isize + 1).clamp(0, n as isize - 1) as usize;
            let (po, pi) = (idx(line, out_i), idx(line, in_i));
            for c in 0..4 {
                acc[c] = acc[c] + src[pi + c] as u32 - src[po + c] as u32;
            }
        }
    }
}

/// The whole box as a finished picture (export / copy).
pub fn flatten(doc: &Doc, boxr: IR) -> Pic {
    let b = boxr;
    let mut pm = Pixmap::new(b.w.max(1) as u32, b.h.max(1) as u32).expect("pixmap");
    {
        let mut m = pm.as_mut();
        render(doc, b, &mut m);
    }
    Pic { w: pm.width(), h: pm.height(), px: pm.take() }
}

#[cfg(test)]
mod tests {
    #[test]
    fn blur_alpha_matches_blur_px() {
        // a black anti-aliased stroke with transparent around it: alpha-only blur == the 4-channel blur
        let (w, h) = (37u32, 23u32);
        let mut a = tiny_skia::Pixmap::new(w, h).unwrap();
        let mut pb = tiny_skia::PathBuilder::new();
        pb.move_to(2.0, 3.0);
        pb.line_to(33.0, 19.0);
        let p = pb.finish().unwrap();
        let mut paint = tiny_skia::Paint::default();
        paint.set_color_rgba8(0, 0, 0, 255);
        a.stroke_path(&p, &paint, &tiny_skia::Stroke { width: 5.0, ..Default::default() }, tiny_skia::Transform::identity(), None);
        let mut b = a.clone();
        super::blur_px(&mut a.as_mut(), super::IR { x: 0, y: 0, w: w as i32, h: h as i32 }, 3);
        super::blur_alpha(b.data_mut(), w as usize, h as usize, 3);
        assert_eq!(a.data(), b.data());
    }

    use super::*;

    fn doc(w: u32, h: u32) -> Doc {
        let mut p = Pic::new(w, h);
        for i in 0..(w * h) as usize {
            p.px[i * 4] = (i % 251) as u8;
            p.px[i * 4 + 1] = 100;
            p.px[i * 4 + 2] = 50;
            p.px[i * 4 + 3] = 255;
        }
        Doc { base: Arc::new(p), shapes: vec![], images: HashMap::new() }
    }

    #[test]
    fn flatten_copies_the_box() {
        let d = doc(50, 40);
        let out = flatten(&d, IR { x: 10, y: 5, w: 20, h: 10 });
        assert_eq!((out.w, out.h), (20, 10));
        assert_eq!(&out.px[0..4], &d.base.at(10, 5));
    }

    #[test]
    fn pixelate_replaces_pixels() {
        let mut d = doc(64, 64);
        d.shapes.push(Shape { kind: Kind::Blur { r: R::new(0.0, 0.0, 32.0, 32.0), pixelate: true }, color: [0; 4], size: 8.0, fill: false, shadow: false });
        let out = flatten(&d, IR { x: 0, y: 0, w: 64, h: 64 });
        // every pixel in the first 8x8 block is the same
        let first = &out.px[0..4];
        for y in 0..8 {
            for x in 0..8 {
                let i = (y * 64 + x) * 4;
                assert_eq!(&out.px[i..i + 4], first);
            }
        }
        // outside the blur: untouched
        assert_eq!(&out.px[(40 * 64 + 40) * 4..(40 * 64 + 40) * 4 + 4], &d.base.at(40, 40));
    }

    #[test]
    fn blur_redacts_text_lines() {
        // A12: a weak blur over a text-like line leaves no letter detail: every column of the area looks the same
        let mut d = doc(120, 20);
        let mut p = Pic::new(120, 20);
        for y in 0..20u32 {
            for x in 0..120u32 {
                let i = ((y * 120 + x) * 4) as usize;
                let ink = (5..15).contains(&y) && (x * 7 + y * 3) % 5 < 2;
                let v = if ink { 0 } else { 255 };
                p.px[i..i + 4].copy_from_slice(&[v, v, v, 255]);
            }
        }
        d.base = Arc::new(p);
        d.shapes.push(Shape { kind: Kind::Blur { r: R::new(0.0, 0.0, 120.0, 20.0), pixelate: false }, color: [0; 4], size: 2.0, fill: false, shadow: false });
        let out = flatten(&d, IR { x: 0, y: 0, w: 120, h: 20 });
        let row: Vec<u8> = (0..120).map(|x| out.px[(10 * 120 + x) * 4]).collect();
        let (mn, mx) = (row.iter().min().unwrap(), row.iter().max().unwrap());
        assert!(mx - mn < 40, "letters still visible: {mn}..{mx}");
    }

    #[test]
    fn every_shape_draws_and_hits() {
        let mut d = doc(200, 200);
        let mk = |kind| Shape { kind, color: [239, 68, 68, 255], size: 4.0, fill: false, shadow: true };
        d.shapes = vec![
            mk(Kind::Pen { pts: vec![(10.0, 10.0), (20.0, 30.0), (40.0, 35.0)] }),
            mk(Kind::Highlight { pts: vec![(10.0, 50.0), (90.0, 50.0)] }),
            mk(Kind::Line { a: (10.0, 70.0), b: (90.0, 90.0) }),
            mk(Kind::Arrow { a: (100.0, 10.0), b: (180.0, 60.0) }),
            mk(Kind::Rect { r: R::new(100.0, 100.0, 50.0, 30.0) }),
            mk(Kind::Ellipse { r: R::new(20.0, 120.0, 50.0, 30.0) }),
            mk(Kind::Text { at: (20.0, 160.0), text: "Hi\nthere".into(), bold: true, italic: false }),
            mk(Kind::Step { at: (170.0, 170.0), n: 3 }),
            mk(Kind::Spotlight { r: R::new(0.0, 0.0, 100.0, 100.0) }),
        ];
        let out = flatten(&d, IR { x: 0, y: 0, w: 200, h: 200 });
        assert_eq!(out.px.len(), 200 * 200 * 4);
        assert!(d.shapes[4].hit((100.0, 115.0), 3.0));
        assert!(!d.shapes[4].hit((125.0, 115.0), 3.0));
        assert!(d.shapes[3].hit((140.0, 35.0), 3.0));
        assert!(d.shapes[7].hit((170.0, 170.0), 1.0));
    }
}
