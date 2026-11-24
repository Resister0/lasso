//! The always-running part: capture -> frozen screen -> editor -> copy / save / pin / restore, plus the corner
//! popups. Runs on Slint's event loop (Lasso's layer-shell platform on KDE, winit elsewhere).

use crate::config::Config;
use crate::editor::{Editor, Fx, Mode, Phase, View, PALETTE};
use crate::keys::{self, KeyEv, Keymap};
use crate::library;
use crate::shapes::{Kind, Pic, Tool, R};
use crate::{Hello, Overlay, Pin, RestorePopup, ScrollPopup, Thumb};
use slint::{ComponentHandle, Image, Model, ModelRc, Rgba8Pixel, SharedPixelBuffer, SharedString, VecModel};
use std::cell::RefCell;
use std::rc::{Rc, Weak};
use std::sync::Arc;
use std::time::Instant;

#[derive(Clone, Debug, Default)]
pub struct OutInfo {
    pub key: u32,
    pub name: String,
    /// logical rect
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
    /// physical size
    pub pw: i32,
    pub ph: i32,
}

impl OutInfo {
    pub fn scale(&self) -> f32 {
        if self.w > 0 {
            self.pw as f32 / self.w as f32
        } else {
            1.0
        }
    }
}

struct Out {
    info: OutInfo,
    comp: Overlay,
    win: u32,
    layer_ver: u64,
    shown: bool,
    /// the picture the darkened copy (bg-dim) was made from (Arc pointer; 0 = none)
    dim_src: usize,
    /// the shape shown as the small live picture while dragged (the layer then holds the other shapes)
    live_idx: Option<usize>,
    /// the selection handles, changed in place (a new model on every move made Slint repaint every bar after them)
    handles: Rc<VecModel<crate::Pt>>,
}

struct PinWin {
    comp: Pin,
    win: u32,
    pic: Arc<Pic>,
    zoom: f32,
    see: f32,
    pos: (i32, i32),
    out: Option<u32>,
    grab: (f32, f32),
    /// clicks go through it (a map over a game); the tray / `lasso --cmd pins-clickable` turns it off
    through: bool,
}

/// The corner thumbnail after a capture (quick access overlay): drag out, copy, save, pin, edit, delete
struct ThumbWin {
    t: Thumb,
    win: u32,
    id: String,
    pic: Arc<Pic>,
    /// pictures this capture saved (known here before the library write is on disk)
    saved: Vec<String>,
    hovered: bool,
    dragging: bool,
}

/// One monitor's part of the editor's picture, in physical px (= its place on the joined screen). A capture over
/// several monitors is ONE picture with ONE box (the user, 21 Nov 2026: "drag a screenshot between two monitors ...
/// like Lightshot"); every monitor's frozen screen shows its part of it. A library picture: one part at 0, 0.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Part {
    key: u32,
    x: f32,
    y: f32,
    w: f32,
    h: f32,
}

/// What one capture is: library id + where it came from (for "replace the saved file")
#[derive(Clone, Default)]
struct Sess {
    id: String,
    created: i64,
    source: String,
    saved: Vec<String>,
    background: bool,
    /// which monitor's picture screen.png holds now (None = not stored yet): a box on another monitor rewrites it
    stored_out: Option<u32>,
    /// made by this capture (Esc with nothing picked may delete it); false for library edits / restores
    fresh: bool,
    /// a fresh capture goes into the library (Settings > "Keep in the library": every capture / the used ones / none)
    kept: bool,
    /// the app / game the box is on ("Firefox", "Elden Ring"...), for {app} in the file name
    app: String,
    /// a library picture as it was opened (state.json, thumb.png): "Save as a copy" puts it back in place
    opened: Option<(Vec<u8>, Option<Vec<u8>>)>,
}

pub struct App {
    pub cfg: Config,
    outs: Vec<Out>,
    /// the one editor of the frozen screen(s)
    ed: Option<Editor>,
    /// where each monitor showing it sits in its picture
    parts: Vec<Part>,
    /// the last pointer move (joined-screen px): the same again is skipped
    last_move: (f32, f32),
    /// made once for every monitor: the drawn layer ((picture, version), image), the shapes under a dragged one
    /// (its index), the darkened picture (picture)
    layer_img: Option<((usize, u64), Image)>,
    rest_img: Option<(usize, Image)>,
    dim_img: Option<(usize, Image)>,
    /// the monitor the pointer is on (bars + popups without a box, Full screenshot of "this monitor")
    active: usize,
    /// Esc (Shift+Esc) pressed on the frozen screen, acted on when it comes up
    esc_down: Option<KeyEv>,
    sess: Option<Sess>,
    showing: bool,
    pins: Vec<PinWin>,
    restore_popup: Option<(RestorePopup, u32)>,
    hello: Option<Hello>,
    scrolling: Option<Scrolling>,
    thumb: Option<ThumbWin>,
    /// the last finished capture (id, saved files, when) for the "Delete the last capture" key
    last_out: Option<(String, Vec<String>, std::time::Instant)>,
    thumb_timer: slint::Timer,
    toast_timer: slint::Timer,
    delay_timer: slint::Timer,
    me: Weak<RefCell<App>>,
    tools_model: ModelRc<crate::ToolItem>,
    /// the bar under the box (built on start / settings change only - a model set every frame repaints everything)
    bar_model: ModelRc<crate::BarItem>,
    /// the same for a library picture: "Save as a copy" before Done (22 Nov 2026)
    bar_model_lib: ModelRc<crate::BarItem>,
    /// the next library picture / file opens on this monitor (its name), not where the mouse last was over Lasso
    want_out: Option<String>,
    pub on_tray_changed: Option<Box<dyn Fn(Option<i64>)>>,
    keymap: Keymap,
    /// the Settings / Library window processes, with their argument ("--settings-window")
    children: Vec<(String, std::process::Child)>,
    last_box: Option<(String, R)>,
}

pub type AppRc = Rc<RefCell<App>>;

const TOOL_ICONS: [&str; 13] = [
    "M4 4l7 17l2.5-7.5L21 11z",
    "M6 2v4M2 6h4M6 6l3 3M14 4h6v6M20 14v6h-6M10 20H4v-6",
    "M3 21c3-1 4-3 6-6s5-9 8-10s3 2 1 4s-7 5-9 8s-1 4 2 3",
    "M9 11l-6 6v3h9l3-3M22 12l-4.6 4.6a2 2 0 0 1-2.8 0l-5.2-5.2a2 2 0 0 1 0-2.8L14 4",
    "M5 19L19 5",
    "M5 19L19 5M10 5h9v9",
    "M5 5h14a2 2 0 0 1 2 2v10a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2V7a2 2 0 0 1 2-2z",
    "M3 12a9 7 0 1 0 18 0a9 7 0 1 0-18 0z",
    "M5 6V4h14v2M12 4v16M9 20h6",
    "M3 12a9 9 0 1 0 18 0a9 9 0 1 0-18 0zM10 9l2.5-1.5V16",
    "M3 3h6v6H3zM15 3h6v6h-6zM9 9h6v6H9zM3 15h6v6H3zM15 15h6v6h-6z",
    "M8 12a4 4 0 1 0 8 0a4 4 0 1 0-8 0zM12 2v3M12 19v3M2 12h3M19 12h3",
    "M2 22l1-1h3l9-9M3 21v-3l9-9M15 6l3.4-3.4a2.1 2.1 0 1 1 3 3L18 9l.4.4a2.1 2.1 0 1 1-3 3l-3.8-3.8a2.1 2.1 0 1 1 3-3z",
];
const OCR_ICON: &str = "M4 7V4h16v3M9 20h6M12 4v16M3 14h4M17 14h4";

fn sh(s: impl Into<SharedString>) -> SharedString {
    s.into()
}

/// A library picture's backdrop: the box's picture small and blurred (shown darkened behind it, like Echo's player)
fn view_backdrop(base: &Pic, boxr: Option<R>) -> Image {
    let b = boxr.map(|r| r.ir().clamp(base.w as i32, base.h as i32)).filter(|r| !r.is_empty()).unwrap_or(crate::shapes::IR { x: 0, y: 0, w: base.w as i32, h: base.h as i32 });
    let small = crate::export::thumbnail(&base.crop(b), 160);
    let img = image::RgbaImage::from_raw(small.w, small.h, small.px).unwrap_or_default();
    let blurred = image::imageops::blur(&img, 8.0);
    let (w, h) = blurred.dimensions();
    Image::from_rgba8_premultiplied(SharedPixelBuffer::<Rgba8Pixel>::clone_from_slice(blurred.as_raw(), w, h))
}

/// "21 Nov 2026, 03:02 · Firefox" for the viewer bar
fn view_title(s: &Sess) -> String {
    const M: [&str; 12] = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];
    let (y, mo, d, h, mi, _) = crate::util::local_from_unix(s.created);
    let when = format!("{d} {} {y}, {h:02}:{mi:02}", M[(mo.clamp(1, 12) - 1) as usize]);
    if s.app.is_empty() { when } else { format!("{when} · {}", s.app) }
}

fn rgba_image(pic: &Pic) -> Image {
    let buf = SharedPixelBuffer::<Rgba8Pixel>::clone_from_slice(&pic.px, pic.w, pic.h);
    Image::from_rgba8_premultiplied(buf)
}

impl App {
    pub fn new(cfg: Config) -> AppRc {
        let keymap = cfg.keymap();
        let app = Rc::new(RefCell::new(App {
            cfg,
            outs: vec![],
            ed: None,
            parts: vec![],
            last_move: (-1.0, -1.0),
            layer_img: None,
            rest_img: None,
            dim_img: None,
            active: 0,
            esc_down: None,
            sess: None,
            showing: false,
            pins: vec![],
            restore_popup: None,
            hello: None,
            scrolling: None,
            thumb: None,
            last_out: None,
            thumb_timer: slint::Timer::default(),
            toast_timer: slint::Timer::default(),
            delay_timer: slint::Timer::default(),
            me: Weak::new(),
            tools_model: ModelRc::new(VecModel::<crate::ToolItem>::default()),
            bar_model: ModelRc::new(VecModel::<crate::BarItem>::default()),
            bar_model_lib: ModelRc::new(VecModel::<crate::BarItem>::default()),
            want_out: None,
            on_tray_changed: None,
            keymap,
            children: vec![],
            last_box: None,
        }));
        app.borrow_mut().me = Rc::downgrade(&app);
        app.borrow_mut().rebuild_tools();
        app
    }

    fn rebuild_tools(&mut self) {
        let hidden: Vec<String> = self.cfg.get("hidden_tools").split(',').map(|s| s.trim().to_string()).collect();
        let mut v = vec![];
        for t in self.cfg.tool_order().iter() {
            let i = t.index();
            if hidden.contains(&t.id().to_string()) && *t != Tool::Select {
                continue;
            }
            v.push(crate::ToolItem {
                id: sh(t.id()),
                icon: sh(TOOL_ICONS[i]),
                key: sh(self.keymap.first(t.id())),
                name: sh(format!("{}  ·  {}", t.name(), self.keymap.all(t.id()))),
                sep: matches!(t, Tool::Pen | Tool::Blur),
            });
        }
        v.push(crate::ToolItem { id: sh("copy_text"), icon: sh(OCR_ICON), key: sh(self.keymap.first("copy_text").replace("Ctrl ", "")), name: sh("Copy text (OCR)"), sep: false });
        self.tools_model = ModelRc::new(VecModel::from(v));
        let item = |id: &str| crate::BarItem { id: sh(id), text: sh(keys::bar_text(id)), kbd: sh(self.keymap.first(id)) };
        let ids = keys::bar_list(self.cfg.get("bar"));
        self.bar_model = ModelRc::new(VecModel::from(ids.iter().map(|id| item(id)).collect::<Vec<_>>()));
        // a library picture also gets "Save as a copy" (before Done) unless the bar has it already
        let mut lib = ids.clone();
        if !lib.contains(&"save_copy") {
            let at = lib.iter().position(|id| *id == "done").unwrap_or(lib.len());
            lib.insert(at, "save_copy");
        }
        self.bar_model_lib = ModelRc::new(VecModel::from(lib.iter().map(|id| item(id)).collect::<Vec<_>>()));
    }

    pub fn reload_config(&mut self) {
        self.cfg = Config::load();
        self.keymap = self.cfg.keymap();
        self.rebuild_tools();
        let dark = self.cfg.dark();
        for o in &self.outs {
            crate::set_bars_look(&o.comp.global::<crate::B>(), dark);
            o.comp.set_tools(self.tools_model.clone());
            o.comp.set_bar_items(self.bar_model.clone());
        }
        crate::log::line("app", "settings reloaded");
    }

    // ---------------------------------------------------------------------------------------------------------
    // Outputs and the overlay components (made ahead of time: the fast freeze)

    pub fn current_outputs() -> Vec<OutInfo> {
        #[cfg(target_os = "linux")]
        if crate::wl::active() {
            return crate::wl::outputs()
                .into_iter()
                .map(|o| OutInfo { key: o.reg, name: o.name, x: o.x, y: o.y, w: o.w, h: o.h, pw: o.pw, ph: o.ph })
                .collect();
        }
        crate::platform_outputs()
    }

    pub fn prepare(&mut self) {
        let infos = Self::current_outputs();
        if self.outs.is_empty() {
            for i in &infos {
                crate::log::line("app", &format!("monitor {}: {} x {} px at {}, {} (scale {:.2})", i.name, i.pw, i.ph, i.x, i.y, i.scale()));
            }
        }
        self.outs.retain(|o| infos.iter().any(|i| i.key == o.info.key && i.pw == o.info.pw && i.ph == o.info.ph));
        for info in infos {
            if self.outs.iter().any(|o| o.info.key == info.key) {
                continue;
            }
            match self.make_overlay(&info) {
                Ok((comp, win)) => {
                    let handles = Rc::new(VecModel::default());
                    comp.set_sel_handles(ModelRc::from(handles.clone()));
                    // draw it once now (off screen): the first capture is as quick as the later ones
                    #[cfg(target_os = "linux")]
                    if crate::wl::active() {
                        let (w, h, sc) = (info.pw as u32, info.ph as u32, info.scale());
                        slint::Timer::single_shot(std::time::Duration::from_millis(300), move || crate::wl::prewarm(win, w, h, sc));
                    }
                    self.outs.push(Out { info, comp, win, layer_ver: 0, shown: false, dim_src: 0, live_idx: None, handles });
                }
                Err(e) => crate::log::line("app", &format!("overlay: {e}")),
            }
        }
    }

    fn make_overlay(&self, info: &OutInfo) -> Result<(Overlay, u32), String> {
        #[cfg(target_os = "linux")]
        let (comp, win) = if crate::wl::active() {
            let spec = crate::wl::Spec { place: crate::wl::Place::Fill { output: info.key }, keyboard: 1, namespace: "lasso-freeze" };
            let (c, id) = crate::wl::create(spec, Overlay::new);
            (c.map_err(|e| e.to_string())?, id)
        } else {
            (Overlay::new().map_err(|e| e.to_string())?, 0)
        };
        #[cfg(not(target_os = "linux"))]
        let (comp, win) = (Overlay::new().map_err(|e| e.to_string())?, 0u32);
        let _ = info;
        crate::set_bars_look(&comp.global::<crate::B>(), self.cfg.dark());
        comp.set_tools(self.tools_model.clone());
        let pal: Vec<slint::Color> = PALETTE.iter().map(|c| slint::Color::from_argb_u8(255, c[0], c[1], c[2])).collect();
        comp.set_palette(ModelRc::new(VecModel::from(pal)));
        comp.set_link_on(true);
        comp.set_bar_items(self.bar_model.clone());
        let key = info.key;
        let me = self.me.clone();
        let with = move |f: &dyn Fn(&mut App, usize)| {
            if let Some(a) = me.upgrade() {
                let mut a = a.borrow_mut();
                if let Some(i) = a.outs.iter().position(|o| o.info.key == key) {
                    f(&mut a, i);
                }
            }
        };
        let w = with.clone();
        comp.on_canvas_down(move |x, y, b| w(&|a, i| a.pointer(i, 0, x, y, b)));
        let w = with.clone();
        comp.on_canvas_move(move |x, y| w(&|a, i| a.pointer(i, 1, x, y, 0)));
        let w = with.clone();
        comp.on_canvas_up(move |x, y| w(&|a, i| a.pointer(i, 2, x, y, 0)));
        let w = with.clone();
        comp.on_canvas_double(move |x, y| w(&|a, i| a.pointer(i, 3, x, y, 0)));
        let w = with.clone();
        comp.on_canvas_wheel(move |x, y, d| w(&|a, i| a.wheel(i, x, y, d)));
        let w = with.clone();
        comp.on_cmd(move |c| {
            let c = c.to_string();
            w(&|a, i| a.command(i, &c))
        });
        let w = with.clone();
        comp.on_size_set(move |v| w(&|a, i| a.size_set(i, v)));
        let me2 = self.me.clone();
        let phys: Rc<RefCell<Option<char>>> = Rc::new(RefCell::new(None));
        let ph2 = phys.clone();
        comp.on_key(move |text, ctrl, shift, alt, repeat| {
            let ev = keys::from_slint(&text, ctrl, alt, shift, repeat, *ph2.borrow());
            if let Some(a) = me2.upgrade() {
                if let Ok(mut a) = a.try_borrow_mut() {
                    // winit path (Windows / GNOME): the modifiers for Shift / Alt drags, Shift+click, Ctrl+wheel
                    let k = ev.key.as_str();
                    a.mods(ctrl || k.starts_with("Control"), shift || k.starts_with("Shift"), alt || k.starts_with("Alt"));
                    a.key_in(ev, true);
                    return true;
                }
            }
            false
        });
        let me3 = self.me.clone();
        comp.on_key_up(move |text, ctrl, shift, alt| {
            let ev = keys::from_slint(&text, ctrl, alt, shift, false, None);
            if let Some(a) = me3.upgrade() {
                if let Ok(mut a) = a.try_borrow_mut() {
                    let k = ev.key.as_str();
                    a.mods(ctrl && !k.starts_with("Control"), shift && !k.starts_with("Shift"), alt && !k.starts_with("Alt"));
                    a.key_in(ev, false);
                }
            }
        });
        crate::winkeys::hook_physical(&comp, phys);
        #[cfg(windows)]
        crate::win::prewarm_overlay(&comp);
        Ok((comp, win))
    }

