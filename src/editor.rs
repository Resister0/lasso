//! The frozen screen's brain: picking the box, every tool, undo / redo, keys. No platform code and no Slint here,
//! so it is the same on Linux and Windows and can be tested on its own (tests at the end).
//!
//! Coordinates: `R` / points are PICTURE pixels (the frozen picture of one monitor, physical pixels). The screen
//! shows the picture through `View` (offset + zoom); pointer events come in as screen pixels of that monitor.

use crate::keys::{KeyEv, Keymap};
use crate::shapes::{dist, Doc, Kind, Pic, Shape, Tool, IR, R};
use std::collections::HashMap;
use std::sync::Arc;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Phase {
    /// No box yet (or being dragged out)
    Pick,
    /// Box picked: the tools are around it
    Edit,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum Mode {
    Area,
    Window,
    Screen,
}

impl Mode {
    pub fn index(self) -> i32 {
        match self {
            Mode::Area => 0,
            Mode::Window => 1,
            Mode::Screen => 2,
        }
    }
    pub fn from_index(i: i32) -> Mode {
        match i {
            1 => Mode::Window,
            2 => Mode::Screen,
            _ => Mode::Area,
        }
    }
}

/// Picture px -> screen px: screen = o + pic * s
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct View {
    pub ox: f32,
    pub oy: f32,
    pub s: f32,
}

impl Default for View {
    fn default() -> Self {
        View { ox: 0.0, oy: 0.0, s: 1.0 }
    }
}

impl View {
    /// Centred; shrunk to fit when the picture is bigger than the screen (library pictures).
    pub fn fit(pw: f32, ph: f32, sw: f32, sh: f32, margin: f32) -> View {
        let s = ((sw - 2.0 * margin) / pw).min((sh - 2.0 * margin) / ph).min(1.0).max(0.02);
        View { ox: ((sw - pw * s) / 2.0).round(), oy: ((sh - ph * s) / 2.0).round(), s }
    }
    pub fn to_pic(&self, p: (f32, f32)) -> (f32, f32) {
        ((p.0 - self.ox) / self.s, (p.1 - self.oy) / self.s)
    }
    pub fn to_screen(&self, p: (f32, f32)) -> (f32, f32) {
        (self.ox + p.0 * self.s, self.oy + p.1 * self.s)
    }
    pub fn rect(&self, r: &R) -> R {
        R::new(self.ox + r.x * self.s, self.oy + r.y * self.s, r.w * self.s, r.h * self.s)
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Style {
    pub color: [u8; 4],
    pub color_idx: i32,
    pub size: f32,
    pub fill: bool,
    pub shadow: bool,
}

pub const PALETTE: [[u8; 4]; 8] = [
    [0xef, 0x44, 0x44, 255],
    [0xf5, 0x9e, 0x0b, 255],
    [0xfa, 0xcc, 0x15, 255],
    [0x22, 0xc5, 0x5e, 255],
    [0x14, 0xa3, 0x8b, 255],
    [0x3b, 0x82, 0xf6, 255],
    [0xff, 0xff, 0xff, 255],
    [0x11, 0x11, 0x11, 255],
];

/// What the app has to do after an input event.
#[derive(Clone, Debug, PartialEq)]
pub enum Fx {
    Copy,
    CopyText,
    Redact,
    Paste,
    Save,
    SaveAs,
    Link,
    Pin,
    Open,
    Recent,
    Settings,
    Timer,
    Pointer,
    Background,
    /// Ctrl+Enter: copy + save + close
    CopySave,
    /// Ctrl+Alt+C: copy, the frozen screen stays
    CopyStay,
    /// Ctrl+Shift+B: the box of the last capture again
    LastBox,
    /// Ctrl+Alt+S: scrolling capture of the box
    Scroll,
    /// Enter / double-click / the ✓ button: the main action from the settings
    Done,
    /// Esc after a box exists: close now, keep everything for "Restore"
    Close,
    /// Esc / right click with nothing picked: just close
    Cancel,
    /// Shift+Esc: close and throw the capture away (no Restore, nothing in the library)
    Discard,
    /// Ctrl+N: drop this, pick again
    NewBox,
    /// A text for the clipboard (colour picker)
    Clip(String),
    Toast(String),
}

#[derive(Clone)]
struct Snap {
    boxr: Option<R>,
    shapes: Vec<Shape>,
    images: HashMap<String, Arc<Pic>>,
}

#[derive(Clone, Debug)]
enum Drag {
    NewBox { start: (f32, f32), moved: bool },
    MoveBox { last: (f32, f32) },
    /// handle: 0..8 = tl, t, tr, r, br, b, bl, l
    ResizeBox { handle: u8, orig: R, start: (f32, f32) },
    Draw { idx: usize, start: (f32, f32) },
    MoveSel { last: (f32, f32), moved: bool },
    ResizeShape { idx: usize, handle: u8, orig: Shape, start: (f32, f32) },
}

pub struct Editor {
    pub doc: Doc,
    pub view: View,
    /// Screen size (physical px of the monitor the frozen screen is on)
    pub screen: (f32, f32),
    pub phase: Phase,
    pub mode: Mode,
    pub boxr: Option<R>,
    pub tool: Tool,
    prev_tool: Tool,
    pub styles: HashMap<Tool, Style>,
    pub sel: Vec<usize>,
    /// The text shape being typed into
    pub typing: Option<usize>,
    undo: Vec<Snap>,
    redo: Vec<Snap>,
    /// the redo steps a checkpoint put aside: back when that step turns out to be no change (review D8)
    redo_stash: Vec<Snap>,
    drag: Option<Drag>,
    /// Mouse in picture px
    pub mouse: (f32, f32),
    pub mouse_in: bool,
    pub shift: bool,
    pub ctrl: bool,
    pub alt: bool,
    pub space: bool,
    /// Windows on this monitor, top first (picture px) - filled in after the freeze
    pub windows: Vec<R>,
    /// app / game name of each window in `windows` (same order)
    pub window_names: Vec<String>,
    pub hover_win: Option<R>,
    pub keys_sheet: bool,
    crop_backup: Option<R>,
    pub pixelate: bool,
    pub keymap: Keymap,
    /// Bumped whenever what's drawn inside the box changes (the layer must be drawn again)
    pub version: u64,
    /// Pasted picture counter
    next_img: u32,
    /// Window mode: a click picks the window under the mouse (false while the list isn't there yet)
    pub last_double: bool,
    /// Ctrl+H: all bars hidden (only the picture, the box and the shapes)
    pub bars_hidden: bool,
    /// Shift held on the last key (Shift+[ ] = big size steps)
    last_shift: bool,
    /// physical screen px per logical px of the monitor (HiDPI: grab distances / click-vs-drag in logical px)
    pub ui_scale: f32,
    /// the text being typed was made new by this typing (empty at the end = drop it AND its undo step)
    typing_new: bool,
    /// the text before this typing began (re-edit unchanged = no undo step left behind)
    typing_was: Option<Shape>,
    /// repeated small changes (wheel / [ ] on the same shapes) share one undo step
    last_tag: Option<String>,
}

impl Editor {
    pub fn new(base: Arc<Pic>, screen: (f32, f32), keymap: Keymap) -> Editor {
        let mut styles = HashMap::new();
        for t in Tool::ALL {
            let (_, _, def) = t.size_range();
            let (color, idx) = match t {
                Tool::Highlight => (PALETTE[2], 2),
                _ => (PALETTE[0], 0),
            };
            styles.insert(t, Style { color, color_idx: idx, size: def, fill: false, shadow: matches!(t, Tool::Arrow | Tool::Text | Tool::Step) });
        }
        let view = if base.w as f32 == screen.0 && base.h as f32 == screen.1 {
            View::default()
        } else {
            View::fit(base.w as f32, base.h as f32, screen.0, screen.1, 60.0)
        };
        Editor {
            doc: Doc { base, shapes: vec![], images: HashMap::new() },
            view,
            screen,
            phase: Phase::Pick,
            mode: Mode::Area,
            boxr: None,
            tool: Tool::Arrow,
            prev_tool: Tool::Arrow,
            styles,
            sel: vec![],
            typing: None,
            undo: vec![],
            redo: vec![],
            redo_stash: vec![],
            drag: None,
            mouse: (0.0, 0.0),
            mouse_in: false,
            shift: false,
            ctrl: false,
            alt: false,
            space: false,
            windows: vec![],
            window_names: vec![],
            hover_win: None,
            keys_sheet: false,
            crop_backup: None,
            pixelate: true,
            keymap,
            version: 1,
            next_img: 1,
            last_double: false,
            bars_hidden: false,
            last_shift: false,
            ui_scale: 1.0,
            typing_new: false,
            typing_was: None,
            last_tag: None,
        }
    }

    pub fn pic_rect(&self) -> R {
        R::new(0.0, 0.0, self.doc.base.w as f32, self.doc.base.h as f32)
    }

    pub fn style(&self) -> Style {
        self.styles[&self.tool]
    }

    pub fn box_ir(&self) -> Option<IR> {
        self.boxr.map(|b| b.ir().clamp(self.doc.base.w as i32, self.doc.base.h as i32)).filter(|b| !b.is_empty())
    }

    pub fn can_undo(&self) -> bool {
        !self.undo.is_empty()
    }
    pub fn can_redo(&self) -> bool {
        !self.redo.is_empty()
    }
    /// The one shape being drawn / moved / resized right now, when it's the top shape and draws on its own (not
    /// blended with the picture below): the screen then repaints only that shape on each move (fast drags)
    pub fn live_shape(&self) -> Option<usize> {
        let idx = match &self.drag {
            Some(Drag::Draw { idx, .. }) | Some(Drag::ResizeShape { idx, .. }) => *idx,
            Some(Drag::MoveSel { .. }) if self.sel.len() == 1 => self.sel[0],
            _ => return None,
        };
        if idx + 1 != self.doc.shapes.len() {
            return None;
        }
        match self.doc.shapes[idx].kind {
            Kind::Highlight { .. } | Kind::Blur { .. } | Kind::Spotlight { .. } => None,
            _ => Some(idx),
        }
    }

    pub fn dragging_box(&self) -> bool {
        matches!(self.drag, Some(Drag::NewBox { .. }))
    }
    pub fn drawing(&self) -> bool {
        matches!(self.drag, Some(Drag::Draw { .. }) | Some(Drag::MoveSel { .. }) | Some(Drag::ResizeShape { .. }))
    }

    fn snap(&self) -> Snap {
        Snap { boxr: self.boxr, shapes: self.doc.shapes.clone(), images: self.doc.images.clone() }
    }
    fn checkpoint(&mut self) {
        self.last_tag = None;
        self.undo.push(self.snap());
        // 300 steps: plenty, and a closed capture kept for Restore stays small
        if self.undo.len() > 300 {
            self.undo.remove(0);
        }
        self.redo_stash = std::mem::take(&mut self.redo);
    }

    /// The step just checkpointed changed nothing (a click, a look): its undo step goes, the redo steps come back
    fn drop_checkpoint(&mut self) -> Option<Snap> {
        let s = self.undo.pop();
        self.redo = std::mem::take(&mut self.redo_stash);
        s
    }

    /// One undo step for a run of the same small change (size by wheel / [ ], ...)
    fn checkpoint_tag(&mut self, tag: String) {
        if self.last_tag.as_ref() == Some(&tag) {
            return;
        }
        self.checkpoint();
        self.last_tag = Some(tag);
    }
    fn changed(&mut self) {
        self.version += 1;
    }
    fn restore(&mut self, s: Snap) {
        self.boxr = s.boxr;
        self.doc.shapes = s.shapes;
        self.doc.images = s.images;
        self.sel.retain(|&i| i < self.doc.shapes.len());
        self.typing = None;
        self.drag = None;
        self.last_tag = None;
        self.phase = if self.boxr.is_some() { Phase::Edit } else { Phase::Pick };
        self.changed();
    }
    pub fn undo(&mut self) {
        self.typing = None;
        if let Some(s) = self.undo.pop() {
            self.redo.push(self.snap());
            self.restore(s);
        }
    }
    pub fn redo(&mut self) {
        if let Some(s) = self.redo.pop() {
            self.undo.push(self.snap());
            self.restore(s);
        }
    }

    fn clamp_box(&self, r: R) -> R {
        let p = self.pic_rect();
        let x0 = r.x.clamp(p.x, p.r());
        let y0 = r.y.clamp(p.y, p.b());
        let x1 = r.r().clamp(p.x, p.r());
        let y1 = r.b().clamp(p.y, p.b());
        R::new(x0, y0, x1 - x0, y1 - y0)
    }

    /// Screen px -> picture px, and the handle grab distance in picture px
    fn pic(&self, sx: f32, sy: f32) -> (f32, f32) {
        self.view.to_pic((sx, sy))
    }
    fn tol(&self) -> f32 {
        9.0 * self.ui_scale / self.view.s
    }

    fn handles(r: &R) -> [(f32, f32); 8] {
        let (cx, cy) = (r.x + r.w / 2.0, r.y + r.h / 2.0);
        [(r.x, r.y), (cx, r.y), (r.r(), r.y), (r.r(), cy), (r.r(), r.b()), (cx, r.b()), (r.x, r.b()), (r.x, cy)]
    }
    fn hit_handle(&self, r: &R, p: (f32, f32)) -> Option<u8> {
        let t = self.tol();
        Self::handles(r).iter().position(|h| (h.0 - p.0).abs() <= t && (h.1 - p.1).abs() <= t).map(|i| i as u8)
    }
    fn apply_handle(orig: &R, handle: u8, dx: f32, dy: f32) -> R {
        let (mut x0, mut y0, mut x1, mut y1) = (orig.x, orig.y, orig.r(), orig.b());
        match handle {
            0 => {
                x0 += dx;
                y0 += dy
            }
            1 => y0 += dy,
            2 => {
                x1 += dx;
                y0 += dy
            }
            3 => x1 += dx,
            4 => {
                x1 += dx;
                y1 += dy
            }
            5 => y1 += dy,
            6 => {
                x0 += dx;
                y1 += dy
            }
            _ => x0 += dx,
        }
        R::from_pts((x0, y0), (x1, y1))
    }

    pub fn window_at(&self, p: (f32, f32)) -> Option<R> {
        self.windows.iter().find(|w| w.contains(p)).map(|w| self.clamp_box(*w)).filter(|w| w.w > 2.0 && w.h > 2.0)
    }

    /// The box is picked: tools appear.
    fn take_box(&mut self, r: R) {
        self.boxr = Some(self.clamp_box(r));
        self.phase = Phase::Edit;
        self.hover_win = None;
        self.changed();
    }

    // -----------------------------------------------------------------------------------------------------------
    // Pointer

    pub fn pointer_down(&mut self, sx: f32, sy: f32, button: u8) -> Vec<Fx> {
        let p = self.pic(sx, sy);
        self.mouse = p;
        let mut fx = vec![];
        if button == 2 {
            // right click: cancel while picking; nothing once the box exists
            if self.phase == Phase::Pick {
                fx.push(if self.boxr.is_some() { Fx::Close } else { Fx::Cancel });
            }
            return fx;
        }
        if button != 1 {
            return fx;
        }
        if self.keys_sheet {
            self.keys_sheet = false;
            return fx;
        }
        if self.phase == Phase::Pick {
            if self.mode == Mode::Screen {
                self.take_box(self.pic_rect());
                return fx;
            }
            self.drag = Some(Drag::NewBox { start: p, moved: false });
            self.boxr = Some(R::new(p.0, p.1, 0.0, 0.0));
            return fx;
        }
        // typing: a click on another spot ends it (the click then goes on as usual)
        if let Some(t) = self.typing {
            let inside = self.doc.shapes.get(t).map(|s| s.hit(p, self.tol())).unwrap_or(false);
            if inside && self.tool == Tool::Text {
                return fx;
            }
            self.stop_typing();
        }
        let b = self.boxr.unwrap_or_default();
        // 1. the box's own handles (every tool)
        if self.tool != Tool::Picker {
            if let Some(h) = self.hit_handle(&b, p) {
                self.checkpoint();
                self.drag = Some(Drag::ResizeBox { handle: h, orig: b, start: p });
                return fx;
            }
        }
        // 2. handles of the one selected shape
        // (only with the Select tool: with a drawing tool, a new shape right next to the last one - blurring row
        // after row - must not grab the last one's handle; found in the headless test, 13 Nov 2026)
        if self.sel.len() == 1 && self.tool == Tool::Select {
            let idx = self.sel[0];
            if let Some(s) = self.doc.shapes.get(idx).cloned() {
                if let Some(fr) = s.frame() {
                    if let Some(h) = self.hit_handle(&fr, p) {
                        self.checkpoint();
                        self.drag = Some(Drag::ResizeShape { idx, handle: h, orig: s, start: p });
                        return fx;
                    }
                } else if let Some((a, bb)) = s.ends() {
                    let t = self.tol();
                    let h = if dist(a, p) <= t {
                        Some(0)
                    } else if dist(bb, p) <= t {
                        Some(4)
                    } else {
                        None
                    };
                    if let Some(h) = h {
                        self.checkpoint();
                        self.drag = Some(Drag::ResizeShape { idx, handle: h, orig: s, start: p });
                        return fx;
                    }
                }
            }
        }
        // a drag that starts outside the box = a new box (the user, 21 Nov 2026: "need to be able to select an area when
        // an area's already been selected"); the shapes stay where they are on the picture, Ctrl+Z brings the old box back
        if !b.contains(p) && !matches!(self.tool, Tool::Crop | Tool::Picker) {
            self.stop_typing();
            self.sel.clear();
            self.checkpoint();
            self.drag = Some(Drag::NewBox { start: p, moved: false });
            return fx;
        }
        let st = self.style();
        match self.tool {
            Tool::Crop => {
                if self.crop_backup.is_none() {
                    self.crop_backup = self.boxr;
                }
                self.checkpoint();
                self.drag = Some(Drag::NewBox { start: p, moved: false });
            }
            Tool::Select => {
                if let Some(i) = self.shape_at(p) {
                    if self.shift {
                        if let Some(k) = self.sel.iter().position(|&s| s == i) {
                            self.sel.remove(k);
                        } else {
                            self.sel.push(i);
                        }
                    } else if !self.sel.contains(&i) {
                        self.sel = vec![i];
                    }
                    self.checkpoint();
                    self.drag = Some(Drag::MoveSel { last: p, moved: false });
                } else {
                    self.sel.clear();
                    if b.contains(p) {
                        self.checkpoint();
                        self.drag = Some(Drag::MoveBox { last: p });
                    }
                }
            }
            Tool::Picker => {
                let c = self.doc.base.at(p.0 as i32, p.1 as i32);
                let hex = format!("#{:02x}{:02x}{:02x}", c[0], c[1], c[2]);
                fx.push(Fx::Clip(hex.clone()));
                fx.push(Fx::Toast(format!("Copied {hex} - the drawing tools use it now")));
                self.pick_colour([c[0], c[1], c[2], 255]);
            }
            Tool::Text => {
                if let Some(i) = self.shape_at(p).filter(|&i| matches!(self.doc.shapes[i].kind, Kind::Text { .. })) {
                    self.start_typing(i, false);
                } else if b.contains(p) {
                    self.checkpoint();
                    let (_, h, _, _) = crate::shapes::text_metrics("A", st.size, false, false);
                    self.doc.shapes.push(Shape {
                        kind: Kind::Text { at: (p.0, p.1 - h / 2.0), text: String::new(), bold: false, italic: false },
                        color: st.color,
                        size: st.size,
                        fill: false,
                        shadow: st.shadow,
                    });
                    let i = self.doc.shapes.len() - 1;
                    self.start_typing(i, true);
                    self.changed();
                }
            }
            Tool::Step => {
                if b.contains(p) {
                    self.checkpoint();
                    let n = self.doc.shapes.iter().filter_map(|s| if let Kind::Step { n, .. } = s.kind { Some(n) } else { None }).max().unwrap_or(0) + 1;
                    self.doc.shapes.push(Shape { kind: Kind::Step { at: p, n }, color: st.color, size: st.size, fill: true, shadow: st.shadow });
                    self.sel = vec![self.doc.shapes.len() - 1];
                    // a step can be dragged into place right away
                    self.drag = Some(Drag::MoveSel { last: p, moved: true });
                    self.changed();
                }
            }
            t => {
                if !b.contains(p) {
                    return fx;
                }
                self.checkpoint();
                let kind = match t {
                    Tool::Pen => Kind::Pen { pts: vec![p] },
                    Tool::Highlight => Kind::Highlight { pts: vec![p] },
                    Tool::Line => Kind::Line { a: p, b: p },
                    Tool::Arrow => Kind::Arrow { a: p, b: p },
                    Tool::Rect => Kind::Rect { r: R::new(p.0, p.1, 0.0, 0.0) },
                    Tool::Ellipse => Kind::Ellipse { r: R::new(p.0, p.1, 0.0, 0.0) },
                    Tool::Blur => Kind::Blur { r: R::new(p.0, p.1, 0.0, 0.0), pixelate: self.pixelate },
                    _ => Kind::Spotlight { r: R::new(p.0, p.1, 0.0, 0.0) },
                };
                self.doc.shapes.push(Shape { kind, color: st.color, size: st.size, fill: st.fill, shadow: st.shadow && t.has_shadow() });
                let idx = self.doc.shapes.len() - 1;
                self.sel = vec![idx];
                self.drag = Some(Drag::Draw { idx, start: p });
                self.changed();
            }
        }
        fx
    }

    fn shape_at(&self, p: (f32, f32)) -> Option<usize> {
        let t = (4.0 / self.view.s).max(2.0);
        (0..self.doc.shapes.len()).rev().find(|&i| self.doc.shapes[i].hit(p, t))
    }

    pub fn pointer_move(&mut self, sx: f32, sy: f32) {
        let p = self.pic(sx, sy);
        let last_mouse = self.mouse;
        self.mouse = p;
        self.mouse_in = true;
        let Some(drag) = self.drag.clone() else {
            if self.phase == Phase::Pick && self.mode != Mode::Screen {
                self.hover_win = if self.mode == Mode::Window { self.window_at(p) } else { None };
            }
            return;
        };
        match drag {
            Drag::NewBox { start, moved } => {
                if self.space {
                    // hold Space: move the box instead of resizing (macOS)
                    let (dx, dy) = (p.0 - last_mouse.0, p.1 - last_mouse.1);
                    if let Some(b) = self.boxr {
                        self.boxr = Some(b.moved(dx, dy));
                    }
                    self.drag = Some(Drag::NewBox { start: (start.0 + dx, start.1 + dy), moved: true });
                } else {
                    let mut q = p;
                    if self.shift {
                        let d = (p.0 - start.0).abs().max((p.1 - start.1).abs());
                        q = (start.0 + d * (p.0 - start.0).signum(), start.1 + d * (p.1 - start.1).signum());
                    }
                    self.boxr = Some(self.clamp_box(R::from_pts(start, q)));
                    let mv = moved || dist(start, p) * self.view.s > 3.0 * self.ui_scale;
                    self.drag = Some(Drag::NewBox { start, moved: mv });
                }
                self.changed();
            }
            Drag::MoveBox { last } => {
                if let Some(b) = self.boxr {
                    let pr = self.pic_rect();
                    let nx = (b.x + p.0 - last.0).clamp(0.0, (pr.w - b.w).max(0.0));
                    let ny = (b.y + p.1 - last.1).clamp(0.0, (pr.h - b.h).max(0.0));
                    self.boxr = Some(R::new(nx, ny, b.w, b.h));
                    self.drag = Some(Drag::MoveBox { last: (last.0 + nx - b.x, last.1 + ny - b.y) });
                    self.changed();
                }
            }
            Drag::ResizeBox { handle, orig, start } => {
                let r = Self::apply_handle(&orig, handle, p.0 - start.0, p.1 - start.1);
                self.boxr = Some(self.clamp_box(r));
                self.changed();
            }
            Drag::MoveSel { last, .. } => {
                let (dx, dy) = (p.0 - last.0, p.1 - last.1);
                for &i in &self.sel.clone() {
                    if let Some(s) = self.doc.shapes.get_mut(i) {
                        s.translate(dx, dy);
                    }
                }
                self.drag = Some(Drag::MoveSel { last: p, moved: true });
                self.changed();
            }
            Drag::ResizeShape { idx, handle, orig, start } => {
                let (dx, dy) = (p.0 - start.0, p.1 - start.1);
                if let Some(s) = self.doc.shapes.get_mut(idx) {
                    if let Some(fr) = orig.frame() {
                        s.set_frame(Self::apply_handle(&fr, handle, dx, dy));
                    } else if let Some((a, b)) = orig.ends() {
                        if handle == 0 {
                            s.set_ends((a.0 + dx, a.1 + dy), b);
                        } else {
                            s.set_ends(a, (b.0 + dx, b.1 + dy));
                        }
                    }
                }
                self.changed();
            }
            Drag::Draw { idx, start } => {
                let (shift, alt) = (self.shift, self.alt);
                let tol = 1.5 / self.view.s;
                if let Some(s) = self.doc.shapes.get_mut(idx) {
                    match &mut s.kind {
                        Kind::Pen { pts } | Kind::Highlight { pts } => {
                            if shift && pts.len() >= 1 {
                                // Shift: a straight line from the first point
                                let a = pts[0];
                                *pts = vec![a, snap45(a, p)];
                            } else if dist(*pts.last().unwrap(), p) >= tol {
                                pts.push(p);
                            }
                        }
                        Kind::Line { a, b } | Kind::Arrow { a, b } => {
                            *b = if shift { snap45(*a, p) } else { p };
                            if alt {
                                *a = (2.0 * start.0 - b.0, 2.0 * start.1 - b.1);
                            }
                        }
                        Kind::Rect { r } | Kind::Ellipse { r } | Kind::Blur { r, .. } | Kind::Spotlight { r } => {
                            let mut q = p;
                            if shift {
                                let d = (p.0 - start.0).abs().max((p.1 - start.1).abs());
                                q = (start.0 + d * (p.0 - start.0).signum(), start.1 + d * (p.1 - start.1).signum());
                            }
                            *r = if alt { R::from_pts((2.0 * start.0 - q.0, 2.0 * start.1 - q.1), q) } else { R::from_pts(start, q) };
                        }
                        _ => {}
                    }
                }
                self.changed();
            }
        }
    }

    pub fn pointer_up(&mut self, sx: f32, sy: f32) -> Vec<Fx> {
        let p = self.pic(sx, sy);
        self.mouse = p;
        let fx = vec![];
        let Some(drag) = self.drag.take() else { return fx };
        match drag {
            Drag::NewBox { moved, .. } => {
                // a new box drawn while editing (started outside the old one): a click or a tiny drag keeps the old box
                if self.phase == Phase::Edit && self.tool != Tool::Crop {
                    let tiny = self.boxr.map(|b| b.w < 2.0 || b.h < 2.0).unwrap_or(true);
                    if !moved || tiny {
                        if let Some(s) = self.drop_checkpoint() {
                            self.boxr = s.boxr;
                        }
                    } else if let Some(b) = self.boxr {
                        self.take_box(b);
                    }
                    self.changed();
                    return fx;
                }
                if self.tool == Tool::Crop && self.phase == Phase::Edit {
                    if !moved {
                        // a click with the crop tool: keep the old box
                        if let Some(s) = self.drop_checkpoint() {
                            self.boxr = s.boxr;
                        }
                    }
                    self.changed();
                    return fx;
                }
                if !moved {
                    // a click without dragging: Window mode = the window under the mouse, else the whole monitor;
                    // Area mode = nothing, only a drag makes a box (the user, 20 Nov 2026: no automatic window grab)
                    if self.mode == Mode::Window {
                        let r = self.window_at(p).unwrap_or_else(|| self.pic_rect());
                        self.take_box(r);
                    } else {
                        self.boxr = None;
                        self.changed();
                    }
                } else if let Some(b) = self.boxr {
                    if b.w < 2.0 || b.h < 2.0 {
                        self.boxr = None;
                    } else {
                        self.take_box(b);
                    }
                }
            }
            Drag::Draw { idx, .. } => {
                let keep = self.doc.shapes.get(idx).map(|s| match &s.kind {
                    Kind::Line { a, b } | Kind::Arrow { a, b } => dist(*a, *b) * self.view.s >= 3.0 * self.ui_scale,
                    Kind::Rect { r } | Kind::Ellipse { r } | Kind::Blur { r, .. } | Kind::Spotlight { r } => r.w * self.view.s >= 3.0 * self.ui_scale && r.h * self.view.s >= 3.0 * self.ui_scale,
                    _ => true,
                });
                if keep == Some(false) {
                    self.doc.shapes.remove(idx);
                    self.sel.clear();
                    self.drop_checkpoint();
                    self.changed();
                }
            }
            Drag::MoveSel { moved, .. } => {
                if !moved {
                    self.drop_checkpoint();
                }
            }
            Drag::ResizeBox { orig, .. } => {
                if self.boxr == Some(orig) {
                    self.drop_checkpoint();
                }
            }
            Drag::MoveBox { .. } | Drag::ResizeShape { .. } => {
                // a click on the box / a handle without moving: no empty undo step (review D8)
                if self.undo.last().is_some_and(|u| u.boxr == self.boxr && u.shapes == self.doc.shapes) {
                    self.drop_checkpoint();
                }
            }
        }
        fx
    }

    pub fn double_click(&mut self, sx: f32, sy: f32) -> Vec<Fx> {
        let p = self.pic(sx, sy);
        if self.phase != Phase::Edit {
            return vec![];
        }
        if let Some(i) = self.shape_at(p) {
            if matches!(self.doc.shapes[i].kind, Kind::Text { .. }) {
                self.start_typing(i, false);
                return vec![];
            }
        }
        if matches!(self.tool, Tool::Select | Tool::Crop) && self.boxr.map(|b| b.contains(p)).unwrap_or(false) {
            return vec![Fx::Done];
        }
        vec![]
    }

    /// Wheel: Ctrl = zoom around the mouse, else the size of the tool / selection
    pub fn wheel(&mut self, sx: f32, sy: f32, dy: f32) {
        if self.ctrl {
            let f = if dy > 0.0 { 1.0 / 1.15 } else { 1.15 };
            self.zoom_at(sx, sy, self.view.s * f);
        } else if self.phase == Phase::Edit {
            self.bump_size(if dy > 0.0 { -1.0 } else { 1.0 });
        }
    }

    pub fn zoom_at(&mut self, sx: f32, sy: f32, s: f32) {
        let s = s.clamp(0.05, 16.0);
        let p = self.view.to_pic((sx, sy));
        self.view = View { ox: sx - p.0 * s, oy: sy - p.1 * s, s };
        self.changed();
    }

    // -----------------------------------------------------------------------------------------------------------
    // Keys

    pub fn set_mods(&mut self, ctrl: bool, shift: bool, alt: bool) {
        self.ctrl = ctrl;
        self.shift = shift;
        self.alt = alt;
    }

    /// Typing into text shape `i`: a new one (its undo step is already there) or an old one (one undo step now)
    fn start_typing(&mut self, i: usize, new: bool) {
        if !new {
            self.checkpoint();
        }
        self.typing_new = new;
        // the whole shape: bold / italic / colour changes while re-editing count too (review D6)
        self.typing_was = if new { None } else { self.doc.shapes.get(i).cloned() };
        self.typing = Some(i);
        self.sel = vec![i];
    }

    pub fn stop_typing(&mut self) {
        if let Some(t) = self.typing.take() {
            if let Some(Kind::Text { text, .. }) = self.doc.shapes.get(t).map(|s| &s.kind) {
                let unchanged = !self.typing_new && self.typing_was.as_ref() == self.doc.shapes.get(t);
                if text.trim().is_empty() {
                    // an empty text box goes away; a NEW one leaves no undo step (an emptied old one does: undo brings it back)
                    self.doc.shapes.remove(t);
                    self.sel.clear();
                    if self.typing_new {
                        self.drop_checkpoint();
                    }
                } else if unchanged {
                    // looked at, not changed: no empty undo step
                    self.drop_checkpoint();
                }
            }
            self.changed();
        }
    }

    /// Ends whatever the mouse was doing (Esc / close in the middle of a drag) and the typing - nothing follows the
    /// mouse after a Restore
    pub fn settle(&mut self) {
        if self.drag.is_some() {
            let m = self.view.to_screen(self.mouse);
            let _ = self.pointer_up(m.0, m.1);
        }
        self.drag = None;
        self.stop_typing();
        self.space = false;
    }

    pub fn key_up(&mut self, ev: &KeyEv) {
        if ev.is("space") {
            self.space = false;
        }
    }

    pub fn key(&mut self, ev: &KeyEv) -> Vec<Fx> {
        if ev.is("space") && self.typing.is_none() {
            self.space = true;
            if self.dragging_box() {
                return vec![];
            }
        }
        // typing: plain keys are text
        if let Some(t) = self.typing {
            if ev.is("Escape") {
                self.stop_typing();
                return vec![];
            }
            let ta = self.keymap.lookup(ev).map(|s| s.to_string());
            if matches!(ta.as_deref(), Some("bold") | Some("italic")) {
                let bold = ta.as_deref() == Some("bold");
                if let Some(Kind::Text { bold: b, italic: i, .. }) = self.doc.shapes.get_mut(t).map(|s| &mut s.kind) {
                    if bold {
                        *b = !*b
                    } else {
                        *i = !*i
                    }
                }
                self.changed();
                return vec![];
            }
            if ev.plain() {
                if let Some(Kind::Text { text, .. }) = self.doc.shapes.get_mut(t).map(|s| &mut s.kind) {
                    if ev.is("BackSpace") {
                        text.pop();
                    } else if ev.is("Return") {
                        text.push('\n');
                    } else if !ev.text.is_empty() {
                        text.push_str(&ev.text);
                    } else {
                        return vec![];
                    }
                }
                self.changed();
                return vec![];
            }
        }
        // arrows: the selected shapes, else the box (Alt = resize from the bottom-right corner)
        for (name, dx, dy) in [("Left", -1.0, 0.0), ("Right", 1.0, 0.0), ("Up", 0.0, -1.0), ("Down", 0.0, 1.0)] {
            if ev.is(name) && (!ev.ctrl || ev.alt) {
                let step = if ev.shift { 10.0 } else { 1.0 };
                let (dx, dy) = (dx * step, dy * step);
                if !self.sel.is_empty() && !ev.alt && !ev.ctrl {
                    if !ev.repeat {
                        self.checkpoint();
                    }
                    for &i in &self.sel.clone() {
                        if let Some(s) = self.doc.shapes.get_mut(i) {
                            s.translate(dx, dy);
                        }
                    }
                } else if let Some(b) = self.boxr {
                    if !ev.repeat {
                        self.checkpoint();
                    }
                    // Alt: resize from the bottom-right corner; Ctrl+Alt: from the top-left corner
                    let nb = if ev.alt && ev.ctrl {
                        R::new(b.x + dx, b.y + dy, (b.w - dx).max(1.0), (b.h - dy).max(1.0))
                    } else if ev.alt {
                        R::new(b.x, b.y, (b.w + dx).max(1.0), (b.h + dy).max(1.0))
                    } else {
                        b.moved(dx, dy)
                    };
                    let pr = self.pic_rect();
                    let nb = R::new(nb.x.clamp(0.0, (pr.w - nb.w).max(0.0)), nb.y.clamp(0.0, (pr.h - nb.h).max(0.0)), nb.w.min(pr.w), nb.h.min(pr.h));
                    self.boxr = Some(nb);
                }
                self.changed();
                return vec![];
            }
        }
        let Some(action) = self.keymap.lookup(ev).map(|s| s.to_string()) else { return vec![] };
        self.last_shift = ev.shift;
        let fx = self.action(&action);
        self.last_shift = false;
        fx
    }

    /// One action by its id (keys, buttons and tests all come through here).
    pub fn action(&mut self, a: &str) -> Vec<Fx> {
        if self.keys_sheet && a != "keys" && a != "close" && a != "discard" {
            self.keys_sheet = false;
        }
        if let Some(t) = Tool::from_id(a) {
            self.set_tool(t);
            return vec![];
        }
        if let Some(n) = a.strip_prefix("color").and_then(|n| n.parse::<usize>().ok()) {
            if (1..=8).contains(&n) {
                self.set_color(n as i32 - 1, PALETTE[n - 1]);
            }
            return vec![];
        }
        let has_box = self.boxr.map(|b| b.w >= 1.0 && b.h >= 1.0).unwrap_or(false);
        // While picking, the output keys take the box as it is ("take it")
        let take = |s: &mut Editor| {
            if s.phase == Phase::Pick && has_box {
                if let Some(b) = s.boxr {
                    s.drag = None;
                    s.take_box(b);
                }
            }
        };
        match a {
            "thinner" => self.bump_size(-1.0),
            "thicker" => self.bump_size(1.0),
            "delete" => self.delete_sel(),
            "select_all" => {
                self.sel = (0..self.doc.shapes.len()).collect();
                if !self.sel.is_empty() {
                    self.set_tool(Tool::Select);
                }
                self.changed();
            }
            "duplicate" => {
                if !self.sel.is_empty() {
                    self.checkpoint();
                    let mut new = vec![];
                    for &i in &self.sel {
                        let mut s = self.doc.shapes[i].clone();
                        s.translate(12.0, 12.0);
                        self.doc.shapes.push(s);
                        new.push(self.doc.shapes.len() - 1);
                    }
                    self.sel = new;
                    self.changed();
                }
            }
            "undo" => self.undo(),
            "redo" => self.redo(),
            "hide_bars" => {
                self.bars_hidden = !self.bars_hidden;
                if self.bars_hidden {
                    let k = self.keymap.first("hide_bars");
                    return vec![Fx::Toast(format!("Bars hidden - {k} brings them back"))];
                }
            }
            "whole_screen" => {
                if self.phase == Phase::Edit {
                    self.checkpoint();
                }
                self.drag = None;
                self.take_box(self.pic_rect());
            }
            "take_colour" => {
                let c = self.doc.base.at(self.mouse.0 as i32, self.mouse.1 as i32);
                let hex = format!("#{:02x}{:02x}{:02x}", c[0], c[1], c[2]);
                self.pick_colour([c[0], c[1], c[2], 255]);
                return vec![Fx::Clip(hex.clone()), Fx::Toast(format!("Drawing with {hex} (copied)"))];
            }
            "next_shape" | "prev_shape" => {
                let n = self.doc.shapes.len();
                if n > 0 {
                    let cur = self.sel.last().copied();
                    let i = match (cur, a == "next_shape") {
                        (Some(c), true) => (c + 1) % n,
                        (Some(c), false) => (c + n - 1) % n,
                        (None, true) => 0,
                        (None, false) => n - 1,
                    };
                    self.stop_typing();
                    self.tool = Tool::Select;
                    self.sel = vec![i];
                    self.changed();
                }
            }
            "front" | "back" => {
                self.stop_typing();
                if self.sel.len() == 1 {
                    self.checkpoint();
                    let i = self.sel[0];
                    let s = self.doc.shapes.remove(i);
                    if a == "front" {
                        self.doc.shapes.push(s);
                        self.sel = vec![self.doc.shapes.len() - 1];
                    } else {
                        self.doc.shapes.insert(0, s);
                        self.sel = vec![0];
                    }
                    self.changed();
                }
            }
            "mode_back" => {
                let next = Mode::from_index((self.mode.index() + 2) % 3);
                self.set_mode(next);
            }
            "last_box" => return vec![Fx::LastBox],
            "scroll" => {
                if !has_box {
                    return vec![Fx::Toast("Pick the box first (the part of the page that scrolls)".into())];
                }
                take(self);
                return vec![Fx::Scroll];
            }
            "bold" | "italic" => {
                let sel = self.sel.clone();
                let mut any = false;
                for i in sel {
                    if let Some(Kind::Text { bold, italic, .. }) = self.doc.shapes.get_mut(i).map(|s| &mut s.kind) {
                        if !any {
                            any = true;
                        }
                        if a == "bold" {
                            *bold = !*bold
                        } else {
                            *italic = !*italic
                        }
                    }
                }
                if any {
                    self.changed();
                } else {
                    return vec![Fx::Toast("Bold / italic work on a text: pick T and type".into())];
                }
            }
            "zoom_in" => self.zoom_at(self.screen.0 / 2.0, self.screen.1 / 2.0, self.view.s * 1.25),
            "zoom_out" => self.zoom_at(self.screen.0 / 2.0, self.screen.1 / 2.0, self.view.s / 1.25),
            "zoom_fit" => {
                let b = &self.doc.base;
                self.view = if b.w as f32 <= self.screen.0 && b.h as f32 <= self.screen.1 && (b.w as f32 == self.screen.0) {
                    View::default()
                } else {
                    View::fit(b.w as f32, b.h as f32, self.screen.0, self.screen.1, 60.0)
                };
                self.changed();
            }
            "zoom_100" => {
                let c = self.view.to_screen((self.doc.base.w as f32 / 2.0, self.doc.base.h as f32 / 2.0));
                self.zoom_at(c.0, c.1, 1.0);
            }
            "keys" => self.keys_sheet = !self.keys_sheet,
            "mode" => {
                let next = Mode::from_index((self.mode.index() + 1) % 3);
                self.set_mode(next);
            }
            "close" => {
                if self.keys_sheet {
                    self.keys_sheet = false;
                    return vec![];
                }
                if self.tool == Tool::Crop {
                    if let Some(b) = self.crop_backup.take() {
                        // a crop drag still going: its release must not apply the cancelled crop (review D9)
                        self.drag = None;
                        self.boxr = Some(b);
                        self.tool = self.prev_tool;
                        self.changed();
                        return vec![];
                    }
                }
                // anything to keep (a box, even squeezed to nothing, or shapes) = Close with Restore, else nothing
                let keep = self.boxr.is_some() || !self.doc.shapes.is_empty();
                return vec![if keep { Fx::Close } else { Fx::Cancel }];
            }
            "discard" => {
                self.drag = None;
                return vec![Fx::Discard];
            }
            "done" => {
                if self.tool == Tool::Crop && self.phase == Phase::Edit {
                    self.crop_backup = None;
                    self.tool = self.prev_tool;
                    return vec![];
                }
                if !has_box {
                    return vec![];
                }
                take(self);
                return vec![Fx::Done];
            }
            "copy" | "save" | "save_as" | "link" | "pin" | "copy_text" | "redact" | "background" | "copy_save" | "copy_stay" => {
                if !has_box {
                    return vec![];
                }
                take(self);
                return vec![match a {
                    "copy" => Fx::Copy,
                    "save" => Fx::Save,
                    "save_as" => Fx::SaveAs,
                    "link" => Fx::Link,
                    "pin" => Fx::Pin,
                    "redact" => Fx::Redact,
                    "background" => Fx::Background,
                    "copy_save" => Fx::CopySave,
                    "copy_stay" => Fx::CopyStay,
                    _ => Fx::CopyText,
                }];
            }
            "paste" => return vec![Fx::Paste],
            "open" => return vec![Fx::Open],
            "library" => return vec![Fx::Recent],
            "settings" => return vec![Fx::Settings],
            "timer" => return vec![Fx::Timer],
            "pointer" => return vec![Fx::Pointer],
            "new_box" => {
                return vec![Fx::NewBox];
            }
            "fill" => {
                let v = !self.style().fill;
                self.styles.get_mut(&self.tool).unwrap().fill = v;
                self.apply_sel(|s| s.fill = v);
            }
            "shadow" => {
                let v = !self.style().shadow;
                self.styles.get_mut(&self.tool).unwrap().shadow = v;
                self.apply_sel(|s| s.shadow = v);
            }
            _ => {}
        }
        vec![]
    }

    /// Per-tool colour / size / fill / shadow as config text ("style.<tool>" = "r,g,b,idx,size,fill,shadow")
    pub fn styles_text(&self) -> Vec<(String, String)> {
        Tool::ALL
            .iter()
            .filter_map(|t| self.styles.get(t).map(|s| (t, s)))
            .map(|(t, s)| (format!("style.{}", t.id()), format!("{},{},{},{},{},{},{}", s.color[0], s.color[1], s.color[2], s.color_idx, s.size, s.fill as u8, s.shadow as u8)))
            .collect()
    }

    /// The remembered styles back (config text from `styles_text`)
    pub fn load_styles<'a>(&mut self, get: impl Fn(&str) -> &'a str) {
        for t in Tool::ALL {
            let v: Vec<f32> = get(&format!("style.{}", t.id())).split(',').filter_map(|x| x.trim().parse().ok()).collect();
            if v.len() == 7 {
                let (lo, hi, _) = t.size_range();
                if let Some(s) = self.styles.get_mut(&t) {
                    s.color = [v[0] as u8, v[1] as u8, v[2] as u8, 255];
                    s.color_idx = v[3] as i32;
                    s.size = v[4].clamp(lo, hi);
                    s.fill = v[5] != 0.0;
                    s.shadow = v[6] != 0.0;
                }
            }
        }
    }

    /// Ctrl+N: start over on the same frozen picture
    pub fn reset_box(&mut self) {
        self.checkpoint();
        self.boxr = None;
        self.doc.shapes.clear();
        self.sel.clear();
        self.typing = None;
        self.phase = Phase::Pick;
        self.changed();
    }

    pub fn set_mode(&mut self, m: Mode) {
        self.mode = m;
        match m {
            Mode::Screen => {
                if self.phase == Phase::Pick || self.doc.shapes.is_empty() {
                    self.take_box(self.pic_rect());
                } else {
                    self.checkpoint();
                    self.boxr = Some(self.pic_rect());
                    self.changed();
                }
            }
            _ => {
                if self.doc.shapes.is_empty() && self.phase == Phase::Edit {
                    self.boxr = None;
                    self.phase = Phase::Pick;
                    self.changed();
                }
                if m == Mode::Window {
                    self.hover_win = self.window_at(self.mouse);
                }
            }
        }
    }

    pub fn set_tool(&mut self, t: Tool) {
        self.stop_typing();
        if t == Tool::Blur && self.tool == Tool::Blur {
            // B again: switch the look (pixelate <-> blur), also on a selected blur
            self.pixelate = !self.pixelate;
            let p = self.pixelate;
            self.apply_sel(|s| {
                if let Kind::Blur { pixelate, .. } = &mut s.kind {
                    *pixelate = p
                }
            });
            return;
        }
        if t == Tool::Crop && self.tool != Tool::Crop {
            self.prev_tool = self.tool;
            self.crop_backup = self.boxr;
        } else if t != Tool::Crop {
            self.crop_backup = None;
        }
        if t != Tool::Select {
            self.sel.clear();
        }
        self.tool = t;
        self.changed();
    }

    pub fn set_color(&mut self, idx: i32, c: [u8; 4]) {
        let t = self.tool;
        if let Some(st) = self.styles.get_mut(&t) {
            st.color = c;
            st.color_idx = idx;
        }
        // drawing tools share the colour (pick red once, every tool draws red), except the highlighter
        if t != Tool::Highlight {
            for (k, st) in self.styles.iter_mut() {
                if *k != Tool::Highlight && k.has_color() {
                    st.color = c;
                    st.color_idx = idx;
                }
            }
        }
        self.apply_sel(|s| s.color = c);
    }

    /// A colour from the picture becomes the drawing colour (no palette dot is lit for it)
    fn pick_colour(&mut self, c: [u8; 4]) {
        if self.tool == Tool::Picker {
            // the colour goes to the drawing tools, not to the picker itself
            for (k, st) in self.styles.iter_mut() {
                if *k != Tool::Highlight && k.has_color() {
                    st.color = c;
                    st.color_idx = -1;
                }
            }
        } else {
            self.set_color(-1, c);
        }
    }

    fn apply_sel(&mut self, f: impl Fn(&mut Shape)) {
        self.apply_sel_tagged(None, f)
    }

    /// Changes the selected shapes; nothing changed = no undo step; the same `tag` again = the same undo step
    fn apply_sel_tagged(&mut self, tag: Option<String>, f: impl Fn(&mut Shape)) {
        if self.sel.is_empty() {
            self.changed();
            return;
        }
        let before: Vec<Shape> = self.sel.iter().filter_map(|&i| self.doc.shapes.get(i).cloned()).collect();
        let mut after = before.clone();
        after.iter_mut().for_each(&f);
        if after == before {
            self.changed();
            return;
        }
        match tag {
            Some(t) => self.checkpoint_tag(t),
            None => self.checkpoint(),
        }
        for &i in &self.sel {
            if let Some(s) = self.doc.shapes.get_mut(i) {
                f(s);
            }
        }
        self.changed();
    }

    pub fn set_size(&mut self, v: f32) {
        let t = if self.sel.len() == 1 { self.shape_tool(self.sel[0]) } else { self.tool };
        let (lo, hi, _) = t.size_range();
        let v = v.clamp(lo, hi).round();
        if let Some(st) = self.styles.get_mut(&t) {
            st.size = v;
        }
        if self.tool != t {
            if let Some(st) = self.styles.get_mut(&self.tool) {
                st.size = v.clamp(self.tool.size_range().0, self.tool.size_range().1);
            }
        }
        let tag = format!("size{:?}", self.sel);
        self.apply_sel_tagged(Some(tag), |s| s.size = v);
    }

    fn bump_size(&mut self, dir: f32) {
        let t = if self.sel.len() == 1 { self.shape_tool(self.sel[0]) } else { self.tool };
        let cur = if self.sel.len() == 1 { self.doc.shapes[self.sel[0]].size } else { self.style().size };
        let (lo, hi, _) = t.size_range();
        let step = if hi > 50.0 { 2.0 } else { 1.0 } * if self.last_shift { 5.0 } else { 1.0 };
        self.set_size((cur + dir * step).clamp(lo.min(cur), hi.max(cur)));
    }

    fn shape_tool(&self, i: usize) -> Tool {
        match self.doc.shapes.get(i).map(|s| &s.kind) {
            Some(Kind::Pen { .. }) => Tool::Pen,
            Some(Kind::Highlight { .. }) => Tool::Highlight,
            Some(Kind::Line { .. }) => Tool::Line,
            Some(Kind::Arrow { .. }) => Tool::Arrow,
            Some(Kind::Rect { .. }) => Tool::Rect,
            Some(Kind::Ellipse { .. }) => Tool::Ellipse,
            Some(Kind::Text { .. }) => Tool::Text,
            Some(Kind::Step { .. }) => Tool::Step,
            Some(Kind::Blur { .. }) => Tool::Blur,
            _ => self.tool,
        }
    }

    fn delete_sel(&mut self) {
        if self.sel.is_empty() {
            return;
        }
        self.checkpoint();
        let mut s = self.sel.clone();
        s.sort_unstable();
        for i in s.into_iter().rev() {
            if i < self.doc.shapes.len() {
                self.doc.shapes.remove(i);
            }
        }
        self.sel.clear();
        self.changed();
    }

    /// A pasted picture becomes a shape in the middle of the box (shrunk to fit).
    pub fn paste_image(&mut self, pic: Pic) {
        let Some(b) = self.boxr else { return };
        self.checkpoint();
        let id = format!("img{}", self.next_img);
        self.next_img += 1;
        let s = ((b.w * 0.8) / pic.w as f32).min((b.h * 0.8) / pic.h as f32).min(1.0);
        let (w, h) = (pic.w as f32 * s, pic.h as f32 * s);
        let r = R::new(b.x + (b.w - w) / 2.0, b.y + (b.h - h) / 2.0, w, h);
        self.doc.images.insert(id.clone(), Arc::new(pic));
        self.doc.shapes.push(Shape { kind: Kind::Image { r, id }, color: [0; 4], size: 1.0, fill: false, shadow: false });
        self.sel = vec![self.doc.shapes.len() - 1];
        self.set_tool(Tool::Select);
        self.sel = vec![self.doc.shapes.len() - 1];
        self.changed();
    }

    /// Blur boxes found by the privacy check (picture px)
    pub fn add_blurs(&mut self, rs: &[R]) {
        if rs.is_empty() {
            return;
        }
        self.checkpoint();
        for r in rs {
            self.doc.shapes.push(Shape { kind: Kind::Blur { r: r.inflate(3.0), pixelate: true }, color: [0; 4], size: 10.0, fill: false, shadow: false });
        }
        self.changed();
    }

    /// Where the text caret is (picture px): x, y, height
    pub fn caret(&self) -> Option<(f32, f32, f32)> {
        let t = self.typing?;
        let s = self.doc.shapes.get(t)?;
        if let Kind::Text { at, text, bold, italic } = &s.kind {
            let last = text.rsplit('\n').next().unwrap_or("");
            let lines = text.split('\n').count().max(1);
            let (w, _, lh, _) = crate::shapes::text_metrics(last, s.size, *bold, *italic);
            let w = if last.is_empty() { 0.0 } else { w };
            return Some((at.0 + w + 1.0, at.1 + lh * (lines - 1) as f32, lh));
        }
        None
    }

    /// The finished picture of the box.
    pub fn flatten(&self) -> Option<Pic> {
        let b = self.box_ir()?;
        Some(crate::shapes::flatten(&self.doc, b))
    }

    /// Selection outline (picture px) + handle points to show.
    /// The selection outline + handles: only with the Select tool (the user, 20 Nov 2026: a shape just drawn with a
    /// drawing tool got a cyan box around it). The just-drawn shape stays selected (colour / size changes go to it).
    pub fn sel_frame(&self) -> Option<(R, Vec<(f32, f32)>)> {
        if self.sel.is_empty() || self.tool != Tool::Select {
            return None;
        }
        let mut bb: Option<R> = None;
        for &i in &self.sel {
            if let Some(s) = self.doc.shapes.get(i) {
                let b = s.bbox();
                bb = Some(bb.map(|x| x.union(&b)).unwrap_or(b));
            }
        }
        let bb = bb?;
        let mut hs = vec![];
        if self.sel.len() == 1 && self.tool == Tool::Select {
            let s = &self.doc.shapes[self.sel[0]];
            if let Some(f) = s.frame() {
                hs = Self::handles(&f).to_vec();
            } else if let Some((a, b)) = s.ends() {
                hs = vec![a, b];
            }
        }
        Some((bb, hs))
    }
}

fn snap45(a: (f32, f32), p: (f32, f32)) -> (f32, f32) {
    let (dx, dy) = (p.0 - a.0, p.1 - a.1);
    let ang = dy.atan2(dx);
    let step = std::f32::consts::FRAC_PI_4;
    let s = (ang / step).round() * step;
    let len = (dx * dx + dy * dy).sqrt();
    (a.0 + len * s.cos(), a.1 + len * s.sin())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::keys::KeyEv;

    fn ed() -> Editor {
        let mut p = Pic::new(400, 300);
        p.px.iter_mut().enumerate().for_each(|(i, v)| *v = if i % 4 == 3 { 255 } else { (i % 200) as u8 });
        Editor::new(Arc::new(p), (400.0, 300.0), Keymap::default())
    }
    fn k(key: &str) -> KeyEv {
        KeyEv { key: key.into(), text: key.into(), ..Default::default() }
    }
    fn ck(key: &str) -> KeyEv {
        KeyEv { key: key.into(), ctrl: true, ..Default::default() }
    }
    fn pick(e: &mut Editor) {
        e.pointer_down(50.0, 40.0, 1);
        e.pointer_move(150.0, 120.0);
        e.pointer_move(250.0, 200.0);
        e.pointer_up(250.0, 200.0);
    }

    #[test]
    fn drag_picks_a_box() {
        let mut e = ed();
        pick(&mut e);
        assert_eq!(e.phase, Phase::Edit);
        assert_eq!(e.box_ir(), Some(IR { x: 50, y: 40, w: 200, h: 160 }));
    }

    #[test]
    fn click_takes_window_or_screen() {
        // Window mode: a click = the window under the mouse, else the whole monitor
        let mut e = ed();
        e.mode = Mode::Window;
        e.windows = vec![R::new(10.0, 10.0, 100.0, 80.0)];
        e.pointer_down(20.0, 20.0, 1);
        e.pointer_up(20.0, 20.0);
        assert_eq!(e.box_ir(), Some(IR { x: 10, y: 10, w: 100, h: 80 }));
        let mut e = ed();
        e.mode = Mode::Window;
        e.pointer_down(300.0, 250.0, 1);
        e.pointer_up(300.0, 250.0);
        assert_eq!(e.box_ir(), Some(IR { x: 0, y: 0, w: 400, h: 300 }));
    }

    #[test]
    fn drag_outside_the_box_picks_a_new_one() {
        // the user, 21 Nov 2026: a new area while one is already picked
        let mut e = ed();
        pick(&mut e);
        let old = e.box_ir();
        // a click outside keeps the box
        e.pointer_down(380.0, 280.0, 1);
        e.pointer_up(380.0, 280.0);
        assert_eq!(e.box_ir(), old);
        // a drag outside = the new box, still editing
        e.pointer_down(300.0, 220.0, 1);
        e.pointer_move(340.0, 250.0);
        e.pointer_move(380.0, 290.0);
        e.pointer_up(380.0, 290.0);
        assert_eq!((e.phase, e.box_ir()), (Phase::Edit, Some(IR { x: 300, y: 220, w: 80, h: 70 })));
        // undo brings the old box back
        e.undo();
        assert_eq!(e.box_ir(), old);
    }

    #[test]
    fn area_click_does_nothing() {
        // the user, 20 Nov 2026: in Area mode a plain click never grabs a window - still picking, no box
        let mut e = ed();
        e.windows = vec![R::new(10.0, 10.0, 100.0, 80.0)];
        e.pointer_down(20.0, 20.0, 1);
        e.pointer_up(20.0, 20.0);
        assert_eq!((e.phase, e.box_ir()), (Phase::Pick, None));
        // a drag right after still picks a box
        pick(&mut e);
        assert_eq!(e.box_ir(), Some(IR { x: 50, y: 40, w: 200, h: 160 }));
    }

    #[test]
    fn every_tool_draws_and_undo_redo() {
        let mut e = ed();
        pick(&mut e);
        for (key, n) in [("f", 1), ("h", 2), ("l", 3), ("a", 4), ("r", 5), ("e", 6), ("b", 7), ("s", 8)] {
            e.key(&k(key));
            e.pointer_down(80.0, 60.0, 1);
            e.pointer_move(120.0, 90.0);
            e.pointer_move(160.0, 110.0);
            e.pointer_up(160.0, 110.0);
            assert_eq!(e.doc.shapes.len(), n, "tool {key}");
        }
        e.key(&k("n"));
        e.pointer_down(100.0, 100.0, 1);
        e.pointer_up(100.0, 100.0);
        e.pointer_down(130.0, 100.0, 1);
        e.pointer_up(130.0, 100.0);
        assert!(matches!(e.doc.shapes[9].kind, Kind::Step { n: 2, .. }));
        // text with Ctrl+B
        e.key(&k("t"));
        e.pointer_down(90.0, 150.0, 1);
        e.pointer_up(90.0, 150.0);
        for c in ["H", "i"] {
            e.key(&k(c));
        }
        e.key(&ck("b"));
        e.key(&KeyEv::named("Escape"));
        match &e.doc.shapes[10].kind {
            Kind::Text { text, bold, .. } => {
                assert_eq!(text, "Hi");
                assert!(*bold);
            }
            _ => panic!(),
        }
        assert!(e.flatten().is_some());
        let n = e.doc.shapes.len();
        e.key(&ck("z"));
        assert_eq!(e.doc.shapes.len(), n - 1);
        e.key(&KeyEv { key: "z".into(), ctrl: true, shift: true, ..Default::default() });
        assert_eq!(e.doc.shapes.len(), n);
        e.key(&ck("z"));
        e.key(&ck("y"));
        assert_eq!(e.doc.shapes.len(), n);
    }

    #[test]
    fn next_shape_next_to_the_last_one_is_new() {
        let mut e = ed();
        pick(&mut e);
        e.key(&k("b"));
        e.pointer_down(60.0, 50.0, 1);
        e.pointer_move(150.0, 70.0);
        e.pointer_up(150.0, 70.0);
        // starts 5 px below the first blur's corner handle
        e.pointer_down(60.0, 75.0, 1);
        e.pointer_move(150.0, 95.0);
        e.pointer_up(150.0, 95.0);
        assert_eq!(e.doc.shapes.len(), 2);
    }

    #[test]
    fn select_move_delete_dup() {
        let mut e = ed();
        pick(&mut e);
        e.key(&k("r"));
        e.pointer_down(80.0, 60.0, 1);
        e.pointer_move(140.0, 100.0);
        e.pointer_up(140.0, 100.0);
        e.key(&k("v"));
        e.pointer_down(80.0, 80.0, 1);
        e.pointer_move(90.0, 90.0);
        e.pointer_up(90.0, 90.0);
        assert_eq!(e.doc.shapes[0].frame().unwrap().x, 90.0);
        e.key(&KeyEv { key: "Right".into(), shift: true, ..Default::default() });
        assert_eq!(e.doc.shapes[0].frame().unwrap().x, 100.0);
        e.key(&ck("d"));
        assert_eq!(e.doc.shapes.len(), 2);
        e.key(&ck("a"));
        e.key(&KeyEv::named("Delete"));
        assert!(e.doc.shapes.is_empty());
    }

    #[test]
    fn esc_closes_at_once_and_crop_cancels() {
        let mut e = ed();
        assert_eq!(e.key(&KeyEv::named("Escape")), vec![Fx::Cancel]);
        pick(&mut e);
        e.key(&k("c"));
        e.pointer_down(60.0, 50.0, 1);
        e.pointer_move(100.0, 100.0);
        e.pointer_up(100.0, 100.0);
        assert_eq!(e.box_ir().unwrap().w, 40);
        e.key(&KeyEv::named("Escape"));
        assert_eq!(e.box_ir().unwrap().w, 200);
        assert_eq!(e.key(&KeyEv::named("Escape")), vec![Fx::Close]);
    }

    #[test]
    fn outputs_and_modes() {
        let mut e = ed();
        e.key(&KeyEv::named("Tab"));
        assert_eq!(e.mode, Mode::Window);
        e.key(&KeyEv::named("Tab"));
        assert_eq!(e.mode, Mode::Screen);
        assert_eq!(e.box_ir().unwrap().w, 400);
        let mut e = ed();
        pick(&mut e);
        assert_eq!(e.key(&ck("c")), vec![Fx::Copy]);
        assert_eq!(e.key(&ck("s")), vec![Fx::Save]);
        assert_eq!(e.key(&ck("l")), vec![Fx::Link]);
        assert_eq!(e.key(&ck("t")), vec![Fx::Pin]);
        assert_eq!(e.key(&KeyEv::named("Return")), vec![Fx::Done]);
        assert_eq!(e.key(&ck("n")), vec![Fx::NewBox]);
    }

    #[test]
    fn shift_and_alt_while_drawing() {
        let mut e = ed();
        pick(&mut e);
        // Shift: the line snaps to 45 degrees
        e.key(&k("l"));
        e.set_mods(false, true, false);
        e.pointer_down(100.0, 100.0, 1);
        e.pointer_move(150.0, 140.0);
        e.pointer_up(150.0, 140.0);
        let (a, b) = e.doc.shapes[0].ends().unwrap();
        assert!(((b.0 - a.0) - (b.1 - a.1)).abs() < 0.01, "{a:?} {b:?}");
        // Shift: a square
        e.key(&k("r"));
        e.pointer_down(100.0, 60.0, 1);
        e.pointer_move(160.0, 80.0);
        e.pointer_up(160.0, 80.0);
        let r = e.doc.shapes[1].frame().unwrap();
        assert_eq!(r.w, r.h);
        // Alt: from the centre
        e.set_mods(false, false, true);
        e.key(&k("e"));
        e.pointer_down(200.0, 150.0, 1);
        e.pointer_move(220.0, 170.0);
        e.pointer_up(220.0, 170.0);
        let r = e.doc.shapes[2].frame().unwrap();
        assert_eq!((r.x, r.y, r.w, r.h), (180.0, 130.0, 40.0, 40.0));
    }

    #[test]
    fn review_fixes_undo_and_typing() {
        // A5: rect + "Hi"; re-edit "Hi", empty it, Esc -> undo brings "Hi" back, the rect stays
        let mut e = ed();
        pick(&mut e);
        e.key(&k("r"));
        e.pointer_down(60.0, 50.0, 1);
        e.pointer_move(120.0, 90.0);
        e.pointer_up(120.0, 90.0);
        e.key(&k("t"));
        e.pointer_down(200.0, 150.0, 1);
        e.pointer_up(200.0, 150.0);
        e.key(&k("H"));
        e.key(&k("i"));
        e.key(&KeyEv::named("Escape"));
        assert_eq!(e.doc.shapes.len(), 2);
        let at = match &e.doc.shapes[1].kind {
            Kind::Text { at, .. } => *at,
            _ => panic!(),
        };
        e.double_click(at.0 + 3.0, at.1 + 10.0);
        assert!(e.typing.is_some());
        e.key(&KeyEv { key: "BackSpace".into(), ..Default::default() });
        e.key(&KeyEv { key: "BackSpace".into(), ..Default::default() });
        e.key(&KeyEv::named("Escape"));
        assert_eq!(e.doc.shapes.len(), 1);
        e.key(&ck("z"));
        assert_eq!(e.doc.shapes.len(), 2, "undo brings the text back");
        assert!(matches!(&e.doc.shapes[0].kind, Kind::Rect { .. }));
        // A9: Ctrl+Shift+B while typing is "last box", not bold
        e.key(&k("t"));
        e.pointer_down(180.0, 185.0, 1);
        e.pointer_up(180.0, 185.0);
        e.key(&k("x"));
        let fx = e.key(&KeyEv { key: "b".into(), ctrl: true, shift: true, ..Default::default() });
        assert_eq!(fx, vec![Fx::LastBox]);
        let bold = |e: &Editor| e.doc.shapes.iter().any(|s| matches!(&s.kind, Kind::Text { bold: true, .. }));
        assert!(!bold(&e));
        e.key(&ck("b"));
        assert!(bold(&e));
        // A7: Ctrl+] while typing ends the typing first (no typing into a moved index)
        e.key(&KeyEv { key: "[".into(), ctrl: true, ..Default::default() });
        assert!(e.typing.is_none());
        // A6: undo after Ctrl+N is back in Edit with the box
        assert_eq!(e.key(&ck("n")), vec![Fx::NewBox]);
        e.reset_box(); // what the app does for Fx::NewBox
        assert_eq!(e.phase, Phase::Pick);
        e.undo();
        assert_eq!(e.phase, Phase::Edit);
        assert!(e.boxr.is_some());
    }

    #[test]
    fn review_fixes_size_scale_close() {
        // A8: a selected text grows past 40 with the select tool; a run of size changes = one undo step
        let mut e = ed();
        pick(&mut e);
        e.key(&k("t"));
        e.pointer_down(100.0, 100.0, 1);
        e.pointer_up(100.0, 100.0);
        e.key(&k("A"));
        e.key(&KeyEv::named("Escape"));
        e.key(&k("v"));
        e.sel = vec![0];
        let undo_before = e.undo.len();
        for _ in 0..30 {
            e.key(&k("]"));
        }
        assert!(e.doc.shapes[0].size > 40.0, "{}", e.doc.shapes[0].size);
        assert_eq!(e.undo.len(), undo_before + 1);
        // A10: at scale 2 a 4 px jitter is a click (window / screen), not a 4x4 box
        let mut e = ed();
        e.mode = Mode::Window;
        e.ui_scale = 2.0;
        e.pointer_down(100.0, 100.0, 1);
        e.pointer_move(104.0, 103.0);
        e.pointer_up(104.0, 103.0);
        assert_eq!(e.box_ir().unwrap().w, 400);
        // A1: a box squeezed to nothing by the crop tool still closes with Restore (not thrown away)
        let mut e = ed();
        pick(&mut e);
        e.key(&k("a"));
        e.pointer_down(80.0, 60.0, 1);
        e.pointer_move(140.0, 100.0);
        e.pointer_up(140.0, 100.0);
        e.boxr = Some(R::new(50.0, 40.0, 0.0, 0.0));
        assert_eq!(e.key(&KeyEv::named("Escape")), vec![Fx::Close]);
    }

    #[test]
    fn power_keys() {
        let mut e = ed();
        pick(&mut e);
        let alt = |k: &str| KeyEv { key: k.into(), alt: true, ..Default::default() };
        e.key(&alt("4"));
        assert_eq!(e.tool, Tool::Arrow);
        e.key(&alt("9"));
        assert_eq!(e.tool, Tool::Blur);
        e.key(&alt("1"));
        assert_eq!(e.tool, Tool::Pen);
        // two shapes, cycle, reorder
        for (x, y) in [(60.0, 50.0), (100.0, 90.0)] {
            e.pointer_down(x, y, 1);
            e.pointer_move(x + 40.0, y + 20.0);
            e.pointer_up(x + 40.0, y + 20.0);
        }
        e.key(&KeyEv { key: "Tab".into(), ctrl: true, ..Default::default() });
        assert_eq!((e.tool, e.sel.clone()), (Tool::Select, vec![0]));
        e.key(&KeyEv { key: "]".into(), ctrl: true, ..Default::default() });
        assert_eq!(e.sel, vec![1]);
        assert!(matches!(&e.doc.shapes[1].kind, Kind::Pen { pts } if pts[0] == (60.0, 50.0)));
        // Shift+] = big step
        e.key(&k("a"));
        let s0 = e.style().size;
        e.key(&KeyEv { key: "}".into(), phys: Some(']'), shift: true, ..Default::default() });
        assert_eq!(e.style().size, s0 + 5.0);
        // Ctrl+H hides the bars, again shows them
        assert!(matches!(e.key(&ck("h"))[0], Fx::Toast(_)));
        assert!(e.bars_hidden);
        e.key(&ck("h"));
        assert!(!e.bars_hidden);
        // Ctrl+Alt+Left: the box grows to the left
        let b = e.box_ir().unwrap();
        e.key(&KeyEv { key: "Left".into(), ctrl: true, alt: true, ..Default::default() });
        let n = e.box_ir().unwrap();
        assert_eq!((n.x, n.w), (b.x - 1, b.w + 1));
        // Ctrl+Shift+A: the whole screen; Shift+Tab: modes backwards
        e.key(&KeyEv { key: "a".into(), ctrl: true, shift: true, ..Default::default() });
        assert_eq!(e.box_ir().unwrap().w, 400);
        e.key(&KeyEv { key: "Tab".into(), shift: true, ..Default::default() });
        assert_eq!(e.mode, Mode::Screen);
        // copy + save, copy and stay, last box
        assert_eq!(e.key(&KeyEv { key: "Return".into(), ctrl: true, ..Default::default() }), vec![Fx::CopySave]);
        assert_eq!(e.key(&KeyEv { key: "c".into(), ctrl: true, alt: true, ..Default::default() }), vec![Fx::CopyStay]);
        assert_eq!(e.key(&KeyEv { key: "b".into(), ctrl: true, shift: true, ..Default::default() }), vec![Fx::LastBox]);
        // take the colour under the mouse
        e.pointer_move(10.0, 10.0);
        let fx = e.key(&KeyEv { key: "i".into(), ctrl: true, shift: true, ..Default::default() });
        assert!(matches!(&fx[0], Fx::Clip(h) if h.starts_with('#')));
        assert_eq!(e.style().color_idx, -1);
    }

    #[test]
    fn colours_sizes_blur_look() {
        let mut e = ed();
        pick(&mut e);
        e.key(&k("a"));
        e.key(&k("6"));
        assert_eq!(e.style().color, PALETTE[5]);
        let s0 = e.style().size;
        e.key(&k("]"));
        assert!(e.style().size > s0);
        e.key(&k("b"));
        assert!(e.pixelate);
        e.key(&k("b"));
        assert!(!e.pixelate);
    }

    #[test]
    fn esc_during_a_crop_drag_cancels_it() {
        // D9: Esc while dragging a crop, then the release: the old box stays
        let mut e = ed();
        pick(&mut e);
        let before = e.box_ir();
        e.key(&k("c"));
        e.pointer_down(80.0, 60.0, 1);
        e.pointer_move(120.0, 90.0);
        e.pointer_move(150.0, 110.0);
        e.action("close");
        e.pointer_up(150.0, 110.0);
        assert_eq!(e.box_ir(), before);
    }

    #[test]
    fn bold_while_re_editing_a_text_is_one_undo_step() {
        // D6: re-edit a text, only Ctrl+B: that is a change (one undo step), undo takes the bold back
        let mut e = ed();
        pick(&mut e);
        e.key(&k("t"));
        e.pointer_down(90.0, 150.0, 1);
        e.pointer_up(90.0, 150.0);
        for c in ["H", "i"] {
            e.key(&k(c));
        }
        e.key(&KeyEv::named("Escape"));
        let steps = e.undo.len();
        let at = match &e.doc.shapes[0].kind {
            Kind::Text { at, .. } => *at,
            _ => panic!("text"),
        };
        e.key(&k("v"));
        e.double_click(at.0 + 4.0, at.1 + 6.0);
        assert!(e.typing.is_some(), "re-editing");
        e.key(&ck("b"));
        e.key(&KeyEv::named("Escape"));
        assert_eq!(e.undo.len(), steps + 1);
        assert!(matches!(e.doc.shapes[0].kind, Kind::Text { bold: true, .. }));
        e.undo();
        assert!(matches!(e.doc.shapes[0].kind, Kind::Text { bold: false, .. }));
    }

    #[test]
    fn a_click_on_the_box_keeps_undo_and_redo() {
        // D8: Select tool, click inside the box without moving: no empty undo step, redo still there
        let mut e = ed();
        pick(&mut e);
        e.key(&k("r"));
        e.pointer_down(80.0, 60.0, 1);
        e.pointer_move(120.0, 90.0);
        e.pointer_up(160.0, 110.0);
        e.undo();
        assert!(e.can_redo());
        let steps = e.undo.len();
        e.key(&k("v"));
        e.pointer_down(200.0, 180.0, 1);
        e.pointer_up(200.0, 180.0);
        assert_eq!(e.undo.len(), steps);
        assert!(e.can_redo(), "redo survives a click");
    }
}