    // ---------------------------------------------------------------------------------------------------------
    // Capture

    /// The capture key: freeze, then pick.
    pub fn capture(&mut self, mode: Mode, with_delay: bool) {
        if self.showing || self.scrolling.is_some() {
            return;
        }
        let delay = if with_delay { self.cfg.num("delay").clamp(0, 30) } else { 0 };
        if delay > 0 {
            let me = self.me.clone();
            crate::log::line("capture", &format!("timer {delay} s"));
            self.delay_timer.start(slint::TimerMode::SingleShot, std::time::Duration::from_secs(delay as u64), move || {
                if let Some(a) = me.upgrade() {
                    a.borrow_mut().capture(mode, false);
                }
            });
            return;
        }
        let t0 = Instant::now();
        self.hide_popups();
        self.prepare();
        let t_prep = t0.elapsed();
        let infos: Vec<OutInfo> = self.outs.iter().map(|o| o.info.clone()).collect();
        let shots = match crate::grab(&infos, self.cfg.on("pointer")) {
            Ok(s) => s,
            Err(e) => {
                crate::log::line("capture", &format!("failed: {e}"));
                crate::notify("Lasso can't take a screenshot", &e);
                return;
            }
        };
        let t_grab = t0.elapsed();
        let keymap = self.keymap.clone();
        // the user, 20 Nov 2026: a drag in the middle of a fresh box must not draw - start with Select (drag = move the
        // box); drawing starts once a tool is picked (button or key). "Start with the last tool" is the opt-in.
        let tool = if self.cfg.on("remember_tool") { Tool::from_id(self.cfg.get("last_tool")).unwrap_or(Tool::Select) } else { Tool::Select };
        // a monitor without a new picture must not show the previous capture
        for o in self.outs.iter_mut() {
            o.comp.set_bg(Image::default());
            o.comp.set_bg_dim(Image::default());
            o.comp.set_bg_light(Image::default());
            o.comp.set_light_ready(false);
            o.comp.set_dim_ready(false);
            o.dim_src = 0;
            o.comp.set_layer(Image::default());
        }
        let got: Vec<OutInfo> = infos.iter().filter(|i| shots.iter().any(|s| s.0 == i.key)).cloned().collect();
        if got.is_empty() {
            crate::notify("Lasso", "No screen to capture");
            return;
        }
        // every monitor in ONE picture, each at its own pixels (a box can go from one to the other), plus a lighter
        // copy (picking) and a darker one (outside the box), made in row bands on every core: the first frame then only
        // copies pixels and the UI thread copies nothing (the user, 20 Nov 2026: "the time from the key to the UI is
        // quite slow"). One monitor: its own picture, as before.
        let (parts, w, h) = span_layout(&got);
        let (pic, [plain, light, dark]) = stitch_parts(&parts, w, h, shots);
        let pic = Arc::new(pic);
        let mut ed = Editor::new(pic.clone(), (pic.w as f32, pic.h as f32), keymap);
        ed.tool = tool;
        let cfg = &self.cfg;
        ed.load_styles(|k| cfg.get(k));
        if parts.len() > 1 {
            ed.screens = parts.iter().map(|p| R::new(p.x, p.y, p.w, p.h)).collect();
        }
        let (plain, light, dark) = (Image::from_rgba8_premultiplied(plain), Image::from_rgba8_premultiplied(light), Image::from_rgba8_premultiplied(dark));
        let src = Arc::as_ptr(&pic) as usize;
        for o in self.outs.iter_mut().filter(|o| parts.iter().any(|p| p.key == o.info.key)) {
            o.comp.set_bg(plain.clone());
            o.comp.set_bg_light(light.clone());
            o.comp.set_light_ready(true);
            o.comp.set_bg_dim(dark.clone());
            o.comp.set_dim_ready(true);
            o.dim_src = src;
            o.layer_ver = 0;
        }
        self.set_editor(ed, parts);
        self.dim_img = Some((src, dark));
        let t_set = t0.elapsed();
        let kept = self.cfg.get("library_keep") == "all";
        self.sess = Some(Sess { id: library::new_id(), created: crate::util::unix_now(), fresh: true, kept, ..Default::default() });
        // Wayland doesn't say where the mouse is: Screen mode takes the monitor it was last on
        let at = self.part_of(self.active.min(self.outs.len().saturating_sub(1))).map(|p| (p.x + p.w / 2.0, p.y + p.h / 2.0));
        if let Some(e) = self.ed.as_mut() {
            if let Some(at) = at {
                e.mouse = at;
            }
            e.set_mode(mode);
        }
        self.sync_all();
        let t_sync = t0.elapsed();
        self.show_all();
        crate::sound::play(crate::sound::Snd::Freeze);
        crate::log::line("capture", &format!("frozen screen up: grab {} ms, total {} ms", t_grab.as_millis(), t0.elapsed().as_millis()));
        if std::env::var_os("LASSO_DEBUG_WL").is_some() {
            crate::log::line("capture", &format!("steps: prepare {} us, grab done {} us, pictures set {} us, synced {} us, shown {} us", t_prep.as_micros(), t_grab.as_micros(), t_set.as_micros(), t_sync.as_micros(), t0.elapsed().as_micros()));
        }
        self.fetch_windows();
        self.store_async(true);
    }

    /// Repeat the last box: capture and go straight to the tools with the same box
    pub fn repeat_last(&mut self) {
        let Some((name, r)) = self.last_box.clone() else {
            self.capture(Mode::Area, true);
            return;
        };
        self.capture(Mode::Area, false);
        if let Some(r) = self.box_from_last(&name, r, true) {
            if let Some(e) = self.ed.as_mut() {
                e.set_box(r);
            }
            self.sync_all();
        }
    }

    /// The remembered box (monitor name + box on it, "*" = over several) in this capture's picture; `any` = also when
    /// that monitor isn't part of it (only one screen)
    fn box_from_last(&self, name: &str, r: R, any: bool) -> Option<R> {
        if name == "*" {
            return (self.parts.len() > 1).then_some(r);
        }
        if let Some(p) = self.outs.iter().find(|o| o.info.name == name).and_then(|o| self.parts.iter().find(|p| p.key == o.info.key)) {
            return Some(r.moved(p.x, p.y));
        }
        (any || self.parts.len() == 1).then_some(r)
    }

    /// The box to remember for Ctrl+Shift+B / --repeat: on its monitor, or "*" when it is over several
    fn remember_box(&mut self) {
        let Some(b) = self.ed.as_ref().and_then(|e| e.boxr) else { return };
        self.last_box = Some(match self.box_part() {
            Some((i, p)) => (self.outs[i].info.name.clone(), b.moved(-p.x, -p.y)),
            None => ("*".into(), b),
        });
    }

    fn fetch_windows(&self) {
        let me = self.me.clone();
        let outs: Vec<OutInfo> = self.outs.iter().map(|o| o.info.clone()).collect();
        let _ = me;
        std::thread::spawn(move || {
            let wins = crate::window_list();
            let _ = slint::invoke_from_event_loop(move || {
                crate::with_app(|a| a.set_windows(&outs, wins));
            });
        });
    }

    pub fn set_windows(&mut self, _outs: &[OutInfo], wins: Vec<(R, String)>) {
        let Some((pw, ph)) = self.ed.as_ref().map(|e| (e.doc.base.w as f32, e.doc.base.h as f32)) else { return };
        let mut list: Vec<(R, String)> = vec![];
        for (r, n) in wins.iter().filter(|(_, cls)| !cls.to_lowercase().contains("lasso")) {
            // through the monitor its middle is on (desktop layout -> that monitor's pixels -> its place in the picture)
            let c = (r.x + r.w / 2.0, r.y + r.h / 2.0);
            let on = self.outs.iter().filter_map(|o| self.parts.iter().find(|p| p.key == o.info.key).map(|p| (o, p)));
            let Some((o, p)) = on.min_by(|a, b| rect_dist(&logical_rect(&a.0.info), c).total_cmp(&rect_dist(&logical_rect(&b.0.info), c))) else { continue };
            let s = o.info.scale();
            let pr = R::new((r.x - o.info.x as f32) * s + p.x, (r.y - o.info.y as f32) * s + p.y, r.w * s, r.h * s);
            if pr.r() > 0.0 && pr.b() > 0.0 && pr.x < pw && pr.y < ph {
                list.push((pr, app_name(n, pr.w * pr.h >= 0.9 * p.w * p.h)));
            }
        }
        if let Some(e) = self.ed.as_mut() {
            e.window_names = list.iter().map(|(_, n)| n.clone()).collect();
            e.windows = list.into_iter().map(|(r, _)| r).collect();
        }
    }

    /// Full screenshot, no overlay: copy + save + corner thumbnail.
    pub fn full_shot(&mut self) {
        if self.showing || self.scrolling.is_some() {
            return;
        }
        let t0 = Instant::now();
        self.hide_popups();
        let infos = Self::current_outputs();
        let all = self.cfg.get("full_mode") == "all";
        let shots = match crate::grab(&infos, self.cfg.on("pointer")) {
            Ok(s) => s,
            Err(e) => {
                crate::notify("Lasso can't take a screenshot", &e);
                return;
            }
        };
        let pic = if all && shots.len() > 1 {
            stitch(&infos, shots)
        } else {
            // the monitor the pointer was last on (Lasso can't ask Wayland where the mouse is): the active one
            let want = self.outs.get(self.active).map(|o| o.info.key);
            #[cfg(windows)]
            let want = crate::win::monitor_under_mouse().or(want);
            // "pick a monitor": full_mode = 1, 2, 3 = that monitor counted from the left
            let want = match self.cfg.get("full_mode").parse::<usize>() {
                Ok(n) if n >= 1 => {
                    let mut by_x: Vec<&OutInfo> = infos.iter().collect();
                    by_x.sort_by_key(|o| (o.x, o.y));
                    by_x.get(n - 1).map(|o| o.key).or(want)
                }
                _ => want,
            };
            let mut shots = shots;
            let idx = shots.iter().position(|s| Some(s.0) == want).unwrap_or(0);
            shots.swap_remove(idx).1
        };
        crate::log::line("capture", &format!("full screenshot {} x {} in {} ms", pic.w, pic.h, t0.elapsed().as_millis()));
        let pic = Arc::new(pic);
        let id = library::new_id();
        let st = library::State { created: crate::util::unix_now(), boxr: Some(R::new(0.0, 0.0, pic.w as f32, pic.h as f32)), mode: Some(Mode::Screen), ..Default::default() };
        // copy now, save + library in the background (light: no window until the picture is in memory)
        self.copy_pic(&pic);
        let dir = self.cfg.save_dir();
        let pat = self.cfg.get("name_pattern").to_string();
        let fmt = self.cfg.get("format").to_string();
        let fits = self.cfg.on("fits_discord");
        let p2 = pic.clone();
        let id2 = id.clone();
        // a full screenshot is copied + saved = used: in the library unless Settings says nothing goes there
        let keep_it = self.cfg.get("library_keep") != "none";
        let echo = crate::echo_link::target(&self.cfg);
        std::thread::spawn(move || {
            let enc = crate::export::encode(&p2, &fmt, fits, false);
            let path = crate::export::free_name(&dir, &crate::util::name_from_pattern(&pat, "Lasso"), enc.ext);
            let res = crate::export::write_new(&path, &enc.bytes);
            let mut st = st;
            if res.is_ok() {
                st.saved.push(path.to_string_lossy().to_string());
                crate::echo_link::mirror(echo, &id2, &path);
            }
            if keep_it {
                let _ = library::store(&id2, &p2, true, &st, &Default::default(), Some(&p2));
            }
            let saved = res.is_ok().then(|| path.to_string_lossy().to_string());
            let note = match res {
                Ok(()) => format!("Copied + saved · {}", path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default()),
                Err(e) => format!("Copied · not saved: {e}"),
            };
            let _ = slint::invoke_from_event_loop(move || {
                crate::with_app(|a| {
                    a.set_thumb_note(&note);
                    if let Some(p) = saved {
                        a.thumb_saved(&id2, p);
                    }
                })
            });
        });
        self.last_out = Some((id.clone(), vec![], std::time::Instant::now()));
        if self.cfg.on("thumb_after_full") {
            self.show_thumb(&pic, &id, "Copied", vec![]);
        }
    }

    // ---------------------------------------------------------------------------------------------------------
    // Showing / hiding

    fn show_all(&mut self) {
        // a fresh start for Ctrl / Shift / Alt: the key-ups of the shortcut that closed it last time never came (D5)
        self.mods(false, false, false);
        self.esc_down = None;
        self.showing = true;
        for o in self.outs.iter_mut() {
            if self.parts.iter().any(|p| p.key == o.info.key) && !o.shown {
                crate::show_overlay(&o.comp, &o.info);
                o.shown = true;
            }
        }
        if let Some(f) = &self.on_tray_changed {
            f(None);
        }
    }

    fn hide_all(&mut self) {
        self.mods(false, false, false);
        for o in self.outs.iter_mut() {
            if o.shown {
                let _ = o.comp.hide();
                o.shown = false;
            }
            // light while waiting in the tray: the editor keeps the picture for Restore, the screen copies go
            o.comp.set_bg(Image::default());
            o.comp.set_bg_dim(Image::default());
            o.comp.set_bg_light(Image::default());
            o.comp.set_light_ready(false);
            o.comp.set_dim_ready(false);
            o.dim_src = 0;
            o.comp.set_layer(Image::default());
            o.comp.set_mag(Image::default());
            o.layer_ver = 0;
            o.live_idx = None;
        }
        self.layer_img = None;
        self.rest_img = None;
        self.dim_img = None;
        self.showing = false;
        if let Some(f) = &self.on_tray_changed {
            let t = self.sess.as_ref().map(|s| s.created);
            f(t);
        }
        crate::trim_memory_soon();
    }

    pub fn has_restore(&self) -> Option<i64> {
        if self.ed.as_ref().map(|e| e.boxr.is_some()).unwrap_or(false) {
            return self.sess.as_ref().map(|s| s.created);
        }
        None
    }

    /// Esc: close at once, keep everything (memory + library). First time ever: the small popup.
    fn settle_all(&mut self) {
        if let Some(e) = self.ed.as_mut() {
            e.settle();
        }
    }

    fn close_keep(&mut self) {
        self.settle_all();
        self.remember_box();
        self.remember_tool();
        self.hide_all();
        self.store_async(false);
        if !self.cfg.on("esc_hint_seen") {
            self.show_restore_popup();
        }
    }

    /// Shift+Esc: close and throw it away - no Restore, nothing (more) in the library. A library edit only loses the
    /// changes made now (the library entry itself stays).
    fn discard(&mut self) {
        self.remember_tool();
        self.hide_all();
        self.drop_editor();
        if let Some(s) = self.sess.take() {
            if s.fresh {
                library::cancel(&s.id);
            }
        }
        if let Some(f) = &self.on_tray_changed {
            f(None);
        }
        crate::log::line("capture", "thrown away (Shift+Esc)");
    }

    /// Esc with nothing picked: nothing to keep
    fn cancel(&mut self) {
        self.remember_tool();
        self.hide_all();
        self.drop_editor();
        // the library entry made by THIS capture goes away again (never an older capture: library edit, restore)
        if let Some(s) = self.sess.take() {
            if s.fresh {
                library::cancel(&s.id);
            }
        }
        if let Some(f) = &self.on_tray_changed {
            f(None);
        }
    }

    /// The Library deleted a capture: no more writes of it, and Restore doesn't bring it back (review D4)
    pub fn forget(&mut self, id: &str) {
        if id.is_empty() || id.contains('/') || id.contains("..") {
            return;
        }
        library::forget(id);
        if !self.showing && self.sess.as_ref().map(|s| s.id == id).unwrap_or(false) {
            self.sess = None;
            self.drop_editor();
            if let Some(f) = &self.on_tray_changed {
                f(None);
            }
        }
    }

    pub fn restore(&mut self) {
        self.hide_popups();
        if self.showing {
            return;
        }
        if self.has_restore().is_some() {
            self.prepare();
            self.settle_all();
            if let Some(e) = self.ed.as_ref() {
                let img = rgba_image(&e.doc.base);
                for o in self.outs.iter_mut().filter(|o| self.parts.iter().any(|p| p.key == o.info.key)) {
                    o.comp.set_bg(img.clone());
                    o.layer_ver = 0;
                }
            }
            self.sync_all();
            self.show_all();
            return;
        }
        // after a restart: the newest library capture
        let items = library::list(&self.cfg.save_dir());
        if let Some(it) = items.iter().find(|i| !i.id.is_empty()) {
            let id = it.id.clone();
            self.edit_library(&id);
        } else {
            crate::notify("Lasso", "Nothing to restore yet");
        }
    }

    /// The last tool + every tool's colour / size / fill / shadow are kept for the next capture (config.ini)
    fn remember_tool(&mut self) {
        let Some(e) = self.ed.as_ref() else { return };
        let mut changes: Vec<(String, String)> = e.styles_text().into_iter().filter(|(k, v)| self.cfg.get(k) != v).collect();
        let t = e.tool.id().to_string();
        if self.cfg.get("last_tool") != t && self.cfg.on("remember_tool") {
            changes.push(("last_tool".into(), t));
        }
        if changes.is_empty() {
            return;
        }
        let mut c = Config::load();
        for (k, v) in changes {
            c.set(&k, v.clone());
            self.cfg.set(&k, v);
        }
        c.save();
    }

    /// Writes the capture (picture + box + shapes) to the library in the background - a fresh capture only once it's
    /// kept (Settings > Keep in the library; "the used ones" = after copy / save / pin / link).
    fn store_async(&mut self, first: bool) {
        let Some(sess) = self.sess.clone() else { return };
        if sess.fresh && !sess.kept {
            return;
        }
        // its first write: the library limit is applied after it (with "used ones" that isn't the capture moment)
        let first_write = sess.fresh && sess.stored_out.is_none();
        let Some(e) = self.ed.as_ref() else { return };
        // a capture over several monitors: the library keeps the picture of the monitor the box is on (as before
        // the joined picture; no box yet = the one the mouse is on), the joined one only for a box over several
        let region = if self.parts.len() > 1 {
            match e.boxr {
                Some(_) => self.box_part(),
                None => self.part_of(self.active).map(|p| (self.active, p)),
            }
        } else {
            None
        };
        let (out_i, cut) = match region {
            Some((i, p)) => (Some(i), Some(p)),
            None if self.parts.len() > 1 => (None, None),
            None => (self.outs.iter().position(|o| self.parts.first().map(|p| p.key) == Some(o.info.key)), None),
        };
        let base = e.doc.base.clone();
        let images = e.doc.images.clone();
        let mut shapes = e.doc.shapes.clone();
        let mut boxr = e.boxr;
        let mut history = e.history();
        if let Some(p) = cut {
            shapes.iter_mut().for_each(|s| s.translate(-p.x, -p.y));
            boxr = boxr.map(|b| b.moved(-p.x, -p.y));
            for (hb, hs) in history.iter_mut() {
                hs.iter_mut().for_each(|s| s.translate(-p.x, -p.y));
                *hb = hb.map(|b| b.moved(-p.x, -p.y));
            }
        }
        let st = library::State {
            created: sess.created,
            boxr,
            shapes,
            mode: Some(e.mode),
            output: out_i.map(|i| self.outs[i].info.name.clone()).unwrap_or_default(),
            saved: sess.saved.clone(),
            source: sess.source.clone(),
            history,
            ..Default::default()
        };
        let thumb = if first { None } else { e.flatten() };
        let app = if sess.app.is_empty() { self.box_app() } else { sess.app.clone() };
        let st = library::State { app, ..st };
        // which picture screen.png holds: that monitor's, or all of them joined (SPAN_ALL)
        let out_key = Some(match out_i {
            Some(i) => self.outs[i].info.key,
            None => SPAN_ALL,
        });
        // library edits keep their screen.png (u32::MAX); a capture rewrites it when the box is on another monitor
        let base_dirty = base_needs_write(sess.stored_out, out_key);
        if let Some(s) = self.sess.as_mut() {
            if s.stored_out != Some(u32::MAX) {
                s.stored_out = out_key;
            }
        }
        let id = sess.id.clone();
        let keep = (self.cfg.num("keep_count").max(1) as usize, self.cfg.num("keep_days"));
        library::queue(&id, move |id| {
            // only that monitor's part of the joined picture (copied here, off the UI thread)
            let base = match cut {
                Some(p) if base_dirty || first => Arc::new(base.crop(R::new(p.x, p.y, p.w, p.h).ir())),
                _ => base,
            };
            let t = thumb.as_ref().or(if first { Some(&*base) } else { None });
            if let Err(e) = library::store(id, &base, base_dirty, &st, &images, t) {
                crate::log::line("library", &format!("store: {e}"));
                return;
            }
            if first_write {
                library::prune_after_new(keep.0, keep.1);
            }
            // the text in it, for the library's search (quietly skipped when OCR isn't there); its own thread
            if let Some(p) = thumb.filter(|_| !first) {
                library::read_text_later(id, p);
            }
        });
    }

    /// A new editor on the frozen screen(s): `parts` = where each monitor showing it sits in its picture
    fn set_editor(&mut self, ed: Editor, parts: Vec<Part>) {
        let bar = if ed.lib_view { self.bar_model_lib.clone() } else { self.bar_model.clone() };
        for o in &self.outs {
            o.comp.set_bar_items(bar.clone());
        }
        self.ed = Some(ed);
        self.parts = parts;
        self.last_move = (-1.0, -1.0);
        self.layer_img = None;
        self.rest_img = None;
        self.dim_img = None;
        for o in self.outs.iter_mut() {
            o.layer_ver = 0;
            o.live_idx = None;
        }
    }

    fn drop_editor(&mut self) {
        self.ed = None;
        self.parts.clear();
        self.layer_img = None;
        self.rest_img = None;
        self.dim_img = None;
        for o in self.outs.iter_mut() {
            o.comp.set_bg(Image::default());
            o.comp.set_layer(Image::default());
        }
    }

    fn part_of(&self, i: usize) -> Option<Part> {
        let k = self.outs.get(i)?.info.key;
        self.parts.iter().find(|p| p.key == k).copied()
    }

    /// The monitor (index in outs) whose part holds the whole box: None = no box, or a box over several monitors.
    /// Only one part (one monitor, a library picture): that one.
    fn box_part(&self) -> Option<(usize, Part)> {
        let b = self.ed.as_ref()?.boxr?;
        let found = if self.parts.len() == 1 {
            self.parts.first().copied()
        } else {
            self.parts.iter().find(|p| b.x >= p.x - 0.5 && b.y >= p.y - 0.5 && b.r() <= p.x + p.w + 0.5 && b.b() <= p.y + p.h + 0.5).copied()
        }?;
        self.outs.iter().position(|o| o.info.key == found.key).map(|i| (i, found))
    }

    /// The monitor the bars and popups go on: the one showing the most of the box; without a box the one the
    /// pointer is on
    fn ui_out(&self) -> usize {
        let fallback = self.active.min(self.outs.len().saturating_sub(1));
        let Some(e) = self.ed.as_ref() else { return fallback };
        let Some(b) = e.boxr.filter(|_| self.parts.len() > 1) else { return fallback };
        let r = e.view.rect(&b);
        let area = |p: &Part| ((r.r().min(p.x + p.w) - r.x.max(p.x)).max(0.0)) * ((r.b().min(p.y + p.h) - r.y.max(p.y)).max(0.0));
        let mut best = (fallback, self.part_of(fallback).map(|p| area(&p)).unwrap_or(-1.0));
        for (i, o) in self.outs.iter().enumerate() {
            if let Some(p) = self.parts.iter().find(|p| p.key == o.info.key) {
                let a = area(p);
                if a > best.1 + 0.5 {
                    best = (i, a);
                }
            }
        }
        best.0
    }

    /// The monitor whose part of the joined screen holds this point
    fn out_at(&self, g: (f32, f32)) -> Option<usize> {
        self.outs.iter().position(|o| self.parts.iter().any(|p| p.key == o.info.key && g.0 >= p.x && g.1 >= p.y && g.0 < p.x + p.w && g.1 < p.y + p.h))
    }

    /// A pointer position on monitor `i`'s frozen screen (logical px) -> the joined screen (physical px). Outside
    /// that monitor (a drag that went on to another one: Wayland / Windows keep sending it to the surface where it
    /// started): through the desktop layout to the monitor it is over, with that monitor's scale.
    fn to_global(&self, i: usize, x: f32, y: f32) -> (f32, f32) {
        let mons: Vec<(OutInfo, Part, f32)> = self.outs.iter().enumerate().filter_map(|(j, o)| self.part_of(j).map(|p| (o.info.clone(), p, self.scale_of(j)))).collect();
        let from = mons.iter().position(|m| m.0.key == self.outs[i].info.key);
        match from {
            Some(f) => joined_point(&mons, f, x, y),
            None => {
                let sc = self.scale_of(i);
                (x * sc, y * sc)
            }
        }
    }

    // ---------------------------------------------------------------------------------------------------------
    // Input

    fn pointer(&mut self, i: usize, kind: u8, x: f32, y: f32, button: i32) {
        if self.ed.is_none() {
            return;
        }
        let sc = self.scale_of(i);
        // one joined screen for every monitor: a drag can go on from one monitor into the next
        let g = self.to_global(i, x, y);
        if kind == 1 && self.last_move == g {
            return;
        }
        if kind <= 1 {
            if let Some(j) = self.out_at(g) {
                self.active = j;
            }
        }
        if kind == 1 {
            self.last_move = g;
        }
        let Some(ed) = self.ed.as_mut() else { return };
        ed.ui_scale = sc;
        let fx = match kind {
            0 => ed.pointer_down(g.0, g.1, button as u8),
            1 => {
                ed.pointer_move(g.0, g.1);
                vec![]
            }
            2 => ed.pointer_up(g.0, g.1),
            _ => ed.double_click(g.0, g.1),
        };
        let t = Instant::now();
        self.sync_all();
        if kind == 1 && std::env::var_os("LASSO_DEBUG_WL").is_some() {
            crate::log::line("app", &format!("move sync {} us", t.elapsed().as_micros()));
        }
        self.cursor(i);
        self.run_fx(i, fx);
    }

    fn wheel(&mut self, i: usize, x: f32, y: f32, d: f32) {
        let g = self.to_global(i, x, y);
        if let Some(ed) = self.ed.as_mut() {
            ed.wheel(g.0, g.1, -d);
        }
        self.sync_all();
    }

    fn size_set(&mut self, _i: usize, v: f32) {
        if let Some(ed) = self.ed.as_mut() {
            ed.set_size(v);
        }
        self.sync_all();
    }

    fn command(&mut self, i: usize, c: &str) {
        let fx = {
            let Some(ed) = self.ed.as_mut() else { return };
            if let Some(m) = c.strip_prefix("mode:") {
                ed.set_mode(Mode::from_index(m.parse().unwrap_or(0)));
                vec![]
            } else {
                ed.action(c)
            }
        };
        self.sync_all();
        self.run_fx(i, fx);
    }

    /// A key from the layer-shell platform (KDE) or the winit FocusScope (Windows / GNOME).
    pub fn key_in(&mut self, ev: KeyEv, pressed: bool) {
        let i = self.active.min(self.outs.len().saturating_sub(1));
        if !self.showing {
            return;
        }
        if ev.key.starts_with("Shift") || ev.key.starts_with("Control") || ev.key.starts_with("Alt") {
            return;
        }
        // Esc acts on its RELEASE: closing on the press gave the key-up (or a synthetic press) to the app behind -
        // a full-screen browser left full screen, games react to Esc going up (the user, 21 Nov 2026)
        let Some(ev) = esc_gate(&mut self.esc_down, ev, pressed) else { return };
        let Some(ed) = self.ed.as_mut() else { return };
        if !pressed && !ev.is("Escape") {
            ed.key_up(&ev);
            return;
        }
        let fx = ed.key(&ev);
        self.sync_all();
        self.cursor(i);
        self.run_fx(i, fx);
    }

    pub fn key_for_win(&mut self, win: u32, ev: KeyEv, pressed: bool) {
        if let Some(p) = self.pins.iter().position(|p| p.win == win) {
            if pressed && ev.plain() && (ev.is("t") || (!ev.key.is_ascii() && ev.phys == Some('t'))) {
                // T: click-through (pin a map / build order over a game); see-through follows Ctrl+wheel
                let pin = &mut self.pins[p];
                pin.through = true;
                if pin.see > 0.8 {
                    pin.see = 0.7;
                    pin.comp.set_see(pin.see);
                }
                crate::pin_click_through(&pin.comp, pin.win, true);
                crate::notify("Lasso", "The pin lets clicks through now. Tray menu > \"Make pins clickable again\" brings it back.");
            } else if pressed && (ev.is("Escape") || (ev.ctrl && ev.is("w"))) {
                self.close_pin(p);
            } else if pressed && ev.ctrl && ev.is("c") {
                let pic = self.pins[p].pic.clone();
                self.copy_pic(&pic);
            }
            return;
        }
        if let Some(i) = self.outs.iter().position(|o| o.win == win) {
            if self.ed.as_ref().map(|e| e.boxr.is_none()).unwrap_or(true) {
                self.active = i;
            }
        }
        self.key_in(ev, pressed);
    }

    pub fn mods(&mut self, ctrl: bool, shift: bool, alt: bool) {
        if let Some(e) = self.ed.as_mut() {
            e.set_mods(ctrl, shift, alt);
        }
        crate::MODS.with(|m| m.set((ctrl, shift, alt)));
    }

    /// Physical px per logical px of this monitor's frozen screen: the surface's own scale (what the compositor
    /// really uses), the monitor list's only until the surface is up
    fn scale_of(&self, i: usize) -> f32 {
        let o = &self.outs[i];
        if o.shown {
            let s = o.comp.window().scale_factor();
            if s > 0.0 {
                return s;
            }
        }
        o.info.scale().max(0.01)
    }

    pub fn configured(&mut self, win: u32) {
        if let Some(i) = self.outs.iter().position(|o| o.win == win) {
            if self.ed.is_some() {
                self.outs[i].layer_ver = 0;
                self.outs[i].live_idx = None;
                self.sync_all();
            }
        }
    }

    pub fn pointer_on(&mut self, win: u32) {
        if let Some(i) = self.outs.iter().position(|o| o.win == win) {
            self.active = i;
            self.cursor(i);
        }
    }

    fn cursor(&self, i: usize) {
        #[cfg(target_os = "linux")]
        {
            let o = &self.outs[i];
            let Some(e) = self.ed.as_ref() else { return };
            // the pointer's monitor shows no part of the box: it looks + acts like picking there (a drag = a new box)
            let elsewhere = self.parts.len() > 1 && self.out_at(e.view.to_screen(e.mouse)).map(|j| !self.box_here(j)).unwrap_or(false);
            let shape = if o.comp.get_over_ui() {
                "default"
            } else if e.phase == Phase::Pick || elsewhere {
                if e.mode == Mode::Area {
                    "crosshair"
                } else {
                    "pointer"
                }
            } else {
                let m = e.mouse;
                let b = e.boxr.unwrap_or_default();
                let t = 9.0 / e.view.s;
                let near = |x: f32, y: f32| (m.0 - x).abs() <= t && (m.1 - y).abs() <= t;
                if near(b.x, b.y) || near(b.r(), b.b()) {
                    "nwse"
                } else if near(b.r(), b.y) || near(b.x, b.b()) {
                    "nesw"
                } else if near(b.x + b.w / 2.0, b.y) || near(b.x + b.w / 2.0, b.b()) {
                    "ns"
                } else if near(b.x, b.y + b.h / 2.0) || near(b.r(), b.y + b.h / 2.0) {
                    "ew"
                } else if !b.contains(m) {
                    "default"
                } else {
                    match e.tool {
                        Tool::Select => "move",
                        Tool::Text => "text",
                        Tool::Picker | Tool::Step => "pointer",
                        _ => "crosshair",
                    }
                }
            };
            crate::wl::set_cursor(o.win, shape);
        }
        #[cfg(not(target_os = "linux"))]
        let _ = i;
    }

    /// Does monitor `i` show a part of the box? (a box only on another monitor: this one looks like picking)
    #[cfg(target_os = "linux")]
    fn box_here(&self, i: usize) -> bool {
        let (Some(e), Some(p)) = (self.ed.as_ref(), self.part_of(i)) else { return false };
        let Some(b) = e.boxr else { return false };
        if self.parts.len() < 2 {
            return true;
        }
        touches(&e.view.rect(&b), &R::new(p.x, p.y, p.w, p.h))
    }

    // ---------------------------------------------------------------------------------------------------------
    // Effects

    fn run_fx(&mut self, i: usize, fx: Vec<Fx>) {
        for f in fx {
            match f {
                Fx::Toast(t) => self.toast(i, &t),
                Fx::Clip(t) => {
                    crate::clipboard::set_text(&t);
                }
                Fx::Cancel => self.cancel(),
                Fx::Discard => self.discard(),
                Fx::Close => self.close_keep(),
                Fx::NewBox => {
                    if let Some(e) = self.ed.as_mut() {
                        e.reset_box();
                    }
                    self.sync_all();
                }
                Fx::Done => {
                    let what = self.cfg.get("enter_does").to_string();
                    match what.as_str() {
                        "save" => self.output(i, "save"),
                        "link" => self.output(i, "link"),
                        "copysave" => self.output(i, "copysave"),
                        _ => self.output(i, "copy"),
                    }
                }
                Fx::Copy => self.output(i, "copy"),
                Fx::Save => self.output(i, "save"),
                Fx::SaveAs => self.output(i, "save_as"),
                Fx::SaveCopy => self.save_copy(i),
                Fx::Link => self.output(i, "link"),
                Fx::Pin => self.output(i, "pin"),
                Fx::CopySave => self.output(i, "copysave"),
                Fx::CopyStay => {
                    if let Some(p) = self.finished_pic(i) {
                        self.copy_pic(&p);
                        self.toast(i, "Copied - still editing");
                    }
                }
                Fx::Scroll => self.start_scroll(i),
                Fx::LastBox => match self.last_box.clone().and_then(|(n, r)| self.box_from_last(&n, r, false)) {
                    Some(r) => {
                        if let Some(e) = self.ed.as_mut() {
                            e.set_box(r);
                        }
                        self.sync_all();
                    }
                    None => self.toast(i, "No earlier box on this screen yet"),
                },
                Fx::Background => {
                    let on = {
                        let s = self.sess.get_or_insert_with(Default::default);
                        s.background = !s.background;
                        s.background
                    };
                    self.toast(i, if on { "Background on: copy / save add padding + shadow" } else { "Background off" });
                }
                Fx::CopyText => self.ocr(i, false),
                Fx::Redact => self.ocr(i, true),
                Fx::Paste => self.paste(i),
                Fx::Open => {
                    self.close_keep();
                    crate::open_dialog();
                }
                Fx::Recent => {
                    self.close_keep();
                    self.open_window("--library-window");
                }
                Fx::Settings => {
                    self.close_keep();
                    self.open_window("--settings-window");
                }
                Fx::Timer => {
                    let next = match self.cfg.num("delay") {
                        0 => 3,
                        3 => 5,
                        5 => 10,
                        _ => 0,
                    };
                    let mut c = Config::load();
                    c.set("delay", next.to_string());
                    c.save();
                    self.cfg.set("delay", next.to_string());
                    let picking = self.ed.as_ref().map(|e| e.boxr.is_none()).unwrap_or(false);
                    if next > 0 && picking {
                        // start over with the timer: the screen is taken again in N seconds
                        let mode = self.ed.as_ref().map(|e| e.mode).unwrap_or(Mode::Area);
                        self.cancel();
                        self.capture(mode, true);
                    } else {
                        self.toast(i, &if next > 0 { format!("Timer {next} s for the next capture") } else { "Timer off".into() });
                    }
                }
                Fx::Pointer => {
                    let on = !self.cfg.on("pointer");
                    let mut c = Config::load();
                    c.set("pointer", if on { "1" } else { "0" });
                    c.save();
                    self.cfg.set("pointer", if on { "1" } else { "0" });
                    let picking = self.ed.as_ref().map(|e| e.boxr.is_none()).unwrap_or(false);
                    if picking {
                        let mode = self.ed.as_ref().map(|e| e.mode).unwrap_or(Mode::Area);
                        self.cancel();
                        // one frame later: the frozen screen must be gone from the new picture
                        slint::Timer::single_shot(std::time::Duration::from_millis(60), move || crate::with_app(|a| a.capture(mode, false)));
                    } else {
                        self.toast(i, if on { "Mouse pointer: in the next capture" } else { "Mouse pointer: off" });
                    }
                }
            }
        }
    }

    fn finished_pic(&self, i: usize) -> Option<Pic> {
        let _ = i;
        let p = self.ed.as_ref()?.flatten()?;
        if self.sess.as_ref().map(|s| s.background).unwrap_or(false) {
            return Some(crate::export::background(&p, self.cfg.num("background_pad").clamp(8, 400) as u32));
        }
        Some(p)
    }

    fn copy_pic(&self, pic: &Pic) {
        let fits = self.cfg.on("fits_discord");
        let enc = crate::export::encode(pic, "png", fits, true);
        crate::clipboard::set_image(enc.bytes, enc.mime);
    }

    /// copy | save | copysave | save_as | link | pin - all close the frozen screen (the capture stays restorable)
    fn output(&mut self, i: usize, what: &str) {
        let Some(pic) = self.finished_pic(i) else { return };
        let app = self.box_app();
        if let Some(s) = self.sess.as_mut() {
            s.app = app;
        }
        if what == "link" && !self.cfg.on("share_on") {
            self.toast(i, "Share links are off (Settings > Library & tools > Share links). Ctrl+C copies the picture.");
            return;
        }
        let pic = Arc::new(pic);
        let id = self.sess.as_ref().map(|s| s.id.clone()).unwrap_or_default();
        // used = kept in the library (unless Settings says nothing goes there)
        if self.cfg.get("library_keep") != "none" {
            if let Some(s) = self.sess.as_mut() {
                s.kept = true;
            }
        }
        let copy = what == "copy" || what == "copysave" || (self.cfg.on("always_copy") && what != "pin");
        if copy {
            self.copy_pic(&pic);
            // copied but not saved: the picture still goes to Echo's Screenshots ("Also show in Echo"); a save of
            // the same capture later replaces it there (echo_link: one file per capture)
            if !what.contains("save") {
                let stem = self.file_name(&self.sess.as_ref().map(|s| s.app.clone()).unwrap_or_default());
                crate::echo_link::put_pic(crate::echo_link::target(&self.cfg), &id, pic.clone(), stem, self.cfg.get("format").to_string(), self.cfg.on("fits_discord"));
            }
        }
        let mut note = if copy { "Copied".to_string() } else { String::new() };
        match what {
            "save" | "copysave" => {
                let enc = crate::export::encode(&pic, self.cfg.get("format"), self.cfg.on("fits_discord"), false);
                let path = self.save_target(enc.ext);
                match crate::export::write_new(&path, &enc.bytes) {
                    Ok(()) => {
                        let n = path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
                        note = if copy { format!("Copied + saved · {n}") } else { format!("Saved · {n}") };
                        if !enc.note.is_empty() {
                            note = format!("{note} · {}", enc.note);
                        }
                        if let Some(s) = self.sess.as_mut() {
                            s.saved.push(path.to_string_lossy().to_string());
                        }
                        crate::echo_link::mirror(crate::echo_link::target(&self.cfg), &id, &path);
                        crate::log::line("save", &format!("{} ({} KB)", n, enc.bytes.len() / 1024));
                    }
                    Err(e) => note = format!("Not saved: {e}"),
                }
            }
            "save_as" => {
                let p2 = pic.clone();
                let fmt = self.cfg.get("format").to_string();
                let fits = self.cfg.on("fits_discord");
                let name = self.file_name(&self.sess.as_ref().map(|s| s.app.clone()).unwrap_or_default());
                let dir = self.cfg.save_dir();
                let echo = crate::echo_link::target(&self.cfg);
                let id3 = id.clone();
                // the dialog starts with the format from the settings ("shot.jpg"); the user can type another kind
                let start_ext = match fmt.as_str() {
                    "jpg" | "jpeg" => "jpg",
                    "webp" => "webp",
                    _ => "png",
                };
                crate::save_dialog(dir, format!("{name}.{start_ext}"), move |path| {
                    // the kind follows the name the user typed (photo.jpg = JPEG); no known extension = the setting's
                    let typed = path.extension().map(|e| e.to_string_lossy().to_lowercase()).unwrap_or_default();
                    let fmt = if ["png", "jpg", "jpeg", "webp"].contains(&typed.as_str()) { typed } else { fmt };
                    let enc = crate::export::encode(&p2, &fmt, fits, false);
                    let path = crate::export::save_as_path(&path, enc.ext);
                    let r = crate::export::write_new(&path, &enc.bytes);
                    if r.is_ok() {
                        crate::echo_link::mirror(echo, &id3, &path);
                    }
                    crate::log::line("save", &format!("save as: {:?}", r.as_ref().map(|_| "ok")));
                });
                note = "Save as...".into();
            }
            "pin" => {
                // on the monitor the box's top-left corner is on, at the box's place there
                let (b, v) = self.ed.as_ref().map(|e| (e.boxr.unwrap_or_default(), e.view)).unwrap_or_default();
                let g = v.to_screen((b.x, b.y));
                let k = self.out_at((g.0 + 0.5, g.1 + 0.5)).unwrap_or_else(|| self.ui_out());
                let p = self.part_of(k).unwrap_or(Part { key: 0, x: 0.0, y: 0.0, w: 0.0, h: 0.0 });
                let sc = self.scale_of(k);
                let out = self.outs[k].info.key;
                self.pin(pic.clone(), Some(out), (((g.0 - p.x) / sc) as i32, ((g.1 - p.y) / sc) as i32), sc);
                note = String::new();
            }
            "link" => {
                crate::share::start(pic.clone(), id.clone(), &self.cfg);
                note = "Uploading for a link...".into();
            }
            _ => {}
        }
        self.close_keep_quiet();
        if copy || what.contains("save") {
            crate::sound::play(crate::sound::Snd::Done);
        }
        let saved = self.sess.as_ref().map(|s| s.saved.clone()).unwrap_or_default();
        self.last_out = Some((id.clone(), saved.clone(), std::time::Instant::now()));
        if !note.is_empty() && self.cfg.on("thumb_after_full") {
            self.show_thumb(&pic, &id, &note, saved);
        }
    }

    /// Where Save writes: a library edit of a saved picture replaces that file when it has the same kind (the original
    /// stays in the library); everything else gets a free new name with the real extension (never overwrites)
    fn save_target(&self, ext: &str) -> std::path::PathBuf {
        if let Some(s) = &self.sess {
            let src = std::path::Path::new(&s.source);
            let same_kind = src.extension().map(|e| e.to_string_lossy().to_lowercase()).map(|e| e == ext || (e == "jpeg" && ext == "jpg")).unwrap_or(false);
            if !s.source.is_empty() && !self.cfg.on("edits_as_copy") && same_kind {
                return src.to_path_buf();
            }
        }
        let app = self.sess.as_ref().map(|s| s.app.clone()).unwrap_or_default();
        let name = self.file_name(&app);
        crate::export::free_name(&self.cfg.save_dir(), &name, ext)
    }

    /// Closing after an output: kept for restore like Esc, but no first-time popup
    fn close_keep_quiet(&mut self) {
        self.settle_all();
        self.remember_box();
        self.remember_tool();
        self.hide_all();
        self.store_async(false);
    }

    fn ocr(&mut self, _i: usize, redact: bool) {
        let i = self.ui_out();
        let Some(pic) = self.ed.as_ref().and_then(|e| e.flatten()) else { return };
        let off = self.ed.as_ref().and_then(|e| e.boxr).map(|b| (b.x, b.y)).unwrap_or((0.0, 0.0));
        self.outs[i].comp.set_busy(true);
        self.outs[i].comp.set_busy_text(sh(if redact { "Looking for emails, numbers, keys..." } else { "Reading the text..." }));
        let key = self.outs[i].info.key;
        std::thread::spawn(move || {
            let res = crate::ocr::read(&pic);
            let _ = slint::invoke_from_event_loop(move || {
                crate::with_app(|a| {
                    let Some(i) = a.outs.iter().position(|o| o.info.key == key) else { return };
                    a.outs[i].comp.set_busy(false);
                    match res {
                        Err(e) => a.toast(i, &e),
                        Ok(words) => {
                            if redact {
                                let hits = crate::ocr::sensitive(&words);
                                let n = hits.len();
                                let rs: Vec<R> = hits.into_iter().map(|(r, _)| r.moved(off.0, off.1)).collect();
                                if let Some(e) = a.ed.as_mut() {
                                    e.add_blurs(&rs);
                                }
                                a.sync_all();
                                a.toast(i, &if n == 0 { "Nothing private found".to_string() } else { format!("Blurred {n} private thing(s) - Ctrl+Z to undo") });
                            } else {
                                let text = crate::ocr::text(&words);
                                if text.trim().is_empty() {
                                    a.toast(i, "No text found");
                                } else {
                                    crate::clipboard::set_text(&text);
                                    a.toast(i, &format!("Copied the text ({} words)", words.len()));
                                }
                            }
                        }
                    }
                })
            });
        });
    }

    fn paste(&mut self, i: usize) {
        let key = self.outs[i].info.key;
        #[cfg(target_os = "linux")]
        crate::log::line("paste", &format!("clipboard has: {:?}", crate::wl::clipboard_types()));
        crate::clipboard::read_image(move |res| {
            crate::with_app(|a| {
                let Some(i) = a.outs.iter().position(|o| o.info.key == key) else { return };
                match res {
                    Ok(p) => {
                        if let Some(e) = a.ed.as_mut() {
                            e.paste_image(p);
                        }
                        a.sync_all();
                    }
                    Err(e) => a.toast(i, &e),
                }
            })
        });
    }

    fn toast(&mut self, _i: usize, t: &str) {
        crate::log::line("toast", t);
        if let Some(o) = self.outs.get(self.ui_out()) {
            o.comp.set_toast(sh(t));
            let w = o.comp.as_weak();
            let ms = (900 + t.len() as u64 * 25).min(3000);
            self.toast_timer.start(slint::TimerMode::SingleShot, std::time::Duration::from_millis(ms), move || {
                if let Some(c) = w.upgrade() {
                    c.set_toast(sh(""));
                }
            });
        }
    }

    // ---------------------------------------------------------------------------------------------------------
    // The UI from the editor state

    /// Every monitor showing the capture, from the one editor: each shows its part of the picture; the bars go on
    /// the monitor with the most of the box. Pictures every monitor needs are made once here.
    pub fn sync_all(&mut self) {
        let ui = self.ui_out();
        let sc = self.scale_of(ui);
        let Some(e) = self.ed.as_mut() else { return };
        e.ui_scale = sc;
        let e = &*e;
        let src = Arc::as_ptr(&e.doc.base) as usize;
        if e.boxr.is_some() && self.dim_img.as_ref().map(|d| d.0) != Some(src) {
            // made once per picture when the first box appears (~ a few ms); moving the box then repaints only the
            // box, not the four dark strips around it = the whole screen (the user, 20 Nov 2026: "60 Hz")
            self.dim_img = Some((src, rgba_image(&darkened(&e.doc.base))));
        }
        // a shape being dragged: the layer holds the OTHER shapes (made once), the dragged one is a small picture of
        // its own - each move repaints only that (the user, 20 Nov 2026: "dragging the arrow is laggy" = the whole box
        // was drawn again on every move)
        let live = e.live_shape().zip(e.box_ir());
        let mut live_img = None;
        if let Some((idx, b)) = live {
            if idx > 0 && self.rest_img.as_ref().map(|r| r.0) != Some(idx) {
                let rest = crate::shapes::Doc { base: e.doc.base.clone(), shapes: e.doc.shapes[..idx].to_vec(), images: e.doc.images.clone() };
                self.rest_img = Some((idx, layer_image(&rest, b)));
            }
            let s = &e.doc.shapes[idx];
            let area = s.bbox().inflate(4.0).ir().intersect(&b);
            if area.w > 0 && area.h > 0 {
                let mut buf = SharedPixelBuffer::<Rgba8Pixel>::new(area.w as u32, area.h as u32);
                if let Some(mut pm) = tiny_skia::PixmapMut::from_bytes(buf.make_mut_bytes(), area.w as u32, area.h as u32) {
                    crate::shapes::render_one(&e.doc, s, area, &mut pm);
                }
                live_img = Some((Image::from_rgba8_premultiplied(buf), area));
            }
        } else {
            self.rest_img = None;
            if let Some(b) = e.box_ir().filter(|_| !e.doc.shapes.is_empty()) {
                if self.layer_img.as_ref().map(|l| l.0) != Some((src, e.version)) {
                    self.layer_img = Some(((src, e.version), layer_image(&e.doc, b)));
                }
            }
        }
        for i in 0..self.outs.len() {
            self.sync_out(i, i == ui, live.map(|l| l.0), live_img.as_ref());
        }
    }

    fn sync_out(&mut self, i: usize, is_ui: bool, live: Option<usize>, live_img: Option<&(Image, crate::shapes::IR)>) {
        let Some(p) = self.part_of(i) else { return };
        let sc = self.scale_of(i);
        let multi = self.parts.len() > 1;
        let Some(e) = self.ed.as_ref() else { return };
        let o = &mut self.outs[i];
        let c = &o.comp;
        let lg = |v: f32| v / sc;
        // this monitor's view: the editor's (joined screen) moved by where the monitor sits on it
        let v = View { ox: e.view.ox - p.x, oy: e.view.oy - p.y, s: e.view.s };
        let vis = R::new(0.0, 0.0, p.w, p.h);
        c.set_vx(lg(v.ox));
        c.set_vy(lg(v.oy));
        c.set_vw(lg(e.doc.base.w as f32 * v.s));
        c.set_vh(lg(e.doc.base.h as f32 * v.s));
        // 100 %: one picture pixel per screen pixel - nearest is the same picture as smooth and much faster to draw
        c.set_pixelated(v.s >= 2.0 || (v.s - 1.0).abs() < 1e-4);
        // the box only on another monitor: this one looks (and acts) like picking - a drag here makes a new box
        let box_here = e.boxr.filter(|b| (b.w >= 1.0 || e.phase == Phase::Pick) && (!multi || touches(&v.rect(b), &vis)));
        let picking = e.phase == Phase::Pick || (multi && box_here.is_none());
        c.set_picking(picking);
        c.set_dragging(e.dragging_box());
        c.set_bars_here(is_ui);
        let m = v.to_screen(e.mouse);
        c.set_mx(lg(m.0));
        c.set_my(lg(m.1));
        let mouse_in = e.mouse_in && (!multi || (m.0 >= 0.0 && m.1 >= 0.0 && m.0 < p.w && m.1 < p.h));
        c.set_mouse_in(mouse_in);
        c.set_mode(e.mode.index());
        match box_here {
            Some(b) => {
                let r = v.rect(&b);
                c.set_has_box(true);
                c.set_bx(lg(r.x));
                c.set_by(lg(r.y));
                c.set_bw(lg(r.w));
                c.set_bh(lg(r.h));
                // the bars go around the part of the box on this monitor
                let u = clip(&r, &vis);
                c.set_ubx(lg(u.x));
                c.set_uby(lg(u.y));
                c.set_ubw(lg(u.w));
                c.set_ubh(lg(u.h));
                // the box in picture px: the bright part drawn over the darkened copy
                c.set_pbx(b.x.round() as i32);
                c.set_pby(b.y.round() as i32);
                c.set_pbw((b.w.round() as i32).max(1));
                c.set_pbh((b.h.round() as i32).max(1));
                if let Some((src, img)) = self.dim_img.as_ref().filter(|d| d.0 != o.dim_src) {
                    c.set_bg_dim(img.clone());
                    c.set_dim_ready(true);
                    o.dim_src = *src;
                }
                c.set_size_label(sh(format!("{} × {}", b.w.round() as i32, b.h.round() as i32)));
            }
            None => c.set_has_box(false),
        }
        if let Some(w) = e.hover_win.filter(|w| e.phase == Phase::Pick && e.boxr.is_none() && (!multi || touches(&v.rect(w), &vis))) {
            let r = v.rect(&w);
            c.set_win_hover(true);
            c.set_wx(lg(r.x));
            c.set_wy(lg(r.y));
            c.set_ww(lg(r.w));
            c.set_wh(lg(r.h));
        } else {
            c.set_win_hover(false);
        }
        let mag_on = self.cfg.on("magnifier");
        c.set_show_mag(mag_on);
        if picking && mag_on && mouse_in {
            c.set_mag(magnifier(&e.doc.base, e.mouse));
            c.set_mag_label(sh(format!("{}, {}", e.mouse.0.round() as i32, e.mouse.1.round() as i32)));
        }
        // tool + options
        let t = e.tool;
        c.set_tool(self.tools_model.iter().position(|ti| ti.id.as_str() == t.id()).map(|p| p as i32).unwrap_or(-1));
        let sel_tool = if e.sel.len() == 1 { shape_tool(&e.doc.shapes[e.sel[0]].kind).unwrap_or(t) } else { t };
        c.set_tool_name(sh(if sel_tool != t { format!("{} (selected)", sel_tool.name()) } else { t.name().to_string() }));
        c.set_tool_key(sh(self.keymap.first(t.id())));
        let st = e.styles[&sel_tool];
        let (size, color_idx, fill, shadow) = if e.sel.len() == 1 {
            let s = &e.doc.shapes[e.sel[0]];
            // the lit colour dot: the shape's only when a new colour would change it, else what the next shape gets
            let idx = if e.colour_goes_to_sel() { PALETTE.iter().position(|p| *p == s.color).map(|p| p as i32).unwrap_or(-1) } else { st.color_idx };
            (s.size, idx, s.fill, s.shadow)
        } else {
            (st.size, st.color_idx, st.fill, st.shadow)
        };
        c.set_color_idx(color_idx);
        c.set_has_color(sel_tool.has_color());
        c.set_has_size(sel_tool.has_size());
        c.set_has_fill(sel_tool.has_fill());
        c.set_has_shadow(sel_tool.has_shadow());
        let (lo, hi, _) = sel_tool.size_range();
        c.set_size_lo(lo);
        c.set_size_hi(hi);
        c.set_size_v(size);
        c.set_size_text(sh(match sel_tool {
            Tool::Text => format!("{} px text", size as i32),
            Tool::Blur => format!("strength {}", size as i32),
            _ => format!("{} px", size as i32),
        }));
        c.set_fill(fill);
        c.set_shadow(shadow);
        c.set_blur_look(sh(if sel_tool == Tool::Blur { if e.pixelate { "pixelate · B switches" } else { "blur · B switches" } } else { "" }));
        c.set_can_undo(e.can_undo());
        c.set_can_redo(e.can_redo());
        c.set_timer_label(sh(match self.cfg.num("delay") {
            0 => "Timer".to_string(),
            n => format!("{n} s"),
        }));
        c.set_pointer_on(self.cfg.on("pointer"));
        c.set_link_on(true);
        let sheet = e.keys_sheet && is_ui;
        c.set_keys_sheet(sheet);
        c.set_bars_hidden(e.bars_hidden);
        c.set_only_box(e.only_box());
        if e.lib_view {
            c.set_zoom_text(sh(format!("{} %", (e.view.s * 100.0).round())));
            c.set_view_title(sh(self.sess.as_ref().map(view_title).unwrap_or_default()));
        }
        if sheet && c.get_key_rows().row_count() == 0 {
            c.set_key_rows(ModelRc::new(VecModel::from(key_sheet(&self.keymap))));
        }
        // selection
        if let Some((bb, hs)) = e.sel_frame() {
            let r = v.rect(&bb);
            c.set_sel_show(true);
            c.set_sx(lg(r.x));
            c.set_sy(lg(r.y));
            c.set_sw(lg(r.w));
            c.set_sh(lg(r.h));
            let pts: Vec<crate::Pt> = hs.iter().map(|p| v.to_screen(*p)).map(|p| crate::Pt { x: lg(p.0), y: lg(p.1) }).collect();
            set_handles(&o.handles, pts);
        } else {
            c.set_sel_show(false);
            set_handles(&o.handles, vec![]);
        }
        if let Some((x, y, h)) = e.caret() {
            let p = v.to_screen((x, y));
            c.set_caret_show(true);
            c.set_cx(lg(p.0));
            c.set_cy(lg(p.1));
            c.set_ch(lg(h * v.s));
        } else {
            c.set_caret_show(false);
        }
        let place_layer = |c: &Overlay, b: crate::shapes::IR| {
            let r = v.rect(&b.f());
            c.set_lx(lg(r.x));
            c.set_ly(lg(r.y));
            c.set_lw(lg(r.w));
            c.set_lh(lg(r.h));
        };
        if let (Some(idx), Some(b)) = (live, e.box_ir()) {
            if o.live_idx != Some(idx) {
                o.live_idx = Some(idx);
                o.layer_ver = u64::MAX;
                match self.rest_img.as_ref().filter(|_| idx > 0) {
                    Some((_, img)) => {
                        c.set_layer(img.clone());
                        place_layer(c, b);
                        c.set_show_layer(true);
                    }
                    None => c.set_show_layer(false),
                }
            }
            match live_img {
                Some((img, area)) => {
                    c.set_live(img.clone());
                    let r = v.rect(&area.f());
                    c.set_live_x(lg(r.x));
                    c.set_live_y(lg(r.y));
                    c.set_live_w(lg(r.w));
                    c.set_live_h(lg(r.h));
                    c.set_show_live(true);
                }
                None => c.set_show_live(false),
            }
            return;
        }
        if o.live_idx.take().is_some() {
            c.set_show_live(false);
            c.set_live(Image::default());
        }
        // the drawn layer: only while there are shapes
        if e.doc.shapes.is_empty() {
            c.set_show_layer(false);
        } else if o.layer_ver != e.version {
            o.layer_ver = e.version;
            match (e.box_ir(), self.layer_img.as_ref()) {
                (Some(b), Some((_, img))) => {
                    c.set_layer(img.clone());
                    place_layer(c, b);
                    c.set_show_layer(true);
                }
                _ => c.set_show_layer(false),
            }
        }
    }

    // ---------------------------------------------------------------------------------------------------------
    // Library edit / open a file

    pub fn edit_library(&mut self, id: &str) {
        match library::load(id) {
            Ok((base, st, imgs)) => {
                let mut ed = self.editor_for(base);
                ed.doc.images = imgs;
                ed.doc.shapes = st.shapes.clone();
                ed.boxr = st.boxr.or(Some(ed.pic_rect()));
                ed.phase = Phase::Edit;
                ed.mode = st.mode.unwrap_or(Mode::Area);
                // the picture alone, fitted; Select (a drag draws nothing); last time's undo steps
                ed.lib_view = true;
                ed.tool = Tool::Select;
                ed.view = ed.box_view();
                ed.set_history(st.history.clone());
                let d = library::dir(id);
                let opened = std::fs::read(d.join("state.json")).ok().map(|s| (s, std::fs::read(d.join("thumb.png")).ok()));
                self.open_editor(ed, Sess { id: id.to_string(), created: st.created, source: st.saved.last().cloned().unwrap_or(st.source), saved: st.saved, background: false, stored_out: Some(u32::MAX), fresh: false, kept: true, app: st.app, opened });
            }
            Err(e) => crate::notify("Lasso", &e),
        }
    }

    /// "Save as a copy" (the user, 22 Nov 2026: "no button to officially save a version of a picture"): the picture as
    /// it is now becomes a new library picture and editing goes on in it; the one that was opened goes back to how it
    /// was when it was opened (edits written to it meanwhile - by Copy, Save... - are undone there)
    fn save_copy(&mut self, i: usize) {
        let Some(sess) = self.sess.as_mut() else { return };
        let (old, opened) = (sess.id.clone(), sess.opened.take());
        sess.id = library::new_id();
        sess.created = crate::util::unix_now();
        sess.saved.clear();
        sess.stored_out = None;
        sess.fresh = false;
        sess.kept = true;
        if let Some((st, thumb)) = opened {
            library::queue(&old, move |id| {
                let d = library::dir(id);
                let _ = crate::export::write_new(&d.join("state.json"), &st);
                if let Some(t) = thumb {
                    let _ = crate::export::write_new(&d.join("thumb.png"), &t);
                }
            });
        }
        self.store_async(false);
        self.toast(i, "Saved as a new picture - the one you opened stays as it was");
    }

    pub fn open_file(&mut self, path: &str) {
        let pic = std::fs::read(path).map_err(|e| e.to_string()).and_then(|b| crate::export::decode(&b));
        match pic {
            Ok(p) => {
                let mut ed = self.editor_for(p);
                ed.boxr = Some(ed.pic_rect());
                ed.phase = Phase::Edit;
                ed.mode = Mode::Screen;
                ed.lib_view = true;
                ed.tool = Tool::Select;
                ed.view = ed.box_view();
                let created = crate::util::unix_now();
                let id = library::new_id();
                self.open_editor(ed, Sess { id: id.clone(), created, source: path.to_string(), fresh: true, ..Default::default() });
                self.store_async(true);
                // the file's own bytes stay in the library ("Back to original" gives exactly them back, EXIF and all)
                let src = path.to_string();
                library::queue(&id, move |id| library::keep_original(id, std::path::Path::new(&src)));
            }
            Err(e) => crate::notify("Lasso can't open this", &e),
        }
    }

    pub fn want_monitor(&mut self, name: &str) {
        self.want_out = (!name.is_empty()).then(|| name.to_string());
    }

    fn editor_for(&mut self, base: Pic) -> Editor {
        self.prepare();
        if let Some(name) = self.want_out.take() {
            match self.outs.iter().position(|o| o.info.name == name) {
                Some(i) => self.active = i,
                None => crate::log::line("app", &format!("monitor {name} not found - opened where the mouse last was")),
            }
        }
        let o = self.outs.get(self.active).or(self.outs.first());
        let screen = o.map(|o| (o.info.pw as f32, o.info.ph as f32)).unwrap_or((1920.0, 1080.0));
        let mut ed = Editor::new(Arc::new(base), screen, self.keymap.clone());
        if ed.doc.base.w as f32 != screen.0 || ed.doc.base.h as f32 != screen.1 {
            ed.view = View::fit(ed.doc.base.w as f32, ed.doc.base.h as f32, screen.0, screen.1, 90.0);
        }
        ed
    }

    fn open_editor(&mut self, ed: Editor, sess: Sess) {
        if self.showing {
            self.hide_all();
        }
        self.hide_popups();
        let i = self.active.min(self.outs.len().saturating_sub(1));
        if self.outs.is_empty() {
            return;
        }
        let o = &mut self.outs[i];
        o.comp.set_bg(rgba_image(&ed.doc.base));
        if ed.lib_view {
            o.comp.set_view_bg(view_backdrop(&ed.doc.base, ed.boxr));
        }
        let part = Part { key: o.info.key, x: 0.0, y: 0.0, w: o.info.pw as f32, h: o.info.ph as f32 };
        self.set_editor(ed, vec![part]);
        self.sess = Some(sess);
        self.sync_all();
        self.show_all();
    }

    // ---------------------------------------------------------------------------------------------------------
    // Popups: Esc restore (first time), thumbnail, pins

    fn hide_popups(&mut self) {
        if let Some((p, _)) = self.restore_popup.take() {
            let _ = p.hide();
        }
        if let Some(t) = self.thumb.take() {
            let _ = t.t.hide();
        }
    }

    fn popup_spec(w: i32, h: i32, kb: u8) -> PopupSpec {
        PopupSpec { w, h, kb }
    }

    fn show_restore_popup(&mut self) {
        let spec = Self::popup_spec(440, 76, 0);
        let Ok((p, id)) = crate::make_popup(spec, RestorePopup::new) else { return };
        crate::set_bars_look(&p.global::<crate::B>(), self.cfg.dark());
        p.on_restore(|| {
            crate::with_app(|a| {
                a.seen_hint();
                a.restore();
            })
        });
        p.on_dismiss(|| {
            crate::with_app(|a| {
                a.seen_hint();
                a.hide_popups();
            })
        });
        let _ = p.show();
        self.restore_popup = Some((p, id));
        let me = self.me.clone();
        self.thumb_timer.start(slint::TimerMode::SingleShot, std::time::Duration::from_secs(5), move || {
            if let Some(a) = me.upgrade() {
                let mut a = a.borrow_mut();
                a.seen_hint();
                if let Some((p, _)) = a.restore_popup.take() {
                    let _ = p.hide();
                }
            }
        });
    }

    /// First start ever: a small "Lasso is running" popup (where it lives, set up a key, capture now)
    pub fn first_run(&mut self) {
        if self.cfg.on("first_run_done") {
            return;
        }
        let Ok((h, _)) = crate::make_popup(Self::popup_spec(420, 176, 0), Hello::new) else { return };
        crate::set_bars_look(&h.global::<crate::B>(), self.cfg.dark());
        h.on_act(|what| {
            let what = what.to_string();
            crate::with_app(move |a| {
                let mut c = Config::load();
                c.set("first_run_done", "1");
                c.save();
                a.cfg.set("first_run_done", "1");
                if let Some(h) = a.hello.take() {
                    let _ = h.hide();
                }
                match what.as_str() {
                    "capture" => {
                        // one frame later, so the popup isn't on the picture
                        slint::Timer::single_shot(std::time::Duration::from_millis(120), || crate::with_app(|a| a.capture(Mode::Area, false)));
                    }
                    "keys" => a.open_window_page("--settings-window", 3),
                    _ => {}
                }
            })
        });
        let _ = h.show();
        self.hello = Some(h);
    }

    fn seen_hint(&mut self) {
        if !self.cfg.on("esc_hint_seen") {
            let mut c = Config::load();
            c.set("esc_hint_seen", "1");
            c.save();
            self.cfg.set("esc_hint_seen", "1");
        }
    }

    /// The corner thumbnail (quick access overlay, the user's list of 20 Nov 2026): drag the picture out into Discord /
    /// a browser / a folder, click = edit, copy / save / pin / delete. Goes after 6 s, but never while the mouse is on it
    /// or a drag runs.
    fn show_thumb(&mut self, pic: &Arc<Pic>, id: &str, note: &str, saved: Vec<String>) {
        if let Some(t) = self.thumb.take() {
            let _ = t.t.hide();
        }
        let th = crate::export::thumbnail(pic, 300);
        let w = 316;
        let h = (th.h as i32 * 300 / th.w.max(1) as i32).clamp(80, 220) + 58;
        let Ok((t, wid)) = crate::make_popup(Self::popup_spec(w, h, 0), Thumb::new) else { return };
        crate::set_bars_look(&t.global::<crate::B>(), self.cfg.dark());
        t.set_pic(rgba_image(&th));
        t.set_info(sh(format!("{} × {}", pic.w, pic.h)));
        t.set_note(sh(note));
        t.set_can_drag(crate::can_drag_out());
        t.set_del_key(sh(if self.cfg.on("hotkeys") { self.cfg.get("hk.delete") } else { "" }));
        let id2 = id.to_string();
        t.on_open(move || {
            let id = id2.clone();
            crate::with_app(move |a| {
                a.hide_popups();
                // not in the library (Settings: keep nothing): the capture still in memory comes back instead
                if !library::dir(&id).join("state.json").exists() && a.sess.as_ref().is_some_and(|s| s.id == id) {
                    a.restore();
                } else {
                    a.edit_library(&id);
                }
            })
        });
        t.on_act(move |what| {
            let what = what.to_string();
            crate::with_app(move |a| a.thumb_act(&what));
        });
        t.on_hover(|on| crate::with_app(move |a| a.thumb_hover(on)));
        t.on_drag_out(|| crate::with_app(|a| a.thumb_drag_out()));
        let _ = t.show();
        self.thumb = Some(ThumbWin { t, win: wid, id: id.to_string(), pic: pic.clone(), saved, hovered: false, dragging: false });
        self.thumb_countdown();
    }

    /// (Re)start the 6 s until the thumbnail goes; stopped while the mouse is on it or a drag runs
    fn thumb_countdown(&mut self) {
        let busy = self.thumb.as_ref().map(|t| t.hovered || t.dragging);
        match busy {
            None => {}
            Some(true) => self.thumb_timer.stop(),
            Some(false) => {
                let me = self.me.clone();
                self.thumb_timer.start(slint::TimerMode::SingleShot, std::time::Duration::from_secs(6), move || {
                    if let Some(a) = me.upgrade() {
                        let mut a = a.borrow_mut();
                        if a.thumb.as_ref().is_some_and(|t| !t.hovered && !t.dragging) {
                            if let Some(t) = a.thumb.take() {
                                let _ = t.t.hide();
                            }
                        }
                    }
                });
            }
        }
    }

    fn thumb_hover(&mut self, on: bool) {
        if let Some(t) = self.thumb.as_mut() {
            t.hovered = on;
        }
        self.thumb_countdown();
    }

    fn thumb_act(&mut self, what: &str) {
        let Some(t) = self.thumb.as_ref() else { return };
        let p = t.pic.clone();
        let tid = t.id.clone();
        match what {
            "copy" => {
                self.copy_pic(&p);
                self.set_thumb_note("Copied");
            }
            "save" => {
                let enc = crate::export::encode(&p, self.cfg.get("format"), self.cfg.on("fits_discord"), false);
                let name = crate::util::name_from_pattern(self.cfg.get("name_pattern"), "Lasso");
                let path = crate::export::free_name(&self.cfg.save_dir(), &name, enc.ext);
                let r = crate::export::write_new(&path, &enc.bytes);
                if r.is_ok() {
                    if let Some(t) = self.thumb.as_mut() {
                        t.saved.push(path.to_string_lossy().to_string());
                    }
                    crate::echo_link::mirror(crate::echo_link::target(&self.cfg), &tid, &path);
                }
                self.set_thumb_note(&match r {
                    Ok(()) => format!("Saved · {}", path.file_name().unwrap_or_default().to_string_lossy()),
                    Err(e) => e,
                });
            }
            "pin" => {
                self.hide_popups();
                self.pin(p, None, (80, 80), 1.0);
            }
            "delete" => self.thumb_delete(),
            _ => self.hide_popups(),
        }
    }

    /// Delete from the thumbnail: the capture leaves the library and its saved pictures go to the trash (like the
    /// Library's Delete). A shared capture is left alone: its link must be stopped first (the Library does that).
    fn thumb_delete(&mut self) {
        let Some(t) = self.thumb.as_ref() else { return };
        let (id, saved) = (t.id.clone(), t.saved.clone());
        self.delete_out(&id, &saved, false);
    }

    /// The "Delete the last capture" key (21 Nov 2026: "delete a screenshot without moving your mouse to the right"):
    /// the editor open = throw it away (like Shift+Esc); else the capture in the corner thumbnail, or the last one
    /// finished in the past 2 minutes. Older ones only in the Library, so a stray key press can't eat an old picture.
    pub fn delete_last(&mut self) {
        if self.showing {
            self.discard();
            return;
        }
        // the key is quiet when it works (21 Nov 2026: the user set it up for quick access - no "Moved to the trash")
        if let Some(t) = self.thumb.as_ref() {
            let (id, saved) = (t.id.clone(), t.saved.clone());
            self.delete_out(&id, &saved, true);
            return;
        }
        match self.last_out.clone() {
            Some((id, saved, at)) if at.elapsed() < std::time::Duration::from_secs(120) => self.delete_out(&id, &saved, true),
            // silent: the key may be one a game uses too (21 Nov 2026: "if there's nothing to delete, don't show it")
            _ => crate::log::line("thumb", "delete key: nothing recent to delete"),
        }
    }

    /// `by_key` = the delete key: no "Moved to the trash" popup (errors still say so)
    fn delete_out(&mut self, id: &str, saved: &[String], by_key: bool) {
        if crate::share::link_of(id).is_some() {
            self.set_thumb_note("Shared - delete it in the Library (stops the link)");
            if self.thumb.is_none() {
                crate::notify("Lasso", "Shared - delete it in the Library (stops the link)");
            }
            return;
        }
        let r = library::delete_capture(id, saved);
        self.forget(id);
        match r {
            Ok(_) => {
                crate::log::line("thumb", if by_key { "deleted with the delete key" } else { "deleted from the corner thumbnail" });
                self.last_out = None;
                self.hide_popups();
                if !by_key {
                    crate::notify("Lasso", if saved.is_empty() { "Moved to the trash" } else { "Moved to the trash (with its saved picture)" });
                }
            }
            Err(e) => {
                self.set_thumb_note(&format!("Not deleted: {e}"));
                if self.thumb.is_none() {
                    crate::notify("Lasso", &format!("Not deleted: {e}"));
                }
            }
        }
    }

    /// The picture leaves the thumbnail by drag and drop: the saved file if there is one, else a PNG in the drag cache
    fn thumb_drag_out(&mut self) {
        let Some(t) = self.thumb.as_ref() else { return };
        let (win, pic) = (t.win, t.pic.clone());
        let saved = t.saved.iter().rev().map(std::path::PathBuf::from).find(|p| p.exists());
        let png = Arc::new(crate::export::encode(&pic, "png", self.cfg.on("fits_discord"), true).bytes);
        let path = match saved {
            Some(p) => p,
            None => {
                let dir = crate::paths::drag_dir();
                clean_drag_dir(&dir);
                let name = crate::util::name_from_pattern(self.cfg.get("name_pattern"), "Lasso");
                let p = crate::export::free_name(&dir, &name, "png");
                if let Err(e) = crate::export::write_new(&p, &png) {
                    self.set_thumb_note(&format!("Can't drag: {e}"));
                    return;
                }
                p
            }
        };
        let icon = {
            let i = crate::export::thumbnail(&pic, 160);
            Some((i.w, i.h, i.px))
        };
        if let Some(t) = self.thumb.as_mut() {
            t.dragging = true;
        }
        self.thumb_countdown();
        crate::log::line("thumb", "drag out started");
        match crate::drag_out(win, &path, png, icon) {
            Ok(None) => {}
            Ok(Some(dropped)) => self.thumb_drag_done(dropped),
            Err(()) => {
                if let Some(t) = self.thumb.as_mut() {
                    t.dragging = false;
                }
                self.set_thumb_note("Dragging doesn't work here - use Copy");
                self.thumb_countdown();
            }
        }
    }

    /// The drag ended: dropped = the thumbnail's job is done (it goes soon), given up = it stays as before
    pub fn thumb_drag_done(&mut self, dropped: bool) {
        crate::log::line("thumb", if dropped { "drag out: dropped" } else { "drag out: given up" });
        let Some(t) = self.thumb.as_mut() else { return };
        t.dragging = false;
        if dropped {
            t.hovered = false;
            t.t.set_note(sh("Dropped"));
            let me = self.me.clone();
            self.thumb_timer.start(slint::TimerMode::SingleShot, std::time::Duration::from_millis(1200), move || {
                if let Some(a) = me.upgrade() {
                    if let Some(t) = a.borrow_mut().thumb.take() {
                        let _ = t.t.hide();
                    }
                }
            });
        } else {
            self.thumb_countdown();
        }
    }

    pub fn set_thumb_note(&mut self, note: &str) {
        if let Some(t) = &self.thumb {
            t.t.set_note(sh(note));
        }
    }

    /// A note for the corner thumbnail only if it still shows that capture (share.rs: "Uploading for a link..." ->
    /// "Link copied - ..." when the upload is done; a newer capture's thumbnail is left alone)
    pub fn thumb_note_for(&mut self, id: &str, note: &str) {
        if let Some(t) = self.thumb.as_ref().filter(|t| t.id == id) {
            t.t.set_note(sh(note));
        }
    }

    /// The full screenshot's background save finished: the thumbnail drags / deletes that file
    pub fn thumb_saved(&mut self, id: &str, path: String) {
        if let Some(l) = self.last_out.as_mut().filter(|l| l.0 == id) {
            l.1.push(path.clone());
        }
        if let Some(t) = self.thumb.as_mut().filter(|t| t.id == id) {
            t.saved.push(path);
        }
    }

    /// A picture floating on top (Snipaste). `pos` = logical px on that monitor.
    pub fn pin(&mut self, pic: Arc<Pic>, out: Option<u32>, pos: (i32, i32), scale: f32) {
        let (w, h) = (((pic.w as f32 / scale) as i32 + 8).max(40), ((pic.h as f32 / scale) as i32 + 8).max(40));
        let Ok((p, win)) = crate::make_pin(out, pos, (w, h), Pin::new) else { return };
        p.set_pic(rgba_image(&pic));
        p.set_zoom_text(sh("100 % · T: click-through · Esc closes"));
        let wk = win;
        p.on_drag_start(move |x, y| crate::with_app(move |a| a.pin_grab(wk, x, y)));
        p.on_drag(move |x, y| crate::with_app(move |a| a.pin_drag(wk, x, y)));
        p.on_wheel(move |d, ctrl| crate::with_app(move |a| a.pin_wheel(wk, d, ctrl)));
        p.on_close_pin(move || {
            crate::with_app(move |a| {
                if let Some(i) = a.pins.iter().position(|p| p.win == wk) {
                    a.close_pin(i);
                }
            })
        });
        let _ = p.show();
        self.pins.push(PinWin { comp: p, win, pic, zoom: 1.0 / scale, see: 1.0, pos, out, grab: (0.0, 0.0), through: false });
    }

    fn pin_grab(&mut self, win: u32, x: f32, y: f32) {
        if let Some(p) = self.pins.iter_mut().find(|p| p.win == win) {
            p.grab = (x, y);
        }
    }

    fn pin_drag(&mut self, win: u32, x: f32, y: f32) {
        if let Some(p) = self.pins.iter_mut().find(|p| p.win == win) {
            let (dx, dy) = ((x - p.grab.0) as i32, (y - p.grab.1) as i32);
            if dx == 0 && dy == 0 {
                return;
            }
            p.pos = (p.pos.0 + dx, p.pos.1 + dy);
            let (w, h) = ((p.pic.w as f32 * p.zoom) as i32 + 8, (p.pic.h as f32 * p.zoom) as i32 + 8);
            crate::move_pin(&p.comp, win, p.out, p.pos, (w, h));
        }
    }

    fn pin_wheel(&mut self, win: u32, d: f32, ctrl: bool) {
        // layer-shell pins: the wheel comes without modifiers, Lasso's own keyboard state says if Ctrl is down.
        // Normal windows (winit) carry Ctrl with the wheel - the stored state could be stale there (review D5).
        #[cfg(target_os = "linux")]
        let ctrl = ctrl || (crate::wl::active() && crate::MODS.with(|m| m.get().0));
        if let Some(p) = self.pins.iter_mut().find(|p| p.win == win) {
            if ctrl {
                p.see = (p.see + if d > 0.0 { 0.1 } else { -0.1 }).clamp(0.15, 1.0);
                p.comp.set_see(p.see);
                p.comp.set_zoom_text(sh(format!("{} % see-through", ((1.0 - p.see) * 100.0).round() as i32)));
            } else {
                p.zoom = (p.zoom * if d > 0.0 { 1.1 } else { 1.0 / 1.1 }).clamp(0.1, 8.0);
                let (w, h) = ((p.pic.w as f32 * p.zoom) as i32 + 8, (p.pic.h as f32 * p.zoom) as i32 + 8);
                crate::move_pin(&p.comp, win, p.out, p.pos, (w, h));
                p.comp.set_zoom_text(sh(format!("{} %", (p.zoom * 100.0).round() as i32)));
            }
        }
    }

    pub fn pins_clickable(&mut self) {
        for p in self.pins.iter_mut().filter(|p| p.through) {
            p.through = false;
            crate::pin_click_through(&p.comp, p.win, false);
        }
    }

    fn close_pin(&mut self, i: usize) {
        let p = self.pins.remove(i);
        let _ = p.comp.hide();
    }

    pub fn surface_closed(&mut self, win: u32) {
        if let Some(i) = self.pins.iter().position(|p| p.win == win) {
            self.pins.remove(i);
        }
        if let Some(o) = self.outs.iter_mut().find(|o| o.win == win) {
            o.shown = false;
        }
        if self.showing && self.outs.iter().all(|o| !o.shown) {
            self.showing = false;
        }
    }

    // ---------------------------------------------------------------------------------------------------------
    // Settings / Library windows (own processes, destroyed when closed)

    pub fn open_window_page(&mut self, arg: &str, page: i32) {
        std::env::set_var("LASSO_PAGE", page.to_string());
        self.open_window(arg);
        std::env::remove_var("LASSO_PAGE");
    }

    /// Quit closes Lasso's own windows too (Settings / Library are their own processes; the user, 20 Nov 2026: "closing
    /// it in the tray doesn't close the Lasso UI"). Both save every change at once, so ending them loses nothing.
    pub fn close_windows(&mut self) {
        for (_, mut c) in self.children.drain(..) {
            end_child(&mut c);
        }
    }

    pub fn open_window(&mut self, arg: &str) {
        self.children.retain_mut(|(_, c)| matches!(c.try_wait(), Ok(None)));
        // already open (21 Nov 2026: Settings <-> Library buttons): the old one goes and a fresh one opens in front -
        // a window of another process can't be raised on Wayland, and both save every change at once
        // Settings and Library are one window now (either argument): one at a time
        for (_, c) in self.children.iter_mut() {
            end_child(c);
        }
        self.children.clear();
        let exe = std::env::current_exe().unwrap_or_else(|_| "lasso".into());
        match std::process::Command::new(exe).arg(arg).spawn() {
            Ok(c) => self.children.push((arg.to_string(), c)),
            Err(e) => crate::log::line("app", &format!("{arg}: {e}")),
        }
    }

    pub fn copy_file(&mut self, path: &str) {
        match std::fs::read(path).map_err(|e| e.to_string()).and_then(|b| crate::export::decode(&b)) {
            Ok(p) => self.copy_pic(&p),
            Err(e) => crate::notify("Lasso", &e),
        }
    }

    pub fn copy_lib(&mut self, id: &str) {
        match library::finished(id) {
            Ok(p) => self.copy_pic(&p),
            Err(e) => crate::notify("Lasso", &e),
        }
    }

    pub fn pin_lib(&mut self, id: &str) {
        match library::finished(id) {
            Ok(p) => self.pin(Arc::new(p), None, (80, 80), 1.0),
            Err(e) => crate::notify("Lasso", &e),
        }
    }

    /// The Library's Share link for a capture: its finished picture, under its own id (sharing it again later -
    /// also from the frozen screen - updates the same link)
    pub fn share_lib(&mut self, id: &str) {
        match library::finished(id) {
            Ok(p) => crate::share::start(Arc::new(p), id.to_string(), &self.cfg),
            Err(e) => crate::notify("Lasso", &e),
        }
    }

    /// The Library's Share link for a picture from the save folder: under a hash of its path (share::file_key)
    pub fn share_file(&mut self, path: &str) {
        match std::fs::read(path).map_err(|e| e.to_string()).and_then(|b| crate::export::decode(&b)) {
            Ok(p) => crate::share::start(Arc::new(p), crate::share::file_key(std::path::Path::new(path)), &self.cfg),
            Err(e) => crate::notify("Lasso", &e),
        }
    }

    pub fn pin_file(&mut self, path: &str) {
        match std::fs::read(path).map_err(|e| e.to_string()).and_then(|b| crate::export::decode(&b)) {
            Ok(p) => self.pin(Arc::new(p), None, (80, 80), 1.0),
            Err(e) => crate::notify("Lasso", &e),
        }
    }

    /// Test hook: the editor of the frozen screen(s)
    pub fn test_editor(&mut self) -> Option<&mut Editor> {
        self.ed.as_mut()
    }
    pub fn test_sync(&mut self) {
        self.sync_all();
    }
    pub fn test_fx(&mut self, fx: Vec<Fx>) {
        let i = self.active;
        self.run_fx(i, fx);
    }
    pub fn showing(&self) -> bool {
        self.showing
    }
}

struct Scrolling {
    popup: ScrollPopup,
    stop: Arc<std::sync::atomic::AtomicBool>,
    cancel: Arc<std::sync::atomic::AtomicBool>,
}

impl App {
    /// Ctrl+Alt+S: the frozen screen goes, the box is grabbed again and again while the user scrolls the page under
    /// it, new rows are added below (scroll.rs). Done = the long picture opens on the frozen screen with every tool.
    fn start_scroll(&mut self, i: usize) {
        if self.scrolling.is_some() {
            return;
        }
        let Some(b) = self.ed.as_ref().and_then(|e| e.box_ir()) else { return };
        // grabbed again from one monitor: the box must be on one
        let Some((k, p)) = self.box_part() else {
            self.toast(i, "Scrolling capture works inside one screen");
            return;
        };
        let b = crate::shapes::IR { x: b.x - p.x as i32, y: b.y - p.y as i32, ..b };
        let target = crate::live_target(&self.outs[k].info);
        self.close_keep_quiet();
        let Ok((p, _)) = crate::make_popup(Self::popup_spec(430, 118, 0), ScrollPopup::new) else { return };
        crate::set_bars_look(&p.global::<crate::B>(), self.cfg.dark());
        p.set_status(sh("Scroll the page under the box slowly - Lasso adds what comes in."));
        p.on_act(|what| {
            let what = what.to_string();
            crate::with_app(move |a| a.scroll_act(&what));
        });
        let _ = p.show();
        let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let cancel = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let (st, ca) = (stop.clone(), cancel.clone());
        crate::log::line("scroll", &format!("scrolling capture of a {} x {} box", b.w, b.h));
        std::thread::spawn(move || {
            use std::sync::atomic::Ordering::Relaxed;
            // the frozen screen has to be gone from the screen first
            std::thread::sleep(std::time::Duration::from_millis(300));
            let mut s: Option<crate::scroll::Stitcher> = None;
            let mut logged = false;
            while !st.load(Relaxed) && !ca.load(Relaxed) {
                let g = crate::grab_live(&target);
                if let (Err(e), false) = (&g, logged) {
                    crate::log::line("scroll", &format!("grab: {e}"));
                    logged = true;
                }
                if let Ok(full) = g {
                    let f = full.crop(b);
                    let added = match s.as_mut() {
                        None => {
                            s = Some(crate::scroll::Stitcher::new(f));
                            1
                        }
                        Some(s) => s.add(f),
                    };
                    if added > 0 {
                        let h = s.as_ref().map(|s| s.pic.h).unwrap_or(0);
                        let _ = slint::invoke_from_event_loop(move || crate::with_app(|a| a.scroll_progress(h)));
                    }
                }
                std::thread::sleep(std::time::Duration::from_millis(120));
            }
            let pic = if ca.load(Relaxed) { None } else { s.map(|s| s.pic) };
            let _ = slint::invoke_from_event_loop(move || crate::with_app(move |a| a.scroll_done(pic)));
        });
        self.scrolling = Some(Scrolling { popup: p, stop, cancel });
    }

    fn scroll_progress(&mut self, h: u32) {
        if let Some(s) = &self.scrolling {
            s.popup.set_status(sh(format!("{h} px so far - keep scrolling, then Done")));
        }
    }

    fn scroll_act(&mut self, what: &str) {
        if let Some(s) = &self.scrolling {
            let flag = if what == "done" { &s.stop } else { &s.cancel };
            flag.store(true, std::sync::atomic::Ordering::Relaxed);
            s.popup.set_status(sh(if what == "done" { "Putting it together..." } else { "Cancelled" }));
        }
    }

    fn scroll_done(&mut self, pic: Option<Pic>) {
        if let Some(s) = self.scrolling.take() {
            let _ = s.popup.hide();
        }
        let Some(pic) = pic else { return };
        crate::log::line("scroll", &format!("long picture {} x {}", pic.w, pic.h));
        let mut ed = self.editor_for(pic);
        ed.boxr = Some(ed.pic_rect());
        ed.phase = Phase::Edit;
        ed.mode = Mode::Screen;
        let created = crate::util::unix_now();
        let kept = self.cfg.get("library_keep") == "all";
        self.open_editor(ed, Sess { id: library::new_id(), created, fresh: true, kept, ..Default::default() });
        self.store_async(true);
    }
}

pub struct PopupSpec {
    pub w: i32,
    pub h: i32,
    pub kb: u8,
}

/// Must screen.png be (re)written? Not stored yet, or stored from ANOTHER monitor than the box is on now (review B1:
/// the box on monitor 2 must not keep monitor 1's picture). Library edits (Some(u32::MAX)) keep theirs.
fn base_needs_write(stored: Option<u32>, now: Option<u32>) -> bool {
    stored != Some(u32::MAX) && stored != now
}

/// "org.kde.dolphin|Downloads - Dolphin" -> "Dolphin"; a full-screen window or a Steam game -> its title
/// ("ELDEN RING™" -> "ELDEN RING"): {app} in the file name (the notes: "names the file by the game")
pub fn app_name(class_caption: &str, fullscreen: bool) -> String {
    let (class, caption) = class_caption.split_once('|').unwrap_or((class_caption, ""));
    let clean = |s: &str| s.chars().filter(|c| c.is_alphanumeric() || " -_.'&".contains(*c)).collect::<String>().trim().to_string();
    let cap = clean(caption);
    if (fullscreen || class.starts_with("steam_app") || class.starts_with("gamescope")) && !cap.is_empty() && cap.chars().count() <= 48 {
        return cap;
    }
    let last = class.rsplit('.').next().unwrap_or(class);
    let mut ch = last.chars();
    match ch.next() {
        Some(f) => clean(&(f.to_uppercase().collect::<String>() + ch.as_str())),
        None => String::new(),
    }
}

impl App {
    /// The app / game the box is on: the top window under the box's middle
    fn box_app(&self) -> String {
        let Some(e) = self.ed.as_ref() else { return String::new() };
        let Some(b) = e.boxr else { return String::new() };
        let c = (b.x + b.w / 2.0, b.y + b.h / 2.0);
        e.windows.iter().position(|w| w.contains(c)).and_then(|k| e.window_names.get(k)).cloned().unwrap_or_default()
    }

    fn file_name(&self, app: &str) -> String {
        let app = if app.is_empty() { "Lasso" } else { app };
        crate::util::name_from_pattern(self.cfg.get("name_pattern"), app)
    }
}

fn shape_tool(k: &Kind) -> Option<Tool> {
    Some(match k {
        Kind::Pen { .. } => Tool::Pen,
        Kind::Highlight { .. } => Tool::Highlight,
        Kind::Line { .. } => Tool::Line,
        Kind::Arrow { .. } => Tool::Arrow,
        Kind::Rect { .. } => Tool::Rect,
        Kind::Ellipse { .. } => Tool::Ellipse,
        Kind::Text { .. } => Tool::Text,
        Kind::Step { .. } => Tool::Step,
        Kind::Blur { .. } => Tool::Blur,
        Kind::Spotlight { .. } => Tool::Spotlight,
        Kind::Image { .. } => return None,
    })
}

/// The ? sheet: every key in sections, spread over 3 columns (sections stay together)
fn key_sheet(km: &Keymap) -> Vec<crate::KeyRow> {
    let sections: [(&str, &[&str]); 5] = [
        ("Tools", &["select", "crop", "pen", "highlight", "line", "arrow", "rect", "ellipse", "text", "step", "blur", "spotlight", "picker"]),
        ("Colour, size, text", &["@colours", "thinner", "thicker", "take_colour", "bold", "italic"]),
        ("Shapes", &["delete", "select_all", "duplicate", "next_shape", "prev_shape", "front", "back", "undo", "redo", "paste"]),
        ("Finish", &["done", "copy", "copy_save", "copy_stay", "save", "save_as", "pin", "link", "copy_text", "redact", "background", "close", "discard"]),
        ("Box and view", &["mode", "mode_back", "whole_screen", "scroll", "last_box", "new_box", "hide_bars", "zoom_in", "zoom_out", "zoom_fit", "zoom_100", "open", "library", "keys"]),
    ];
    let mut groups: Vec<Vec<crate::KeyRow>> = vec![];
    for (title, ids) in sections {
        let mut g = vec![crate::KeyRow { key: sh(""), what: sh(title), head: true, col: 0 }];
        for id in ids {
            if *id == "@colours" {
                let ks: Vec<String> = (1..=8).map(|n| km.first(&format!("color{n}"))).collect();
                g.push(crate::KeyRow { key: sh(format!("{} … {}", ks[0], ks[7])), what: sh("Colours 1 - 8"), head: false, col: 0 });
                continue;
            }
            g.push(crate::KeyRow { key: sh(km.all(id)), what: sh(keys::action_name(id)), head: false, col: 0 });
        }
        groups.push(g);
    }
    groups.push(
        [("", "Mouse and arrows", true), ("Shift", "Straight / 45° lines, square, circle", false), ("Alt", "Draw from the centre", false), ("Space", "Hold while dragging: move the box", false), ("←↑→↓", "Move shape / box 1 px (Shift: 10)", false), ("Alt ←↑→↓", "Resize the box (bottom-right)", false), ("Ctrl Alt ←↑→↓", "Resize the box (top-left)", false), ("Ctrl wheel", "Zoom", false), ("wheel", "Size of the tool / shape", false)]
            .iter()
            .map(|(k, w, h)| crate::KeyRow { key: sh(*k), what: sh(*w), head: *h, col: 0 })
            .collect(),
    );
    // fill the columns in order, a new column when this one has its share
    let total: usize = groups.iter().map(|g| g.len()).sum();
    let per = total.div_ceil(3);
    let mut out = vec![];
    let (mut col, mut n) = (0, 0);
    for g in groups {
        if n > 0 && n + g.len() / 2 > per && col < 2 {
            col += 1;
            n = 0;
        }
        n += g.len();
        out.extend(g.into_iter().map(|mut r| {
            r.col = col;
            r
        }));
    }
    out
}

/// 11 x 11 picture pixels around the mouse, 12x bigger, with a pixel grid and the centre pixel marked.
/// Selection handles into the overlay's one model: same count = rows changed in place, nothing rebuilt
fn set_handles(m: &Rc<VecModel<crate::Pt>>, pts: Vec<crate::Pt>) {
    if m.row_count() == pts.len() {
        for (i, p) in pts.into_iter().enumerate() {
            if m.row_data(i).map(|o| o.x != p.x || o.y != p.y).unwrap_or(true) {
                m.set_row_data(i, p);
            }
        }
    } else if m.row_count() > 0 || !pts.is_empty() {
        m.set_vec(pts);
    }
}

/// The box content (picture + `doc`'s shapes) as the overlay's layer picture
fn layer_image(doc: &crate::shapes::Doc, b: crate::shapes::IR) -> Image {
    let mut buf = SharedPixelBuffer::<Rgba8Pixel>::new(b.w as u32, b.h as u32);
    if let Some(mut pm) = tiny_skia::PixmapMut::from_bytes(buf.make_mut_bytes(), b.w as u32, b.h as u32) {
        crate::shapes::render(doc, b, &mut pm);
    }
    Image::from_rgba8_premultiplied(buf)
}

/// screen.png holds every monitor of the capture joined (a box over several)
const SPAN_ALL: u32 = u32::MAX - 1;

/// Do two rects touch (shared edge counts: a box just being started is 0 x 0)?
/// Esc waits for its release: the press is kept (repeats ignored), the release hands back that press to act on.
/// Every other key goes through as it is. A release without a kept press (it went down before the frozen screen
/// came) does nothing.
fn esc_gate(kept: &mut Option<KeyEv>, ev: KeyEv, pressed: bool) -> Option<KeyEv> {
    if !ev.is("Escape") {
        return Some(ev);
    }
    if pressed {
        if kept.is_none() {
            *kept = Some(ev);
        }
        return None;
    }
    kept.take()
}

fn touches(a: &R, b: &R) -> bool {
    a.x <= b.r() && a.r() >= b.x && a.y <= b.b() && a.b() >= b.y
}

/// `a` cut to `b` (nothing in common: a 0-size rect at the nearest place)
fn clip(a: &R, b: &R) -> R {
    let x0 = a.x.clamp(b.x, b.r());
    let y0 = a.y.clamp(b.y, b.b());
    R::new(x0, y0, a.r().clamp(b.x, b.r()) - x0, a.b().clamp(b.y, b.b()) - y0)
}

/// How far a point is from a rect (0 inside)
fn rect_dist(r: &R, p: (f32, f32)) -> f32 {
    (r.x - p.0).max(p.0 - r.r()).max(0.0) + (r.y - p.1).max(p.1 - r.b()).max(0.0)
}

fn logical_rect(o: &OutInfo) -> R {
    R::new(o.x as f32, o.y as f32, o.w as f32, o.h as f32)
}

/// The desktop layout pointer positions are compared in: Linux = logical px (KDE's layout), Windows = physical px
/// (win.rs outputs: physical origins, monitors of mixed scale don't meet in a logical layout there)
fn desk_rect(o: &OutInfo) -> R {
    if cfg!(windows) {
        R::new(o.x as f32, o.y as f32, o.pw as f32, o.ph as f32)
    } else {
        logical_rect(o)
    }
}

/// A position on a monitor's frozen screen (logical px, `sc` = its scale) on the desktop layout
fn desk_point(o: &OutInfo, x: f32, y: f32, sc: f32) -> (f32, f32) {
    let k = if cfg!(windows) { sc } else { 1.0 };
    (o.x as f32 + x * k, o.y as f32 + y * k)
}

/// Physical px per desktop-layout px of a monitor with scale `sc`
fn desk_unit(sc: f32) -> f32 {
    if cfg!(windows) {
        1.0
    } else {
        sc
    }
}

/// A pointer position on monitor `from`'s frozen screen (logical px) -> the joined screen (physical px); `mons` =
/// (monitor, its part, its scale). Outside that monitor: through the desktop layout to the monitor it is over (the
/// nearest one between them), with that monitor's scale.
fn joined_point(mons: &[(OutInfo, Part, f32)], from: usize, x: f32, y: f32) -> (f32, f32) {
    let (o, p, sc) = &mons[from];
    let inside = x >= 0.0 && y >= 0.0 && x < o.w as f32 && y < o.h as f32;
    if inside || mons.len() < 2 {
        return (x * sc + p.x, y * sc + p.y);
    }
    let d = desk_point(o, x, y, *sc);
    let dist = |m: &(OutInfo, Part, f32)| rect_dist(&desk_rect(&m.0), d);
    let Some((o2, p2, sc2)) = mons.iter().min_by(|a, b| dist(a).total_cmp(&dist(b))) else { return (x * sc + p.x, y * sc + p.y) };
    let r = desk_rect(o2);
    let u = desk_unit(*sc2);
    ((d.0 - r.x) * u + p2.x, (d.1 - r.y) * u + p2.y)
}

/// Where each monitor's own pixels go in ONE picture of all of them (a box can go from one to the other), + its
/// size. No monitor is scaled: each one's frozen screen shows its part 1:1, a box on one monitor is exactly as
/// before. Linux: KDE's logical layout stretched piece by piece with the scale of the monitor(s) in each piece, so
/// neighbours still touch with mixed scale; Windows: the physical places as they are.
fn span_layout(infos: &[OutInfo]) -> (Vec<Part>, u32, u32) {
    let places: Vec<(f32, f32)> = if cfg!(windows) {
        let x0 = infos.iter().map(|o| o.x).min().unwrap_or(0);
        let y0 = infos.iter().map(|o| o.y).min().unwrap_or(0);
        infos.iter().map(|o| ((o.x - x0) as f32, (o.y - y0) as f32)).collect()
    } else {
        let ax = Axis::new(infos.iter().map(|o| (o.x, o.w, o.scale())).collect());
        let ay = Axis::new(infos.iter().map(|o| (o.y, o.h, o.scale())).collect());
        infos.iter().map(|o| (ax.at(o.x).round(), ay.at(o.y).round())).collect()
    };
    let parts: Vec<Part> = infos.iter().zip(places).map(|(o, (x, y))| Part { key: o.key, x, y, w: o.pw as f32, h: o.ph as f32 }).collect();
    let w = parts.iter().map(|p| p.x + p.w).fold(1.0f32, f32::max) as u32;
    let h = parts.iter().map(|p| p.y + p.h).fold(1.0f32, f32::max) as u32;
    (parts, w, h)
}

/// One axis of the logical layout -> physical px: each piece between monitor edges counts with the biggest scale
/// of the monitors covering it (none = 0: no gap in the picture)
struct Axis {
    segs: Vec<(i32, i32, f32)>,
}

impl Axis {
    fn new(items: Vec<(i32, i32, f32)>) -> Axis {
        let mut cuts: Vec<i32> = items.iter().flat_map(|&(s, l, _)| [s, s + l]).collect();
        cuts.sort_unstable();
        cuts.dedup();
        let segs = cuts.windows(2).map(|c| (c[0], c[1], items.iter().filter(|&&(s, l, _)| s <= c[0] && s + l >= c[1]).map(|t| t.2).fold(0.0f32, f32::max))).collect();
        Axis { segs }
    }
    fn at(&self, v: i32) -> f32 {
        self.segs.iter().map(|&(a, b, s)| (v.min(b) - a).max(0) as f32 * s).sum()
    }
}

/// The monitors' pictures joined at their `span_layout` places + the frozen screen's three copies of it (plain, a
/// lighter one while picking: x 166/255, a darker one outside the box: x 87/255), in row bands on every core. One
/// monitor: its own picture is the editor's (not copied).
fn stitch_parts(parts: &[Part], w: u32, h: u32, shots: Vec<(u32, Pic)>) -> (Pic, [SharedPixelBuffer<Rgba8Pixel>; 3]) {
    let mut shots = shots;
    let single = shots.len() == 1 && parts.first().map(|p| p.x == 0.0 && p.y == 0.0 && shots[0].1.w == w && shots[0].1.h == h).unwrap_or(false);
    let own = if single { shots.pop().map(|s| s.1) } else { None };
    let mut joined = if single { None } else { Some(Pic::new(w, h)) };
    let srcs: Vec<((u32, u32), &Pic)> = match own.as_ref() {
        Some(p) => vec![((0, 0), p)],
        None => shots.iter().filter_map(|(k, pic)| parts.iter().find(|p| p.key == *k).map(|p| ((p.x as u32, p.y as u32), pic))).collect(),
    };
    let mut bufs = [SharedPixelBuffer::<Rgba8Pixel>::new(w, h), SharedPixelBuffer::<Rgba8Pixel>::new(w, h), SharedPixelBuffer::<Rgba8Pixel>::new(w, h)];
    let n = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(4).clamp(1, 8) as u32;
    let band = h.div_ceil(n).max(1);
    let row = (w * 4) as usize;
    {
        let [b0, b1, b2] = &mut bufs;
        let (c0, c1, c2) = (b0.make_mut_bytes().chunks_mut(band as usize * row), b1.make_mut_bytes().chunks_mut(band as usize * row), b2.make_mut_bytes().chunks_mut(band as usize * row));
        let cj: Vec<Option<&mut [u8]>> = match joined.as_mut() {
            Some(j) => j.px.chunks_mut(band as usize * row).map(Some).collect(),
            None => (0..h.div_ceil(band)).map(|_| None).collect(),
        };
        let srcs = &srcs;
        std::thread::scope(|sc| {
            for (k, (((d0, d1), d2), dj)) in c0.zip(c1).zip(c2).zip(cj).enumerate() {
                sc.spawn(move || {
                    let y0 = k as u32 * band;
                    let rows = (d0.len() / row) as u32;
                    let mut dj = dj;
                    for &((ox, oy), s) in srcs {
                        let n = s.w.min(w.saturating_sub(ox)) as usize * 4;
                        for y in y0.max(oy)..(y0 + rows).min(oy + s.h) {
                            let si = ((y - oy) * s.w * 4) as usize;
                            let src = &s.px[si..si + n];
                            let di = ((y - y0) * w + ox) as usize * 4;
                            if let Some(j) = dj.as_mut() {
                                j[di..di + n].copy_from_slice(src);
                            }
                            d0[di..di + n].copy_from_slice(src);
                            for ((a, b), p) in d1[di..di + n].chunks_exact_mut(4).zip(d2[di..di + n].chunks_exact_mut(4)).zip(src.chunks_exact(4)) {
                                a[0] = ((p[0] as u16 * 166) / 255) as u8;
                                a[1] = ((p[1] as u16 * 166) / 255) as u8;
                                a[2] = ((p[2] as u16 * 166) / 255) as u8;
                                a[3] = p[3];
                                b[0] = ((p[0] as u16 * 87) / 255) as u8;
                                b[1] = ((p[1] as u16 * 87) / 255) as u8;
                                b[2] = ((p[2] as u16 * 87) / 255) as u8;
                                b[3] = p[3];
                            }
                        }
                    }
                });
            }
        });
    }
    let pic = own.or(joined).unwrap_or_else(|| Pic::new(w, h));
    (pic, bufs)
}

/// The frozen picture darkened like the dim outside the box (#000000a8 over it: x 87/255)
fn darkened(pic: &Pic) -> Pic {
    let mut px = pic.px.clone();
    for p in px.chunks_exact_mut(4) {
        p[0] = ((p[0] as u16 * 87) / 255) as u8;
        p[1] = ((p[1] as u16 * 87) / 255) as u8;
        p[2] = ((p[2] as u16 * 87) / 255) as u8;
    }
    Pic { w: pic.w, h: pic.h, px }
}

fn magnifier(pic: &Pic, at: (f32, f32)) -> Image {
    const N: i32 = 11;
    const Z: i32 = 12;
    let size = (N * Z) as u32;
    let mut buf = SharedPixelBuffer::<Rgba8Pixel>::new(size, size);
    let (cx, cy) = (at.0.floor() as i32, at.1.floor() as i32);
    let px = buf.make_mut_slice();
    for py in 0..N * Z {
        for pxx in 0..N * Z {
            let (sx, sy) = (cx - N / 2 + pxx / Z, cy - N / 2 + py / Z);
            let c = pic.at(sx, sy);
            let mut p = Rgba8Pixel { r: c[0], g: c[1], b: c[2], a: 255 };
            if c[3] == 0 {
                p = Rgba8Pixel { r: 20, g: 20, b: 24, a: 255 };
            }
            let grid = pxx % Z == 0 || py % Z == 0;
            let centre = pxx / Z == N / 2 || py / Z == N / 2;
            if grid {
                p = Rgba8Pixel { r: (p.r as u16 * 3 / 4) as u8, g: (p.g as u16 * 3 / 4) as u8, b: (p.b as u16 * 3 / 4) as u8, a: 255 };
            }
            if centre {
                // teal guide lines through the centre pixel
                p = Rgba8Pixel { r: ((p.r as u16 + 0x14) / 2) as u8, g: ((p.g as u16 + 0xa3) / 2) as u8, b: ((p.b as u16 + 0x8b) / 2) as u8, a: 255 };
            }
            px[(py * N * Z + pxx) as usize] = p;
        }
    }
    Image::from_rgba8(buf)
}

/// All monitors into one picture at their places (logical layout, each monitor's own pixel size scaled to the
/// biggest scale; gaps between monitors of different sizes stay transparent).
fn stitch(infos: &[OutInfo], shots: Vec<(u32, Pic)>) -> Pic {
    // Windows: physical places and sizes, no scaling (win.rs outputs: the origins are physical)
    #[cfg(windows)]
    let infos = &crate::win::physical_layout(infos);
    let s = infos.iter().map(|o| o.scale()).fold(1.0f32, f32::max);
    let x0 = infos.iter().map(|o| o.x).min().unwrap_or(0);
    let y0 = infos.iter().map(|o| o.y).min().unwrap_or(0);
    let x1 = infos.iter().map(|o| o.x + o.w).max().unwrap_or(1);
    let y1 = infos.iter().map(|o| o.y + o.h).max().unwrap_or(1);
    let (w, h) = (((x1 - x0) as f32 * s) as u32, ((y1 - y0) as f32 * s) as u32);
    let mut out = Pic::new(w.max(1), h.max(1));
    for (key, p) in shots {
        let Some(o) = infos.iter().find(|o| o.key == key) else { continue };
        let (dx, dy) = (((o.x - x0) as f32 * s) as u32, ((o.y - y0) as f32 * s) as u32);
        let (tw, th) = ((o.w as f32 * s) as u32, (o.h as f32 * s) as u32);
        let img = image::RgbaImage::from_raw(p.w, p.h, p.px).unwrap_or_default();
        let img = if img.width() != tw || img.height() != th { image::imageops::resize(&img, tw, th, image::imageops::FilterType::Lanczos3) } else { img };
        for y in 0..th.min(h.saturating_sub(dy)) {
            let s0 = (y * tw * 4) as usize;
            let d0 = (((dy + y) * w + dx) * 4) as usize;
            let n = (tw.min(w.saturating_sub(dx)) * 4) as usize;
            out.px[d0..d0 + n].copy_from_slice(&img.as_raw()[s0..s0 + n]);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pic(w: u32, h: u32, v: u8) -> Pic {
        let mut p = Pic::new(w, h);
        p.px.chunks_exact_mut(4).for_each(|c| c.copy_from_slice(&[v, v, v, 255]));
        p
    }

    #[test]
    fn base_picture_follows_the_box_monitor() {
        // B1: stored at capture time from monitor 1, the box ends up on monitor 2 -> rewrite
        assert!(base_needs_write(None, Some(1)));
        assert!(!base_needs_write(Some(1), Some(1)));
        assert!(base_needs_write(Some(1), Some(2)));
        // a library edit keeps its own picture
        assert!(!base_needs_write(Some(u32::MAX), Some(2)));
    }

    #[test]
    fn app_names() {
        assert_eq!(app_name("org.kde.dolphin|Downloads - Dolphin", false), "Dolphin");
        assert_eq!(app_name("steam_app_1245620|ELDEN RING™", false), "ELDEN RING");
        assert_eq!(app_name("firefox|Shop - Mozilla Firefox", true), "Shop - Mozilla Firefox");
        assert_eq!(app_name("", false), "");
    }

    #[test]
    fn esc_acts_when_it_comes_up() {
        let esc = |shift: bool, repeat: bool| KeyEv { key: "Escape".into(), shift, repeat, ..Default::default() };
        let mut kept = None;
        assert!(esc_gate(&mut kept, esc(true, false), true).is_none(), "Shift+Esc down: nothing yet");
        assert!(esc_gate(&mut kept, esc(false, true), true).is_none(), "held (repeat): nothing");
        // Shift may come up first: the release acts as the press was (Shift+Esc = throw away)
        let got = esc_gate(&mut kept, esc(false, false), false).expect("acts on release");
        assert!(got.shift && !got.repeat);
        assert!(kept.is_none());
        // a release whose press came before the frozen screen: nothing
        assert!(esc_gate(&mut kept, esc(false, false), false).is_none());
        // other keys: straight through, press and release
        let a = KeyEv { key: "a".into(), ..Default::default() };
        assert!(esc_gate(&mut kept, a.clone(), true).is_some());
        assert!(esc_gate(&mut kept, a, false).is_some());
    }

    #[test]
    fn spanning_layout_mixed_scale() {
        // the user's kind of desk: 1920x1080 at 1.0 on the left, 2560x1440 at 1.25 (logical 2048x1152) on the right
        let a = OutInfo { key: 1, name: "A".into(), x: 0, y: 0, w: 1920, h: 1080, pw: 1920, ph: 1080 };
        let b = OutInfo { key: 2, name: "B".into(), x: 1920, y: 0, w: 2048, h: 1152, pw: 2560, ph: 1440 };
        #[cfg(windows)]
        let b = OutInfo { w: 2048, ..b };
        let (parts, w, h) = span_layout(&[a.clone(), b.clone()]);
        // each monitor at its own pixels, side by side, no gap, nothing scaled
        assert_eq!((w, h), (4480, 1440));
        assert_eq!((parts[0].x, parts[0].y, parts[0].w), (0.0, 0.0, 1920.0));
        assert_eq!((parts[1].x, parts[1].y, parts[1].w, parts[1].h), (1920.0, 0.0, 2560.0, 1440.0));
        // the joined picture: A's pixels at 0, B's from 1920, below A transparent; the dim copies
        let (j, [plain, light, dark]) = stitch_parts(&parts, w, h, vec![(1, pic(1920, 1080, 100)), (2, pic(2560, 1440, 200))]);
        assert_eq!((j.w, j.h), (4480, 1440));
        assert_eq!(j.at(1919, 500)[0], 100);
        assert_eq!(j.at(1920, 500)[0], 200);
        assert_eq!(j.at(4479, 1439)[0], 200);
        assert_eq!(j.at(100, 1300), [0, 0, 0, 0]);
        let px = |b: &SharedPixelBuffer<Rgba8Pixel>, x: u32, y: u32| b.as_slice()[(y * w + x) as usize];
        assert_eq!(px(&plain, 1920, 7).r, 200);
        assert_eq!(px(&light, 1920, 7).r, (200u16 * 166 / 255) as u8);
        assert_eq!(px(&dark, 10, 1000).r, (100u16 * 87 / 255) as u8);
        assert_eq!(px(&dark, 10, 1300).a, 0);
        // a drag from A that went on to B: Wayland keeps sending it to A's surface (logical px of A, beyond its edge)
        let mons = vec![(a.clone(), parts[0], 1.0), (b.clone(), parts[1], 1.25)];
        assert_eq!(joined_point(&mons, 0, 100.0, 200.0), (100.0, 200.0));
        #[cfg(not(windows))]
        {
            // 100 logical px into B = 125 of B's pixels
            assert_eq!(joined_point(&mons, 0, 2020.0, 200.0), (1920.0 + 125.0, 250.0));
            // and back from B onto A
            assert_eq!(joined_point(&mons, 1, -20.0, 100.0), (1900.0, 100.0));
            // on B, inside: its own scale
            assert_eq!(joined_point(&mons, 1, 100.0, 100.0), (1920.0 + 125.0, 125.0));
        }
        // one monitor: its own picture, not copied
        let one = pic(800, 600, 50);
        let ptr = one.px.as_ptr();
        let (parts, w, h) = span_layout(&[OutInfo { key: 7, w: 800, h: 600, pw: 800, ph: 600, ..Default::default() }]);
        let (p1, _) = stitch_parts(&parts, w, h, vec![(7, one)]);
        assert_eq!(p1.px.as_ptr(), ptr);
    }

    #[test]
    #[cfg(not(windows))]
    fn spanning_layout_stacked_and_left() {
        // B (scale 1.25) above A, and a monitor left of x 0: they still touch, none overlaps
        let a = OutInfo { key: 1, x: 0, y: 1152, w: 1920, h: 1080, pw: 1920, ph: 1080, ..Default::default() };
        let b = OutInfo { key: 2, x: 0, y: 0, w: 2048, h: 1152, pw: 2560, ph: 1440, ..Default::default() };
        let c = OutInfo { key: 3, x: -1280, y: 1152, w: 1280, h: 1024, pw: 1280, ph: 1024, ..Default::default() };
        let (parts, w, h) = span_layout(&[a, b, c]);
        let p = |k: u32| *parts.iter().find(|p| p.key == k).unwrap();
        assert_eq!((p(3).x, p(3).y), (0.0, 1440.0));
        assert_eq!((p(2).x, p(2).y), (1280.0, 0.0));
        assert_eq!((p(1).x, p(1).y), (1280.0, 1440.0));
        assert_eq!((w, h), (1280 + 2560, 1440 + 1080));
        for x in &parts {
            for y in &parts {
                if x.key != y.key {
                    assert!(x.x + x.w <= y.x || y.x + y.w <= x.x || x.y + x.h <= y.y || y.y + y.h <= x.y, "{x:?} {y:?}");
                }
            }
        }
    }

    #[test]
        fn stitch_two_monitors_mixed_scale() {
        // a 1920x1080 monitor at 1.0 on the left, a 2560x1440 one at 1.25 (logical 2048x1152) on the right
        let a = OutInfo { key: 1, name: "A".into(), x: 0, y: 0, w: 1920, h: 1080, pw: 1920, ph: 1080 };
        let b = OutInfo { key: 2, name: "B".into(), x: 1920, y: 0, w: 2048, h: 1152, pw: 2560, ph: 1440 };
        let out = stitch(&[a, b], vec![(1, pic(1920, 1080, 10)), (2, pic(2560, 1440, 200))]);
        // Linux: the biggest scale for everything: (1920 + 2048) * 1.25 wide, 1152 * 1.25 high
        #[cfg(not(windows))]
        assert_eq!((out.w, out.h), (4960, 1440));
        // Windows: physical places and sizes, no scaling (win::physical_layout): 1920 + 2560 wide
        #[cfg(windows)]
        assert_eq!((out.w, out.h), (4480, 1440));
        assert_eq!(out.at(100, 100)[0], 10);
        assert_eq!(out.at(4000, 100)[0], 200);
        // below the smaller left monitor: a transparent gap, no black bar
        assert_eq!(out.at(100, 1400)[3], 0);
    }
}

/// The drag cache keeps only today's pictures (a drop target may read the file a bit later, so never right away)
fn clean_drag_dir(dir: &std::path::Path) {
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    for e in rd.flatten() {
        let old = e.metadata().and_then(|m| m.modified()).ok().and_then(|t| t.elapsed().ok()).is_some_and(|age| age.as_secs() > 24 * 3600);
        if old {
            let _ = std::fs::remove_file(e.path());
        }
    }
}

/// Ends a Settings / Library window process (SIGTERM on Linux) and waits for it
fn end_child(c: &mut std::process::Child) {
    if matches!(c.try_wait(), Ok(None)) {
        #[cfg(unix)]
        unsafe {
            libc::kill(c.id() as i32, libc::SIGTERM);
        }
        #[cfg(windows)]
        let _ = c.kill();
        let _ = c.wait();
    }
}
