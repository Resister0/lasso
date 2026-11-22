//! Lasso's own Slint platform for KDE Plasma / wlroots Wayland: every Lasso surface (frozen screen, pins, corner
//! popups) is a `zwlr_layer_shell_v1` surface drawn by Slint's SOFTWARE renderer into shared-memory buffers.
//!
//! Why not a normal (winit) window: KWin keeps a new window without an activation token behind the focused one
//! (focus-stealing prevention), so the keys would go to the app below - exactly Flameshot's Wayland bug and the
//! reason "Ctrl+B didn't work" in other tools. A layer surface with EXCLUSIVE keyboard always gets every key.
//! Keys come straight from xkb here (key names, no Ctrl control characters) and go to the app's key hook, not
//! through Slint. Pointer events go through Slint (the bars' buttons), the frozen canvas forwards them to Rust.
//!
//! Speed (the fast freeze): the Slint components and their pixel buffers are made at start and kept; showing =
//! one layer surface + one configure round trip (well under a millisecond on the local socket) + one render.
//! Like Aimdot, a surface is made new each time it is shown (re-mapping an old layer surface was unreliable on
//! Plasma 6.7) and destroyed when hidden, so KWin has nothing extra to composite while Lasso sits in the tray.
//!
//! Also here (they need the same Wayland connection): the clipboard through wlr data-control (no focus needed,
//! lives as long as Lasso runs - Lasso stays in the tray anyway), pasting a picture, the cursor shape.

use crate::keys::{evdev_us, KeyEv};
use slint::platform::software_renderer::{PremultipliedRgbaColor, RepaintBufferType, SoftwareRenderer, TargetPixel};
use slint::platform::{EventLoopProxy, Platform, PointerEventButton, WindowAdapter, WindowEvent};
use slint::{LogicalPosition, PhysicalSize, PlatformError};
use std::cell::{Cell, RefCell};
use std::collections::VecDeque;
use std::io::{Read, Write};
use std::os::fd::{AsFd, AsRawFd, FromRawFd, OwnedFd};
use std::os::unix::net::UnixStream;
use std::rc::{Rc, Weak};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use wayland_client::globals::{registry_queue_init, GlobalListContents};
use wayland_client::protocol::{
    wl_buffer::{self, WlBuffer},
    wl_callback::{self, WlCallback},
    wl_compositor::WlCompositor,
    wl_data_device::{self, WlDataDevice},
    wl_data_device_manager::{self, WlDataDeviceManager},
    wl_data_offer::WlDataOffer,
    wl_data_source::{self, WlDataSource},
    wl_keyboard::{self, WlKeyboard},
    wl_output::{self, WlOutput},
    wl_pointer::{self, WlPointer},
    wl_region::WlRegion,
    wl_registry::{self, WlRegistry},
    wl_seat::{self, WlSeat},
    wl_shm::{self, WlShm},
    wl_shm_pool::WlShmPool,
    wl_surface::{self, WlSurface},
};
use wayland_client::{delegate_noop, event_created_child, Connection, Dispatch, EventQueue, Proxy, QueueHandle, WEnum};
use wayland_protocols::wp::cursor_shape::v1::client::{
    wp_cursor_shape_device_v1::{Shape as CShape, WpCursorShapeDeviceV1},
    wp_cursor_shape_manager_v1::WpCursorShapeManagerV1,
};
use wayland_protocols::wp::fractional_scale::v1::client::{
    wp_fractional_scale_manager_v1::WpFractionalScaleManagerV1,
    wp_fractional_scale_v1::{self, WpFractionalScaleV1},
};
use wayland_protocols::wp::viewporter::client::{wp_viewport::WpViewport, wp_viewporter::WpViewporter};
use wayland_protocols::xdg::xdg_output::zv1::client::{
    zxdg_output_manager_v1::ZxdgOutputManagerV1,
    zxdg_output_v1::{self, ZxdgOutputV1},
};
use wayland_protocols::ext::data_control::v1::client::{
    ext_data_control_device_v1::{self, ExtDataControlDeviceV1},
    ext_data_control_manager_v1::ExtDataControlManagerV1,
    ext_data_control_offer_v1::{self, ExtDataControlOfferV1},
    ext_data_control_source_v1::{self, ExtDataControlSourceV1},
};
use wayland_protocols_wlr::data_control::v1::client::{
    zwlr_data_control_device_v1::{self, ZwlrDataControlDeviceV1},
    zwlr_data_control_manager_v1::ZwlrDataControlManagerV1,
    zwlr_data_control_offer_v1::{self, ZwlrDataControlOfferV1},
    zwlr_data_control_source_v1::{self, ZwlrDataControlSourceV1},
};
use wayland_protocols_wlr::layer_shell::v1::client::{
    zwlr_layer_shell_v1::{Layer, ZwlrLayerShellV1},
    zwlr_layer_surface_v1::{self, Anchor, KeyboardInteractivity, ZwlrLayerSurfaceV1},
};
use xkbcommon::xkb;

// -------------------------------------------------------------------------------------------------------------
// Public types

#[derive(Clone, Debug, PartialEq)]
pub enum Place {
    /// Whole monitor (the frozen screen). `output` = wl_output global name.
    Fill { output: u32 },
    /// Fixed size in a corner: bottom-right with a margin (popups, thumbnail)
    Corner { output: Option<u32>, w: i32, h: i32, margin: i32 },
    /// Free spot (pins): top-left margins on that monitor, size in logical px
    At { output: Option<u32>, x: i32, y: i32, w: i32, h: i32 },
}

#[derive(Clone, Debug, PartialEq)]
pub struct Spec {
    pub place: Place,
    /// 0 none, 1 exclusive, 2 on demand
    pub keyboard: u8,
    pub namespace: &'static str,
}

#[derive(Clone, Debug, Default)]
pub struct OutputInfo {
    pub reg: u32,
    pub name: String,
    /// logical position / size (xdg-output)
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
    /// physical size (current mode, as the screen is turned)
    pub pw: i32,
    pub ph: i32,
    pub scale: i32,
}

/// Input for the app's hooks.
pub enum Input {
    Key { win: u32, ev: KeyEv, pressed: bool },
    Mods { ctrl: bool, shift: bool, alt: bool },
    PointerOn { win: u32 },
    /// A surface got its size / scale (the app lays out again with the real scale)
    Configured { win: u32 },
    /// A surface was closed by the compositor (monitor unplugged, ...)
    Closed { win: u32 },
    /// A drag out of Lasso ended: dropped somewhere that took it, or given up
    DragDone { dropped: bool },
}

thread_local! {
    static NEXT_SPEC: RefCell<Option<Spec>> = const { RefCell::new(None) };
    static LAST_ID: Cell<u32> = const { Cell::new(0) };
    static INNER: RefCell<Option<Rc<Inner>>> = const { RefCell::new(None) };
    static HOOK: RefCell<Option<Box<dyn FnMut(Input)>>> = const { RefCell::new(None) };
}

/// Make a Slint component on a layer surface of this kind: `let (c, id) = wl::create(spec, || Overlay::new())`
pub fn create<T>(spec: Spec, f: impl FnOnce() -> T) -> (T, u32) {
    NEXT_SPEC.with(|s| *s.borrow_mut() = Some(spec));
    let c = f();
    NEXT_SPEC.with(|s| *s.borrow_mut() = None);
    (c, LAST_ID.with(|l| l.get()))
}

pub fn set_hook(f: impl FnMut(Input) + 'static) {
    HOOK.with(|h| *h.borrow_mut() = Some(Box::new(f)));
}

fn with_inner<T>(f: impl FnOnce(&Rc<Inner>) -> T) -> Option<T> {
    INNER.with(|i| i.borrow().as_ref().map(f))
}

pub fn active() -> bool {
    with_inner(|_| ()).is_some()
}

pub fn outputs() -> Vec<OutputInfo> {
    with_inner(|i| i.st.borrow().outputs.iter().filter(|o| o.done).map(|o| o.info.clone()).collect()).unwrap_or_default()
}

/// Change where / how big a shown surface is (pins move and zoom).
pub fn set_place(win: u32, place: Place) {
    with_inner(|i| {
        let mut wins = i.wins.borrow_mut();
        if let Some(w) = wins.iter_mut().find(|w| w.id == win) {
            if let Some(a) = w.adapter.upgrade() {
                a.spec.borrow_mut().place = place.clone();
            }
        }
        drop(wins);
        i.cmds.borrow_mut().push_back(Cmd::Replace(win));
    });
}

/// Click-through on / off (a pin over a game): an empty input region = every click goes to what's below
pub fn set_click_through(win: u32, on: bool) {
    with_inner(|i| {
        let st = i.st.borrow();
        if let Some(s) = st.surfs.iter().find(|s| s.win == win) {
            if on {
                let r = st.compositor.create_region(&i.qh, ());
                s.wl.set_input_region(Some(&r));
                r.destroy();
            } else {
                s.wl.set_input_region(None);
            }
            s.wl.commit();
        }
        drop(st);
        let _ = i.conn.flush();
    });
}

pub fn set_cursor(win: u32, shape: &str) {
    with_inner(|i| {
        let mut st = i.st.borrow_mut();
        let s = match shape {
            "crosshair" => CShape::Crosshair,
            "move" => CShape::Move,
            "pointer" => CShape::Pointer,
            "text" => CShape::Text,
            "nwse" => CShape::NwseResize,
            "nesw" => CShape::NeswResize,
            "ns" => CShape::NsResize,
            "ew" => CShape::EwResize,
            "grab" => CShape::Grab,
            "grabbing" => CShape::Grabbing,
            _ => CShape::Default,
        };
        if st.cursor_want.get(&win) != Some(&s) {
            st.cursor_want.insert(win, s);
            st.apply_cursor();
        }
    });
}

/// Put data on the clipboard (mime -> bytes). false = no data-control here (GNOME): use the fallback.
pub fn set_clipboard(items: Vec<(String, Arc<Vec<u8>>)>) -> bool {
    with_inner(|i| {
        let mut st = i.st.borrow_mut();
        let src = if let (Some(m), Some(dev)) = (st.ext_mgr.clone(), st.ext_dev.clone()) {
            let src = m.create_data_source(&i.qh, ());
            for (m, _) in &items {
                src.offer(m.clone());
            }
            dev.set_selection(Some(&src));
            DcSrc::Ext(src)
        } else if let (Some(m), Some(dev)) = (st.dc_mgr.clone(), st.dc_dev.clone()) {
            let src = m.create_data_source(&i.qh, ());
            for (m, _) in &items {
                src.offer(m.clone());
            }
            dev.set_selection(Some(&src));
            DcSrc::Wlr(src)
        } else {
            return false;
        };
        if let Some(old) = st.dc_src.replace((src, items)) {
            old.0.destroy();
        }
        drop(st);
        let _ = i.conn.flush();
        true
    })
    .unwrap_or(false)
}

/// What's on the clipboard: (mime types). Read one with `read_clipboard`.
/// Asks KWin fresh (a new data-control device, like wl-paste does): in the headless KWin 6.7 test a long-lived
/// device never got selection updates (13 Nov 2026), a new one gets the current selection at once.
pub fn clipboard_types() -> Vec<String> {
    with_inner(|i| {
        {
            let mut st = i.st.borrow_mut();
            if let (Some(m), Some(seat)) = (st.ext_mgr.clone(), st.seat.clone()) {
                let dev = m.get_data_device(&seat, &i.qh, ());
                if let Some(old) = st.ext_dev.replace(dev) {
                    old.destroy();
                }
                st.dc_sel = None;
            } else if let (Some(m), Some(seat)) = (st.dc_mgr.clone(), st.seat.clone()) {
                let dev = m.get_data_device(&seat, &i.qh, ());
                if let Some(old) = st.dc_dev.replace(dev) {
                    old.destroy();
                }
                st.dc_sel = None;
            }
        }
        for _ in 0..2 {
            let mut q = i.queue.borrow_mut();
            let mut st = i.st.borrow_mut();
            let _ = q.roundtrip(&mut st);
        }
        let st = i.st.borrow();
        st.dc_sel.as_ref().and_then(|o| st.offers.iter().find(|x| &x.0 == o).map(|x| x.1.clone())).unwrap_or_default()
    })
    .unwrap_or_default()
}

/// Reads the clipboard in that mime type; `done` runs on the event loop with the bytes (empty = failed).
pub fn read_clipboard(mime: &str, done: impl FnOnce(Vec<u8>) + Send + 'static) {
    let ok = with_inner(|i| {
        let st = i.st.borrow();
        let Some(offer) = st.dc_sel.clone() else { return false };
        let mut fds = [0i32; 2];
        if unsafe { libc::pipe2(fds.as_mut_ptr(), libc::O_CLOEXEC) } != 0 {
            return false;
        }
        let (r, w) = unsafe { (OwnedFd::from_raw_fd(fds[0]), OwnedFd::from_raw_fd(fds[1])) };
        offer.receive(mime.to_string(), w.as_fd());
        drop(w);
        drop(st);
        let _ = i.conn.flush();
        std::thread::spawn(move || {
            let mut f = std::fs::File::from(r);
            let mut buf = vec![];
            let _ = f.read_to_end(&mut buf);
            let _ = slint::invoke_from_event_loop(move || done(buf));
        });
        true
    });
    if ok != Some(true) {
        // nothing to read
    }
}

// -------------------------------------------------------------------------------------------------------------
// Pixel format: wl_shm ARGB8888 = B, G, R, A bytes (little endian), premultiplied

#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct Bgra {
    b: u8,
    g: u8,
    r: u8,
    a: u8,
}

impl TargetPixel for Bgra {
    fn blend(&mut self, c: PremultipliedRgbaColor) {
        let a = (255 - c.alpha) as u16;
        self.r = (self.r as u16 * a / 255) as u8 + c.red;
        self.g = (self.g as u16 * a / 255) as u8 + c.green;
        self.b = (self.b as u16 * a / 255) as u8 + c.blue;
        self.a = (self.a as u16 + c.alpha as u16 - (self.a as u16 * c.alpha as u16) / 255) as u8;
    }
    fn from_rgb(r: u8, g: u8, b: u8) -> Self {
        Bgra { b, g, r, a: 255 }
    }
    fn background() -> Self {
        Bgra { b: 0, g: 0, r: 0, a: 0 }
    }
}

// -------------------------------------------------------------------------------------------------------------
// The window adapter (one per Slint component)

pub struct LWin {
    id: u32,
    window: slint::Window,
    renderer: SoftwareRenderer,
    needs_redraw: Cell<bool>,
    size: Cell<PhysicalSize>,
    pub spec: RefCell<Spec>,
    /// The picture Slint draws into (kept between shows = no allocation on the fast path)
    back: RefCell<Vec<Bgra>>,
    full_next: Cell<bool>,
    inner: Weak<Inner>,
}

impl WindowAdapter for LWin {
    fn window(&self) -> &slint::Window {
        &self.window
    }
    fn size(&self) -> PhysicalSize {
        self.size.get()
    }
    fn renderer(&self) -> &dyn slint::platform::Renderer {
        &self.renderer
    }
    fn request_redraw(&self) {
        self.needs_redraw.set(true);
    }
    fn set_visible(&self, visible: bool) -> Result<(), PlatformError> {
        if let Some(i) = self.inner.upgrade() {
            i.cmds.borrow_mut().push_back(if visible { Cmd::Show(self.id) } else { Cmd::Hide(self.id) });
            i.wake();
        }
        Ok(())
    }
    fn set_size(&self, _size: slint::WindowSize) {
        // the compositor decides (configure); pins change size through set_place
    }
}

// -------------------------------------------------------------------------------------------------------------
// Platform

enum Cmd {
    Show(u32),
    Hide(u32),
    Replace(u32),
}

struct WinRef {
    id: u32,
    adapter: Weak<LWin>,
}

pub struct Inner {
    conn: Connection,
    queue: RefCell<EventQueue<St>>,
    st: RefCell<St>,
    qh: QueueHandle<St>,
    wins: RefCell<Vec<WinRef>>,
    cmds: RefCell<VecDeque<Cmd>>,
    next_id: Cell<u32>,
    proxy: Arc<ProxyShared>,
    start: Instant,
}

struct ProxyShared {
    queue: Mutex<VecDeque<Box<dyn FnOnce() + Send>>>,
    quit: std::sync::atomic::AtomicBool,
    wake_w: UnixStream,
    wake_r: UnixStream,
}

impl Inner {
    fn wake(&self) {
        let _ = (&self.proxy.wake_w).write(&[1]);
    }
}

struct LoopProxy(Arc<ProxyShared>);

impl EventLoopProxy for LoopProxy {
    fn quit_event_loop(&self) -> Result<(), slint::EventLoopError> {
        self.0.quit.store(true, std::sync::atomic::Ordering::SeqCst);
        let _ = (&self.0.wake_w).write(&[1]);
        Ok(())
    }
    fn invoke_from_event_loop(&self, event: Box<dyn FnOnce() + Send>) -> Result<(), slint::EventLoopError> {
        self.0.queue.lock().unwrap().push_back(event);
        let _ = (&self.0.wake_w).write(&[1]);
        Ok(())
    }
}

/// Draws window `win` once into a throwaway buffer at size `w` x `h` (physical px): fonts and the renderer's caches are
/// ready before the first real show (the first capture after start took ~50 ms to draw, the next ones ~10 ms).
pub fn prewarm(win: u32, w: u32, h: u32, scale: f32) {
    with_inner(|i| {
        let Some(a) = i.adapter(win) else { return };
        if w == 0 || h == 0 {
            return;
        }
        a.window.dispatch_event(WindowEvent::ScaleFactorChanged { scale_factor: scale });
        a.size.set(PhysicalSize::new(w, h));
        a.window.dispatch_event(WindowEvent::Resized { size: slint::LogicalSize::new(w as f32 / scale, h as f32 / scale) });
        let mut buf = vec![Bgra::default(); (w as usize) * (h as usize)];
        a.renderer.set_repaint_buffer_type(RepaintBufferType::NewBuffer);
        let _ = a.renderer.render(&mut buf[..], w as usize);
        a.renderer.set_repaint_buffer_type(RepaintBufferType::ReusedBuffer);
        a.full_next.set(true);
        a.needs_redraw.set(true);
    });
}

/// Can Lasso drag things out of its own surfaces here (wl_data_device_manager)?
pub fn can_drag() -> bool {
    with_inner(|i| i.st.borrow().dd.is_some()).unwrap_or(false)
}

/// Drag `items` (mime -> bytes) out of Lasso's surface `win` (the corner thumbnail into Discord / a browser / Dolphin).
/// Must run right after a button press on that surface (its serial). `icon` = straight RGBA picture shown under the
/// mouse while dragging. false = this compositor has no drag and drop for us.
pub fn start_drag(win: u32, items: Vec<(String, Arc<Vec<u8>>)>, icon: Option<(u32, u32, Vec<u8>)>) -> bool {
    with_inner(|i| {
        let mut st = i.st.borrow_mut();
        let (Some(m), Some(dev)) = (st.dd_mgr.clone(), st.dd.clone()) else { return false };
        let Some(origin) = st.surfs.iter().find(|s| s.win == win).map(|s| s.wl.clone()) else { return false };
        let src = m.create_data_source(&i.qh, ());
        for (mime, _) in &items {
            src.offer(mime.clone());
        }
        if src.version() >= 3 {
            src.set_actions(wl_data_device_manager::DndAction::Copy);
        }
        let icon_surf = icon.and_then(|(w, h, rgba)| {
            let buf = Buf::new(&st.shm, w as i32, h as i32, &i.qh, (0, 0))?;
            let out = unsafe { std::slice::from_raw_parts_mut(buf.ptr, buf.len) };
            for (d, s) in out.chunks_exact_mut(4).zip(rgba.chunks_exact(4)) {
                // ARGB8888 little endian = B G R A, premultiplied
                let a = s[3] as u16;
                d[0] = (s[2] as u16 * a / 255) as u8;
                d[1] = (s[1] as u16 * a / 255) as u8;
                d[2] = (s[0] as u16 * a / 255) as u8;
                d[3] = s[3];
            }
            let surf = st.compositor.create_surface(&i.qh, 0);
            surf.attach(Some(&buf.wl), 0, 0);
            surf.damage_buffer(0, 0, w as i32, h as i32);
            surf.commit();
            Some((surf, buf))
        });
        dev.start_drag(Some(&src), &origin, icon_surf.as_ref().map(|(s, _)| s), st.btn_serial);
        if let Some(old) = st.drag.replace(Drag { src, items, icon: icon_surf }) {
            old.end();
        }
        drop(st);
        let _ = i.conn.flush();
        true
    })
    .unwrap_or(false)
}

/// A running drag out of Lasso: the source, its data, the picture under the mouse
struct Drag {
    src: WlDataSource,
    items: Vec<(String, Arc<Vec<u8>>)>,
    icon: Option<(WlSurface, Buf)>,
}

impl Drag {
    fn end(self) {
        self.src.destroy();
        if let Some((s, _buf)) = self.icon {
            s.destroy();
        }
    }
}

pub struct WlPlatform {
    inner: Rc<Inner>,
}

impl WlPlatform {
    /// Err = no Wayland, or no layer-shell (GNOME): use the winit backend instead.
    pub fn new() -> Result<WlPlatform, String> {
        // the GNOME path on KDE: for tests, and a way out if layer-shell ever misbehaves on someone's desktop
        if std::env::var_os("LASSO_NO_LAYER_SHELL").is_some() {
            return Err("switched off (LASSO_NO_LAYER_SHELL)".into());
        }
        let conn = Connection::connect_to_env().map_err(|e| format!("no Wayland session ({e})"))?;
        let (globals, mut queue) = registry_queue_init::<St>(&conn).map_err(|e| format!("Wayland registry: {e}"))?;
        let qh = queue.handle();
        let layer_shell: ZwlrLayerShellV1 = globals.bind(&qh, 1..=4, ()).map_err(|_| "no layer-shell (GNOME?)".to_string())?;
        let compositor: WlCompositor = globals.bind(&qh, 4..=6, ()).map_err(|e| format!("wl_compositor: {e}"))?;
        let shm: WlShm = globals.bind(&qh, 1..=1, ()).map_err(|e| format!("wl_shm: {e}"))?;
        let mut st = St {
            compositor,
            shm,
            layer_shell,
            viewporter: globals.bind(&qh, 1..=1, ()).ok(),
            frac_mgr: globals.bind(&qh, 1..=1, ()).ok(),
            cursor_mgr: globals.bind(&qh, 1..=1, ()).ok(),
            xdg_out_mgr: globals.bind(&qh, 1..=3, ()).ok(),
            dc_mgr: globals.bind(&qh, 1..=2, ()).ok(),
            ext_mgr: globals.bind(&qh, 1..=1, ()).ok(),
            ext_dev: None,
            dd_mgr: globals.bind(&qh, 1..=3, ()).ok(),
            dd: None,
            drag: None,
            btn_serial: 0,
            seat: None,
            pointer: None,
            keyboard: None,
            cursor_dev: None,
            dc_dev: None,
            dc_src: None,
            dc_sel: None,
            offers: vec![],
            outputs: vec![],
            surfs: vec![],
            events: vec![],
            xkb_ctx: xkb::Context::new(xkb::CONTEXT_NO_FLAGS),
            xkb_state: None,
            mods: (false, false, false),
            ptr_win: None,
            ptr_surf: None,
            ptr_serial: 0,
            ptr_pos: (0.0, 0.0),
            kb_win: None,
            repeat_rate: 25,
            repeat_delay: 400,
            repeat: None,
            cursor_want: Default::default(),
            cursor_set: None,
        };
        let list = globals.contents().clone_list();
        for g in &list {
            if g.interface == "wl_output" {
                let o: WlOutput = globals.registry().bind(g.name, g.version.min(4), &qh, g.name);
                st.add_output(g.name, o, &qh);
            } else if g.interface == "wl_seat" && st.seat.is_none() {
                st.seat = Some(globals.registry().bind(g.name, g.version.min(7), &qh, ()));
            }
        }
        if let (Some(m), Some(s)) = (&st.dd_mgr, &st.seat) {
            st.dd = Some(m.get_data_device(s, &qh, ()));
        }
        if let (Some(m), Some(s)) = (&st.ext_mgr, &st.seat) {
            st.ext_dev = Some(m.get_data_device(s, &qh, ()));
        } else if let (Some(m), Some(s)) = (&st.dc_mgr, &st.seat) {
            st.dc_dev = Some(m.get_data_device(s, &qh, ()));
        }
        // two round trips: outputs (+ xdg-output names / positions) and the seat's capabilities
        queue.roundtrip(&mut st).map_err(|e| format!("Wayland: {e}"))?;
        queue.roundtrip(&mut st).map_err(|e| format!("Wayland: {e}"))?;
        let (wake_r, wake_w) = UnixStream::pair().map_err(|e| e.to_string())?;
        let _ = wake_r.set_nonblocking(true);
        let _ = wake_w.set_nonblocking(true);
        let inner = Rc::new(Inner {
            conn,
            queue: RefCell::new(queue),
            st: RefCell::new(st),
            qh,
            wins: RefCell::new(vec![]),
            cmds: RefCell::new(VecDeque::new()),
            next_id: Cell::new(1),
            proxy: Arc::new(ProxyShared { queue: Mutex::new(VecDeque::new()), quit: Default::default(), wake_w, wake_r }),
            start: Instant::now(),
        });
        INNER.with(|i| *i.borrow_mut() = Some(inner.clone()));
        Ok(WlPlatform { inner })
    }
}

impl Platform for WlPlatform {
    fn create_window_adapter(&self) -> Result<Rc<dyn WindowAdapter>, PlatformError> {
        let spec = NEXT_SPEC.with(|s| s.borrow().clone()).unwrap_or(Spec {
            place: Place::Corner { output: None, w: 400, h: 300, margin: 24 },
            keyboard: 2,
            namespace: "lasso",
        });
        let id = self.inner.next_id.get();
        self.inner.next_id.set(id + 1);
        LAST_ID.with(|l| l.set(id));
        let weak = Rc::downgrade(&self.inner);
        let w = Rc::new_cyclic(|me: &Weak<LWin>| LWin {
            id,
            window: slint::Window::new(me.clone() as Weak<dyn WindowAdapter>),
            renderer: SoftwareRenderer::new_with_repaint_buffer_type(RepaintBufferType::ReusedBuffer),
            needs_redraw: Cell::new(true),
            size: Cell::new(PhysicalSize::new(1, 1)),
            spec: RefCell::new(spec),
            back: RefCell::new(vec![]),
            full_next: Cell::new(true),
            inner: weak,
        });
        self.inner.wins.borrow_mut().push(WinRef { id, adapter: Rc::downgrade(&w) });
        Ok(w)
    }

    fn duration_since_start(&self) -> Duration {
        self.inner.start.elapsed()
    }

    fn new_event_loop_proxy(&self) -> Option<Box<dyn EventLoopProxy>> {
        Some(Box::new(LoopProxy(self.inner.proxy.clone())))
    }

    fn run_event_loop(&self) -> Result<(), PlatformError> {
        let i = &self.inner;
        loop {
            // 1. calls from other threads
            loop {
                let job = i.proxy.queue.lock().unwrap().pop_front();
                match job {
                    Some(j) => j(),
                    None => break,
                }
            }
            if i.proxy.quit.swap(false, std::sync::atomic::Ordering::SeqCst) {
                return Ok(());
            }
            // 2. show / hide
            i.run_cmds();
            // 3. timers, then draw what changed
            slint::platform::update_timers_and_animations();
            i.key_repeat();
            i.draw_all();
            let _ = i.conn.flush();
            // 4. sleep until Wayland, another thread, a timer or key repeat
            let mut timeout = slint::platform::duration_until_next_timer_update();
            if let Some((_, _, at, _)) = &i.st.borrow().repeat {
                let d = at.saturating_duration_since(Instant::now());
                timeout = Some(timeout.map(|t| t.min(d)).unwrap_or(d));
            }
            if i.wins.borrow().iter().any(|w| w.adapter.upgrade().map(|a| a.window.has_active_animations()).unwrap_or(false)) {
                timeout = Some(timeout.map(|t| t.min(Duration::from_millis(16))).unwrap_or(Duration::from_millis(16)));
            }
            if !i.proxy.queue.lock().unwrap().is_empty() || !i.cmds.borrow().is_empty() {
                timeout = Some(Duration::ZERO);
            }
            let read_guard = i.queue.borrow_mut().prepare_read();
            match read_guard {
                Some(guard) => {
                    let wfd = guard.connection_fd().as_raw_fd();
                    let mut fds = [
                        libc::pollfd { fd: wfd, events: libc::POLLIN, revents: 0 },
                        libc::pollfd { fd: i.proxy.wake_r.as_raw_fd(), events: libc::POLLIN, revents: 0 },
                    ];
                    let ms = timeout.map(|t| t.as_millis().min(i32::MAX as u128) as i32 + if t.subsec_nanos() % 1_000_000 > 0 { 1 } else { 0 }).unwrap_or(-1);
                    unsafe { libc::poll(fds.as_mut_ptr(), 2, ms) };
                    if fds[0].revents & libc::POLLIN != 0 {
                        if let Err(e) = guard.read() {
                            if !matches!(e, wayland_client::backend::WaylandError::Io(ref io) if io.kind() == std::io::ErrorKind::WouldBlock) {
                                eprintln!("lasso: wayland read: {e}");
                                return Err(PlatformError::Other(format!("Wayland connection lost: {e}")));
                            }
                        }
                    } else {
                        drop(guard);
                    }
                    if fds[1].revents & libc::POLLIN != 0 {
                        let mut b = [0u8; 64];
                        while (&i.proxy.wake_r).read(&mut b).map(|n| n > 0).unwrap_or(false) {}
                    }
                }
                None => {}
            }
            {
                let mut q = i.queue.borrow_mut();
                let mut st = i.st.borrow_mut();
                if let Err(e) = q.dispatch_pending(&mut st) {
                    return Err(PlatformError::Other(format!("Wayland: {e}")));
                }
            }
            i.handle_events();
        }
    }
}

impl Inner {
    fn adapter(&self, id: u32) -> Option<Rc<LWin>> {
        self.wins.borrow().iter().find(|w| w.id == id).and_then(|w| w.adapter.upgrade())
    }

    fn run_cmds(&self) {
        loop {
            let c = self.cmds.borrow_mut().pop_front();
            let Some(c) = c else { break };
            match c {
                Cmd::Show(id) => {
                    if self.st.borrow().surfs.iter().any(|s| s.win == id) {
                        continue;
                    }
                    let Some(a) = self.adapter(id) else { continue };
                    let spec = a.spec.borrow().clone();
                    let mut st = self.st.borrow_mut();
                    st.make_surface(id, &spec, &self.qh);
                }
                Cmd::Hide(id) => {
                    let mut st = self.st.borrow_mut();
                    st.remove_surface(id);
                    // light in the tray: the full-screen picture Slint drew into goes too (made again on the next show)
                    if let Some(a) = self.adapter(id) {
                        *a.back.borrow_mut() = Vec::new();
                        a.full_next.set(true);
                    }
                }
                Cmd::Replace(id) => {
                    let Some(a) = self.adapter(id) else { continue };
                    let spec = a.spec.borrow().clone();
                    let mut st = self.st.borrow_mut();
                    if let Some(s) = st.surfs.iter_mut().find(|s| s.win == id) {
                        // same surface: new size / margins (pins)
                        apply_place(&s.layer, &spec.place);
                        s.wl.commit();
                    }
                }
            }
        }
    }

    fn handle_events(&self) {
        let evs: Vec<Ev> = std::mem::take(&mut self.st.borrow_mut().events);
        for e in evs {
            match e {
                Ev::Configured { win, w, h } => {
                    if let Some(a) = self.adapter(win) {
                        let scale = self.st.borrow().surfs.iter().find(|s| s.win == win).map(|s| s.scale()).unwrap_or(1.0);
                        let pw = (w as f32 * scale).round() as u32;
                        let ph = (h as f32 * scale).round() as u32;
                        a.window.dispatch_event(WindowEvent::ScaleFactorChanged { scale_factor: scale });
                        a.size.set(PhysicalSize::new(pw.max(1), ph.max(1)));
                        a.window.dispatch_event(WindowEvent::Resized { size: slint::LogicalSize::new(w as f32, h as f32) });
                        a.full_next.set(true);
                        a.needs_redraw.set(true);
                    }
                    self.hook(Input::Configured { win });
                }
                Ev::Closed { win } => {
                    self.st.borrow_mut().remove_surface(win);
                    self.hook(Input::Closed { win });
                }
                Ev::Enter { win, x, y } => {
                    if let Some(a) = self.adapter(win) {
                        a.window.dispatch_event(WindowEvent::PointerMoved { position: LogicalPosition::new(x, y) });
                    }
                    self.hook(Input::PointerOn { win });
                }
                Ev::Leave { win } => {
                    if let Some(a) = self.adapter(win) {
                        a.window.dispatch_event(WindowEvent::PointerExited);
                    }
                }
                Ev::Motion { win, x, y } => {
                    if let Some(a) = self.adapter(win) {
                        a.window.dispatch_event(WindowEvent::PointerMoved { position: LogicalPosition::new(x, y) });
                    }
                }
                Ev::Button { win, x, y, button, pressed } => {
                    if let Some(a) = self.adapter(win) {
                        let position = LogicalPosition::new(x, y);
                        a.window.dispatch_event(if pressed {
                            WindowEvent::PointerPressed { position, button }
                        } else {
                            WindowEvent::PointerReleased { position, button }
                        });
                    }
                }
                Ev::DragDone { dropped } => self.hook(Input::DragDone { dropped }),
                Ev::Scroll { win, x, y, dy } => {
                    if let Some(a) = self.adapter(win) {
                        a.window.dispatch_event(WindowEvent::PointerScrolled { position: LogicalPosition::new(x, y), delta_x: 0.0, delta_y: dy });
                    }
                }
                Ev::Key { win, ev, pressed } => self.hook(Input::Key { win, ev, pressed }),
                Ev::Mods { ctrl, shift, alt } => self.hook(Input::Mods { ctrl, shift, alt }),
            }
        }
    }

    fn hook(&self, i: Input) {
        // take the hook out while it runs: it may show / hide windows (which only queues commands)
        let h = HOOK.with(|h| h.borrow_mut().take());
        if let Some(mut h) = h {
            h(i);
            HOOK.with(|slot| {
                let mut s = slot.borrow_mut();
                if s.is_none() {
                    *s = Some(h);
                }
            });
        }
    }

    fn key_repeat(&self) {
        let due = {
            let mut st = self.st.borrow_mut();
            let rate = st.repeat_rate.max(1) as u64;
            match st.repeat.as_mut() {
                Some((ev, win, at, _)) if Instant::now() >= *at => {
                    *at = Instant::now() + Duration::from_millis(1000 / rate);
                    Some((ev.clone(), *win))
                }
                _ => None,
            }
        };
        if let Some((mut ev, win)) = due {
            ev.repeat = true;
            self.hook(Input::Key { win, ev, pressed: true });
        }
    }

    fn draw_all(&self) {
        let ids: Vec<u32> = self.st.borrow().surfs.iter().filter(|s| s.configured && !s.frame_pending).map(|s| s.win).collect();
        for id in ids {
            let Some(a) = self.adapter(id) else { continue };
            if !a.needs_redraw.get() && !a.window.has_active_animations() {
                continue;
            }
            let size = a.size.get();
            let (w, h) = (size.width as usize, size.height as usize);
            if w == 0 || h == 0 {
                continue;
            }
            let mut st = self.st.borrow_mut();
            let qh = self.qh.clone();
            let shm = st.shm.clone();
            let Some(s) = st.surfs.iter_mut().find(|s| s.win == id) else { continue };
            // a free buffer of the right size
            s.bufs.retain(|b| b.busy || (b.w == w as i32 && b.h == h as i32));
            let bi = match s.bufs.iter().position(|b| !b.busy) {
                Some(i) => i,
                None => {
                    if s.bufs.len() >= 3 {
                        continue;
                    }
                    match Buf::new(&shm, w as i32, h as i32, &qh, (id, s.serial)) {
                        Some(b) => {
                            s.bufs.push(b);
                            s.bufs.len() - 1
                        }
                        None => continue,
                    }
                }
            };
            a.needs_redraw.set(false);
            let mut back = a.back.borrow_mut();
            let full = a.full_next.replace(false) || back.len() != w * h;
            if back.len() != w * h {
                *back = vec![Bgra::default(); w * h];
            }
            if full {
                a.renderer.set_repaint_buffer_type(RepaintBufferType::NewBuffer);
            }
            let t_render = Instant::now();
            let region = a.renderer.render(&mut back[..], w);
            let render_us = t_render.elapsed().as_micros();
            if full {
                a.renderer.set_repaint_buffer_type(RepaintBufferType::ReusedBuffer);
            }
            let (mut x0, mut y0, mut x1, mut y1) = (w as i32, h as i32, 0i32, 0i32);
            for (o, sz) in region.iter() {
                x0 = x0.min(o.x);
                y0 = y0.min(o.y);
                x1 = x1.max(o.x + sz.width as i32);
                y1 = y1.max(o.y + sz.height as i32);
            }
            let dirty = if full { Some((0, 0, w as i32, h as i32)) } else if x1 > x0 && y1 > y0 { Some((x0, y0, x1, y1)) } else { None };
            // every buffer collects what changed since it was last shown
            for b in s.bufs.iter_mut() {
                b.pending = match (b.pending, dirty) {
                    (Some(p), Some(d)) => Some((p.0.min(d.0), p.1.min(d.1), p.2.max(d.2), p.3.max(d.3))),
                    (p, d) => p.or(d),
                };
            }
            let b = &mut s.bufs[bi];
            if b.fresh {
                b.pending = Some((0, 0, w as i32, h as i32));
                b.fresh = false;
            }
            let Some((px0, py0, px1, py1)) = b.pending.take() else {
                // nothing changed
                continue;
            };
            let src: &[u8] = unsafe { std::slice::from_raw_parts(back.as_ptr() as *const u8, back.len() * 4) };
            let dst = unsafe { std::slice::from_raw_parts_mut(b.ptr, b.len) };
            let stride = w * 4;
            for y in py0.max(0) as usize..(py1 as usize).min(h) {
                let a0 = y * stride + px0.max(0) as usize * 4;
                let a1 = y * stride + (px1 as usize).min(w) * 4;
                dst[a0..a1].copy_from_slice(&src[a0..a1]);
            }
            if std::env::var_os("LASSO_DEBUG_WL").is_some() {
                let mid = back[(h / 2) * w + w / 2];
                let rects: Vec<String> = region.iter().map(|(o, sz)| format!("{},{}+{}x{}", o.x, o.y, sz.width, sz.height)).collect();
                crate::log::line("wl", &format!("win {id}: frame {w}x{h} full={full} damage {px0},{py0}-{px1},{py1} render {render_us} us, total {} us mid rgba=({},{},{},{}) slint [{}]", t_render.elapsed().as_micros(), mid.r, mid.g, mid.b, mid.a, rects.join(" ")));
            }
            b.busy = true;
            s.wl.attach(Some(&b.wl), 0, 0);
            s.wl.damage_buffer(px0, py0, px1 - px0, py1 - py0);
            s.wl.frame(&qh, id);
            s.frame_pending = true;
            s.wl.commit();
            if let Some(t) = s.shown_at.take() {
                if spec_is_fill(&a) {
                    crate::log::line("wl", &format!("first frame {} ms after show ({w} x {h})", t.elapsed().as_millis()));
                }
            }
        }
    }
}

fn spec_is_fill(a: &LWin) -> bool {
    matches!(a.spec.borrow().place, Place::Fill { .. })
}

fn apply_place(layer: &ZwlrLayerSurfaceV1, place: &Place) {
    match place {
        Place::Fill { .. } => {
            layer.set_anchor(Anchor::Top | Anchor::Bottom | Anchor::Left | Anchor::Right);
            layer.set_exclusive_zone(-1);
            layer.set_size(0, 0);
        }
        Place::Corner { w, h, margin, .. } => {
            layer.set_anchor(Anchor::Bottom | Anchor::Right);
            layer.set_margin(0, *margin, *margin, 0);
            layer.set_size(*w as u32, *h as u32);
        }
        Place::At { x, y, w, h, .. } => {
            layer.set_anchor(Anchor::Top | Anchor::Left);
            layer.set_exclusive_zone(-1);
            layer.set_margin(*y, 0, 0, *x);
            layer.set_size(*w as u32, *h as u32);
        }
    }
}

// -------------------------------------------------------------------------------------------------------------
// Clipboard objects of either data-control protocol (ext = the standard one, wlr = older compositors)

#[derive(Clone, PartialEq)]
enum DcOffer {
    Ext(ExtDataControlOfferV1),
    Wlr(ZwlrDataControlOfferV1),
}

impl DcOffer {
    fn receive(&self, mime: String, fd: std::os::fd::BorrowedFd) {
        match self {
            DcOffer::Ext(o) => o.receive(mime, fd),
            DcOffer::Wlr(o) => o.receive(mime, fd),
        }
    }
    fn destroy(&self) {
        match self {
            DcOffer::Ext(o) => o.destroy(),
            DcOffer::Wlr(o) => o.destroy(),
        }
    }
}

#[derive(Clone, PartialEq)]
enum DcSrc {
    Ext(ExtDataControlSourceV1),
    Wlr(ZwlrDataControlSourceV1),
}

impl DcSrc {
    fn destroy(&self) {
        match self {
            DcSrc::Ext(o) => o.destroy(),
            DcSrc::Wlr(o) => o.destroy(),
        }
    }
}

fn dc_selection(st: &mut St, id: Option<DcOffer>) {
    // forget the offers that are not the selection any more
    st.offers.retain(|(o, _)| {
        if Some(o) == id.as_ref() {
            true
        } else {
            o.destroy();
            false
        }
    });
    st.dc_sel = id;
}

fn dc_send(st: &St, src: &DcSrc, mime_type: String, fd: OwnedFd) {
    let data = st.dc_src.as_ref().filter(|(s, _)| s == src).and_then(|(_, items)| items.iter().find(|(m, _)| *m == mime_type).map(|(_, d)| d.clone()));
    if let Some(d) = data {
        // write in a thread: a big PNG must not stall the frozen screen
        std::thread::spawn(move || {
            let mut f = std::fs::File::from(fd);
            let _ = f.write_all(&d);
        });
    }
}

// -------------------------------------------------------------------------------------------------------------
// Wayland state

enum Ev {
    Configured { win: u32, w: i32, h: i32 },
    Closed { win: u32 },
    Enter { win: u32, x: f32, y: f32 },
    Leave { win: u32 },
    Motion { win: u32, x: f32, y: f32 },
    Button { win: u32, x: f32, y: f32, button: PointerEventButton, pressed: bool },
    Scroll { win: u32, x: f32, y: f32, dy: f32 },
    Key { win: u32, ev: KeyEv, pressed: bool },
    Mods { ctrl: bool, shift: bool, alt: bool },
    DragDone { dropped: bool },
}

struct Output {
    info: OutputInfo,
    wl: WlOutput,
    xdg: Option<ZxdgOutputV1>,
    done: bool,
}

struct Buf {
    wl: WlBuffer,
    _pool: WlShmPool,
    _fd: OwnedFd,
    ptr: *mut u8,
    len: usize,
    w: i32,
    h: i32,
    busy: bool,
    fresh: bool,
    pending: Option<(i32, i32, i32, i32)>,
}

impl Buf {
    fn new(shm: &WlShm, w: i32, h: i32, qh: &QueueHandle<St>, key: (u32, u32)) -> Option<Buf> {
        let len = (w * h * 4) as usize;
        let name = std::ffi::CString::new("lasso").ok()?;
        let fd = unsafe { libc::memfd_create(name.as_ptr(), libc::MFD_CLOEXEC) };
        if fd < 0 {
            return None;
        }
        let fd = unsafe { OwnedFd::from_raw_fd(fd) };
        if unsafe { libc::ftruncate(fd.as_raw_fd(), len as i64) } != 0 {
            return None;
        }
        let ptr = unsafe { libc::mmap(std::ptr::null_mut(), len, libc::PROT_READ | libc::PROT_WRITE, libc::MAP_SHARED, fd.as_raw_fd(), 0) };
        if ptr == libc::MAP_FAILED {
            return None;
        }
        let pool = shm.create_pool(fd.as_fd(), len as i32, qh, ());
        let wl = pool.create_buffer(0, w, h, w * 4, wl_shm::Format::Argb8888, qh, key);
        Some(Buf { wl, _pool: pool, _fd: fd, ptr: ptr as *mut u8, len, w, h, busy: false, fresh: true, pending: None })
    }
}

impl Drop for Buf {
    fn drop(&mut self) {
        self.wl.destroy();
        unsafe { libc::munmap(self.ptr as *mut _, self.len) };
    }
}

struct Surf {
    win: u32,
    /// changes every time the window is shown (buffer events of an old surface are ignored)
    serial: u32,
    wl: WlSurface,
    layer: ZwlrLayerSurfaceV1,
    configured: bool,
    frame_pending: bool,
    frac: Option<WpFractionalScaleV1>,
    viewport: Option<WpViewport>,
    scale120: u32,
    int_scale: i32,
    size: (i32, i32),
    bufs: Vec<Buf>,
    /// when it was asked for, until its first frame is on its way (the freeze time in the log)
    shown_at: Option<Instant>,
}

impl Surf {
    fn scale(&self) -> f32 {
        if self.scale120 > 0 && self.viewport.is_some() {
            self.scale120 as f32 / 120.0
        } else {
            self.int_scale.max(1) as f32
        }
    }
}

pub struct St {
    compositor: WlCompositor,
    shm: WlShm,
    layer_shell: ZwlrLayerShellV1,
    viewporter: Option<WpViewporter>,
    frac_mgr: Option<WpFractionalScaleManagerV1>,
    cursor_mgr: Option<WpCursorShapeManagerV1>,
    xdg_out_mgr: Option<ZxdgOutputManagerV1>,
    dc_mgr: Option<ZwlrDataControlManagerV1>,
    seat: Option<WlSeat>,
    pointer: Option<WlPointer>,
    keyboard: Option<WlKeyboard>,
    cursor_dev: Option<WpCursorShapeDeviceV1>,
    dc_dev: Option<ZwlrDataControlDeviceV1>,
    ext_mgr: Option<ExtDataControlManagerV1>,
    ext_dev: Option<ExtDataControlDeviceV1>,
    /// normal drag and drop (dragging the thumbnail out)
    dd_mgr: Option<WlDataDeviceManager>,
    dd: Option<WlDataDevice>,
    drag: Option<Drag>,
    /// serial of the last button press (start_drag needs it)
    btn_serial: u32,
    dc_src: Option<(DcSrc, Vec<(String, Arc<Vec<u8>>)>)>,
    dc_sel: Option<DcOffer>,
    offers: Vec<(DcOffer, Vec<String>)>,
    outputs: Vec<Output>,
    surfs: Vec<Surf>,
    events: Vec<Ev>,
    xkb_ctx: xkb::Context,
    xkb_state: Option<(xkb::Keymap, xkb::State)>,
    mods: (bool, bool, bool),
    ptr_win: Option<u32>,
    /// the surface the pointer last entered - looked up again when that enter came before Lasso knew the surface
    /// (21 Nov 2026: "sometimes Lasso doesn't let me select on the main monitor, only the side one")
    ptr_surf: Option<WlSurface>,
    ptr_serial: u32,
    ptr_pos: (f32, f32),
    kb_win: Option<u32>,
    repeat_rate: i32,
    repeat_delay: i32,
    /// (key, window, next repeat, evdev keycode)
    repeat: Option<(KeyEv, u32, Instant, u32)>,
    cursor_want: std::collections::HashMap<u32, CShape>,
    cursor_set: Option<CShape>,
}

impl St {
    fn add_output(&mut self, reg: u32, wl: WlOutput, qh: &QueueHandle<St>) {
        let xdg = self.xdg_out_mgr.as_ref().map(|m| m.get_xdg_output(&wl, qh, reg));
        self.outputs.push(Output { info: OutputInfo { reg, scale: 1, ..Default::default() }, wl, xdg, done: false });
    }

    fn make_surface(&mut self, win: u32, spec: &Spec, qh: &QueueHandle<St>) {
        static SERIAL: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(1);
        let serial = SERIAL.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let out_reg = match &spec.place {
            Place::Fill { output } => Some(*output),
            Place::Corner { output, .. } | Place::At { output, .. } => *output,
        };
        let output = out_reg.and_then(|r| self.outputs.iter().find(|o| o.info.reg == r)).map(|o| o.wl.clone());
        let int_scale = out_reg.and_then(|r| self.outputs.iter().find(|o| o.info.reg == r)).map(|o| o.info.scale).unwrap_or(1);
        let wl = self.compositor.create_surface(qh, win);
        let layer = self.layer_shell.get_layer_surface(&wl, output.as_ref(), Layer::Overlay, spec.namespace.to_string(), qh, (win, serial));
        apply_place(&layer, &spec.place);
        layer.set_keyboard_interactivity(match spec.keyboard {
            1 => KeyboardInteractivity::Exclusive,
            2 => KeyboardInteractivity::OnDemand,
            _ => KeyboardInteractivity::None,
        });
        let frac = self.frac_mgr.as_ref().map(|m| m.get_fractional_scale(&wl, qh, win));
        let viewport = if frac.is_some() { self.viewporter.as_ref().map(|v| v.get_viewport(&wl, qh, ())) } else { None };
        if viewport.is_none() && int_scale > 1 {
            wl.set_buffer_scale(int_scale);
        }
        wl.commit();
        self.surfs.push(Surf { win, serial, wl, layer, configured: false, frame_pending: false, frac, viewport, scale120: 0, int_scale, size: (0, 0), bufs: vec![], shown_at: Some(Instant::now()) });
    }

    fn remove_surface(&mut self, win: u32) {
        if let Some(i) = self.surfs.iter().position(|s| s.win == win) {
            let s = self.surfs.remove(i);
            if let Some(f) = &s.frac {
                f.destroy();
            }
            if let Some(v) = &s.viewport {
                v.destroy();
            }
            s.layer.destroy();
            s.wl.destroy();
            if self.ptr_win == Some(win) {
                self.ptr_win = None;
            }
            if self.kb_win == Some(win) {
                self.kb_win = None;
                self.repeat = None;
            }
        }
    }

    fn apply_cursor(&mut self) {
        let Some(win) = self.ptr_win else { return };
        let want = self.cursor_want.get(&win).copied().unwrap_or(CShape::Default);
        if let Some(d) = &self.cursor_dev {
            d.set_shape(self.ptr_serial, want);
            self.cursor_set = Some(want);
        }
    }

    /// The pointer is on a surface whose enter came too early: find its window now and tell the app it entered
    fn resolve_ptr(&mut self) {
        if self.ptr_win.is_some() {
            return;
        }
        let Some(win) = self.ptr_surf.as_ref().and_then(|s| self.surf_of(s)) else { return };
        self.ptr_win = Some(win);
        let (x, y) = self.ptr_pos;
        self.events.push(Ev::Enter { win, x, y });
        crate::log::line("wl", "pointer surface found late - input works there now");
        self.cursor_set = None;
        self.apply_cursor();
    }

    fn surf_of(&self, s: &WlSurface) -> Option<u32> {
        self.surfs.iter().find(|x| &x.wl == s).map(|x| x.win)
    }

    fn update_mods(&mut self) {
        if let Some((_, st)) = &self.xkb_state {
            let c = st.mod_name_is_active(xkb::MOD_NAME_CTRL, xkb::STATE_MODS_EFFECTIVE);
            let s = st.mod_name_is_active(xkb::MOD_NAME_SHIFT, xkb::STATE_MODS_EFFECTIVE);
            let a = st.mod_name_is_active(xkb::MOD_NAME_ALT, xkb::STATE_MODS_EFFECTIVE);
            if (c, s, a) != self.mods {
                self.mods = (c, s, a);
                self.events.push(Ev::Mods { ctrl: c, shift: s, alt: a });
            }
        }
    }

    fn key_event(&mut self, code: u32, pressed: bool) {
        let Some(win) = self.kb_win else { return };
        let Some((km, st)) = &self.xkb_state else { return };
        let kc = xkb::Keycode::new(code + 8);
        let sym = st.key_get_one_sym(kc);
        let name = xkb::keysym_get_name(sym);
        let utf = xkb::keysym_to_utf32(sym);
        let key = match name.as_str() {
            "ISO_Left_Tab" => "Tab".to_string(),
            "KP_Enter" => "Return".into(),
            "space" => "space".into(),
            "plus" | "KP_Add" => "plus".into(),
            "minus" | "KP_Subtract" => "minus".into(),
            "equal" => "equal".into(),
            "BackSpace" | "Return" | "Tab" | "Escape" | "Delete" | "Left" | "Right" | "Up" | "Down" | "Home" | "End" => name.clone(),
            n if n.starts_with('F') && n[1..].parse::<u32>().is_ok() => n.to_string(),
            n if n.starts_with("KP_") && n.len() == 4 => n[3..].to_string(),
            _ => match char::from_u32(utf).filter(|c| !c.is_control() && utf != 0) {
                Some(c) => c.to_lowercase().to_string(),
                None => name.clone(),
            },
        };
        let (ctrl, shift, alt) = self.mods;
        let text = if ctrl || alt { String::new() } else { st.key_get_utf8(kc).chars().filter(|c| !c.is_control()).collect() };
        let ev = KeyEv { key, phys: evdev_us(code), text, ctrl, shift, alt, repeat: false };
        let repeats = km.key_repeats(kc);
        if pressed {
            if repeats && self.repeat_rate > 0 {
                self.repeat = Some((ev.clone(), win, Instant::now() + Duration::from_millis(self.repeat_delay.max(1) as u64), code));
            }
        } else if self.repeat.as_ref().map(|r| r.3 == code).unwrap_or(false) {
            // the same physical key went up (its NAME may differ now: Shift pressed meanwhile)
            self.repeat = None;
        }
        self.events.push(Ev::Key { win, ev, pressed });
    }
}

// --- Dispatch implementations --------------------------------------------------------------------------------

impl Dispatch<WlRegistry, GlobalListContents> for St {
    fn event(st: &mut Self, reg: &WlRegistry, ev: wl_registry::Event, _: &GlobalListContents, _: &Connection, qh: &QueueHandle<Self>) {
        match ev {
            wl_registry::Event::Global { name, interface, version } if interface == "wl_output" => {
                let o: WlOutput = reg.bind(name, version.min(4), qh, name);
                st.add_output(name, o, qh);
            }
            wl_registry::Event::GlobalRemove { name } => {
                st.outputs.retain(|o| o.info.reg != name);
            }
            _ => {}
        }
    }
}

impl Dispatch<WlOutput, u32> for St {
    fn event(st: &mut Self, _: &WlOutput, ev: wl_output::Event, reg: &u32, _: &Connection, _: &QueueHandle<Self>) {
        let Some(o) = st.outputs.iter_mut().find(|o| o.info.reg == *reg) else { return };
        match ev {
            wl_output::Event::Mode { flags, width, height, .. } => {
                if let WEnum::Value(f) = flags {
                    if f.contains(wl_output::Mode::Current) {
                        o.info.pw = width;
                        o.info.ph = height;
                    }
                }
            }
            wl_output::Event::Scale { factor } => o.info.scale = factor,
            wl_output::Event::Name { name } => o.info.name = name,
            wl_output::Event::Geometry { x, y, .. } => {
                if o.xdg.is_none() {
                    o.info.x = x;
                    o.info.y = y;
                }
            }
            wl_output::Event::Done => {
                o.done = true;
                if o.xdg.is_none() {
                    o.info.w = o.info.pw / o.info.scale.max(1);
                    o.info.h = o.info.ph / o.info.scale.max(1);
                }
            }
            _ => {}
        }
    }
}

impl Dispatch<ZxdgOutputV1, u32> for St {
    fn event(st: &mut Self, _: &ZxdgOutputV1, ev: zxdg_output_v1::Event, reg: &u32, _: &Connection, _: &QueueHandle<Self>) {
        let Some(o) = st.outputs.iter_mut().find(|o| o.info.reg == *reg) else { return };
        match ev {
            zxdg_output_v1::Event::LogicalPosition { x, y } => {
                o.info.x = x;
                o.info.y = y;
            }
            zxdg_output_v1::Event::LogicalSize { width, height } => {
                o.info.w = width;
                o.info.h = height;
            }
            zxdg_output_v1::Event::Name { name } => {
                if o.info.name.is_empty() {
                    o.info.name = name;
                }
            }
            _ => {}
        }
    }
}

impl Dispatch<WlSeat, ()> for St {
    fn event(st: &mut Self, seat: &WlSeat, ev: wl_seat::Event, _: &(), _: &Connection, qh: &QueueHandle<Self>) {
        if let wl_seat::Event::Capabilities { capabilities: WEnum::Value(c) } = ev {
            if c.contains(wl_seat::Capability::Pointer) && st.pointer.is_none() {
                let p = seat.get_pointer(qh, ());
                st.cursor_dev = st.cursor_mgr.as_ref().map(|m| m.get_pointer(&p, qh, ()));
                st.pointer = Some(p);
            }
            if c.contains(wl_seat::Capability::Keyboard) && st.keyboard.is_none() {
                st.keyboard = Some(seat.get_keyboard(qh, ()));
            }
        }
    }
}

fn button_of(code: u32) -> PointerEventButton {
    match code {
        0x110 => PointerEventButton::Left,
        0x111 => PointerEventButton::Right,
        0x112 => PointerEventButton::Middle,
        _ => PointerEventButton::Other,
    }
}

impl Dispatch<WlPointer, ()> for St {
    fn event(st: &mut Self, _: &WlPointer, ev: wl_pointer::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {
        match ev {
            wl_pointer::Event::Enter { serial, surface, surface_x, surface_y } => {
                st.ptr_serial = serial;
                st.ptr_win = st.surf_of(&surface);
                if st.ptr_win.is_none() {
                    crate::log::line("wl", "pointer entered a surface Lasso doesn't know yet - looked up again on the next move");
                }
                st.ptr_surf = Some(surface.clone());
                st.ptr_pos = (surface_x as f32, surface_y as f32);
                if let Some(win) = st.ptr_win {
                    st.events.push(Ev::Enter { win, x: surface_x as f32, y: surface_y as f32 });
                }
                st.cursor_set = None;
                st.apply_cursor();
            }
            wl_pointer::Event::Leave { .. } => {
                st.ptr_surf = None;
                if let Some(win) = st.ptr_win.take() {
                    st.events.push(Ev::Leave { win });
                }
            }
            wl_pointer::Event::Motion { surface_x, surface_y, .. } => {
                st.ptr_pos = (surface_x as f32, surface_y as f32);
                st.resolve_ptr();
                if let Some(win) = st.ptr_win {
                    st.events.push(Ev::Motion { win, x: surface_x as f32, y: surface_y as f32 });
                }
            }
            wl_pointer::Event::Button { serial, button, state, .. } => {
                st.resolve_ptr();
                if st.ptr_win.is_none() && state == WEnum::Value(wl_pointer::ButtonState::Pressed) {
                    crate::log::line("wl", &format!("a click went to no Lasso window (pointer surface known: {})", st.ptr_surf.is_some()));
                }
                if let Some(win) = st.ptr_win {
                    let pressed = matches!(state, WEnum::Value(wl_pointer::ButtonState::Pressed));
                    if pressed {
                        st.btn_serial = serial;
                    }
                    st.events.push(Ev::Button { win, x: st.ptr_pos.0, y: st.ptr_pos.1, button: button_of(button), pressed });
                }
            }
            wl_pointer::Event::Axis { axis: WEnum::Value(wl_pointer::Axis::VerticalScroll), value, .. } => {
                if let Some(win) = st.ptr_win {
                    // Wayland: + = down; Slint (like winit): + = up
                    st.events.push(Ev::Scroll { win, x: st.ptr_pos.0, y: st.ptr_pos.1, dy: -(value as f32) * 4.0 });
                }
            }
            _ => {}
        }
    }
}

impl Dispatch<WlKeyboard, ()> for St {
    fn event(st: &mut Self, _: &WlKeyboard, ev: wl_keyboard::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {
        match ev {
            wl_keyboard::Event::Keymap { format: WEnum::Value(wl_keyboard::KeymapFormat::XkbV1), fd, size } => {
                let len = size as usize;
                let ptr = unsafe { libc::mmap(std::ptr::null_mut(), len, libc::PROT_READ, libc::MAP_PRIVATE, fd.as_raw_fd(), 0) };
                if ptr == libc::MAP_FAILED {
                    return;
                }
                let bytes = unsafe { std::slice::from_raw_parts(ptr as *const u8, len) };
                let text = String::from_utf8_lossy(bytes.split(|b| *b == 0).next().unwrap_or(&[])).into_owned();
                unsafe { libc::munmap(ptr, len) };
                if let Some(km) = xkb::Keymap::new_from_string(&st.xkb_ctx, text, xkb::KEYMAP_FORMAT_TEXT_V1, xkb::KEYMAP_COMPILE_NO_FLAGS) {
                    let s = xkb::State::new(&km);
                    st.xkb_state = Some((km, s));
                }
            }
            wl_keyboard::Event::Enter { surface, .. } => {
                st.kb_win = st.surf_of(&surface);
            }
            wl_keyboard::Event::Leave { .. } => {
                st.kb_win = None;
                st.repeat = None;
            }
            wl_keyboard::Event::Modifiers { mods_depressed, mods_latched, mods_locked, group, .. } => {
                if let Some((_, s)) = &mut st.xkb_state {
                    s.update_mask(mods_depressed, mods_latched, mods_locked, 0, 0, group);
                }
                st.update_mods();
            }
            wl_keyboard::Event::Key { key, state, .. } => {
                let pressed = matches!(state, WEnum::Value(wl_keyboard::KeyState::Pressed));
                st.key_event(key, pressed);
            }
            wl_keyboard::Event::RepeatInfo { rate, delay } => {
                st.repeat_rate = rate;
                st.repeat_delay = delay;
            }
            _ => {}
        }
    }
}

impl Dispatch<ZwlrLayerSurfaceV1, (u32, u32)> for St {
    fn event(st: &mut Self, layer: &ZwlrLayerSurfaceV1, ev: zwlr_layer_surface_v1::Event, key: &(u32, u32), _: &Connection, _: &QueueHandle<Self>) {
        let (win, serial) = *key;
        match ev {
            zwlr_layer_surface_v1::Event::Configure { serial: cs, width, height } => {
                layer.ack_configure(cs);
                if let Some(s) = st.surfs.iter_mut().find(|s| s.win == win && s.serial == serial) {
                    s.configured = true;
                    s.size = (width as i32, height as i32);
                    if let Some(v) = &s.viewport {
                        v.set_destination(width as i32, height as i32);
                    }
                    st.events.push(Ev::Configured { win, w: width as i32, h: height as i32 });
                }
            }
            zwlr_layer_surface_v1::Event::Closed => {
                if st.surfs.iter().any(|s| s.win == win && s.serial == serial) {
                    st.events.push(Ev::Closed { win });
                }
            }
            _ => {}
        }
    }
}

impl Dispatch<WpFractionalScaleV1, u32> for St {
    fn event(st: &mut Self, _: &WpFractionalScaleV1, ev: wp_fractional_scale_v1::Event, win: &u32, _: &Connection, _: &QueueHandle<Self>) {
        if let wp_fractional_scale_v1::Event::PreferredScale { scale } = ev {
            if let Some(s) = st.surfs.iter_mut().find(|s| s.win == *win) {
                if s.scale120 != scale {
                    s.scale120 = scale;
                    if s.configured {
                        let (w, h) = s.size;
                        st.events.push(Ev::Configured { win: *win, w, h });
                    }
                }
            }
        }
    }
}

impl Dispatch<WlSurface, u32> for St {
    fn event(st: &mut Self, _: &WlSurface, ev: wl_surface::Event, win: &u32, _: &Connection, _: &QueueHandle<Self>) {
        if let wl_surface::Event::PreferredBufferScale { factor } = ev {
            if let Some(s) = st.surfs.iter_mut().find(|s| s.win == *win) {
                if s.viewport.is_none() && s.int_scale != factor {
                    s.int_scale = factor;
                    s.wl.set_buffer_scale(factor);
                    if s.configured {
                        let (w, h) = s.size;
                        st.events.push(Ev::Configured { win: *win, w, h });
                    }
                }
            }
        }
    }
}

impl Dispatch<WlCallback, u32> for St {
    fn event(st: &mut Self, _: &WlCallback, ev: wl_callback::Event, win: &u32, _: &Connection, _: &QueueHandle<Self>) {
        if let wl_callback::Event::Done { .. } = ev {
            if let Some(s) = st.surfs.iter_mut().find(|s| s.win == *win) {
                s.frame_pending = false;
            }
        }
    }
}

impl Dispatch<WlBuffer, (u32, u32)> for St {
    fn event(st: &mut Self, buf: &WlBuffer, ev: wl_buffer::Event, key: &(u32, u32), _: &Connection, _: &QueueHandle<Self>) {
        if let wl_buffer::Event::Release = ev {
            if let Some(s) = st.surfs.iter_mut().find(|s| s.win == key.0 && s.serial == key.1) {
                if let Some(b) = s.bufs.iter_mut().find(|b| &b.wl == buf) {
                    b.busy = false;
                }
            }
        }
    }
}

impl Dispatch<ZwlrDataControlDeviceV1, ()> for St {
    fn event(st: &mut Self, _: &ZwlrDataControlDeviceV1, ev: zwlr_data_control_device_v1::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {
        match ev {
            zwlr_data_control_device_v1::Event::DataOffer { id } => st.offers.push((DcOffer::Wlr(id), vec![])),
            zwlr_data_control_device_v1::Event::Selection { id } => dc_selection(st, id.map(DcOffer::Wlr)),
            zwlr_data_control_device_v1::Event::Finished => st.dc_dev = None,
            _ => {}
        }
    }
    event_created_child!(St, ZwlrDataControlDeviceV1, [
        zwlr_data_control_device_v1::EVT_DATA_OFFER_OPCODE => (ZwlrDataControlOfferV1, ()),
    ]);
}

impl Dispatch<ZwlrDataControlOfferV1, ()> for St {
    fn event(st: &mut Self, offer: &ZwlrDataControlOfferV1, ev: zwlr_data_control_offer_v1::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {
        if let zwlr_data_control_offer_v1::Event::Offer { mime_type } = ev {
            if let Some(o) = st.offers.iter_mut().find(|o| o.0 == DcOffer::Wlr(offer.clone())) {
                o.1.push(mime_type);
            }
        }
    }
}

impl Dispatch<ZwlrDataControlSourceV1, ()> for St {
    fn event(st: &mut Self, src: &ZwlrDataControlSourceV1, ev: zwlr_data_control_source_v1::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {
        let me = DcSrc::Wlr(src.clone());
        match ev {
            zwlr_data_control_source_v1::Event::Send { mime_type, fd } => dc_send(st, &me, mime_type, fd),
            zwlr_data_control_source_v1::Event::Cancelled => {
                if st.dc_src.as_ref().map(|(s, _)| *s == me).unwrap_or(false) {
                    st.dc_src = None;
                }
                src.destroy();
            }
            _ => {}
        }
    }
}

impl Dispatch<ExtDataControlDeviceV1, ()> for St {
    fn event(st: &mut Self, _: &ExtDataControlDeviceV1, ev: ext_data_control_device_v1::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {
        match ev {
            ext_data_control_device_v1::Event::DataOffer { id } => st.offers.push((DcOffer::Ext(id), vec![])),
            ext_data_control_device_v1::Event::Selection { id } => dc_selection(st, id.map(DcOffer::Ext)),
            ext_data_control_device_v1::Event::Finished => st.ext_dev = None,
            _ => {}
        }
    }
    event_created_child!(St, ExtDataControlDeviceV1, [
        ext_data_control_device_v1::EVT_DATA_OFFER_OPCODE => (ExtDataControlOfferV1, ()),
    ]);
}

impl Dispatch<ExtDataControlOfferV1, ()> for St {
    fn event(st: &mut Self, offer: &ExtDataControlOfferV1, ev: ext_data_control_offer_v1::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {
        if let ext_data_control_offer_v1::Event::Offer { mime_type } = ev {
            if let Some(o) = st.offers.iter_mut().find(|o| o.0 == DcOffer::Ext(offer.clone())) {
                o.1.push(mime_type);
            }
        }
    }
}

impl Dispatch<ExtDataControlSourceV1, ()> for St {
    fn event(st: &mut Self, src: &ExtDataControlSourceV1, ev: ext_data_control_source_v1::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {
        let me = DcSrc::Ext(src.clone());
        match ev {
            ext_data_control_source_v1::Event::Send { mime_type, fd } => dc_send(st, &me, mime_type, fd),
            ext_data_control_source_v1::Event::Cancelled => {
                if st.dc_src.as_ref().map(|(s, _)| *s == me).unwrap_or(false) {
                    st.dc_src = None;
                }
                src.destroy();
            }
            _ => {}
        }
    }
}

impl Dispatch<WlDataDevice, ()> for St {
    fn event(_: &mut Self, _: &WlDataDevice, ev: wl_data_device::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {
        // drops onto Lasso's own surfaces aren't used: offers are dropped at once
        if let wl_data_device::Event::DataOffer { id } = ev {
            id.destroy();
        }
    }
    event_created_child!(St, WlDataDevice, [
        wl_data_device::EVT_DATA_OFFER_OPCODE => (WlDataOffer, ()),
    ]);
}

impl Dispatch<WlDataSource, ()> for St {
    fn event(st: &mut Self, src: &WlDataSource, ev: wl_data_source::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {
        let mine = st.drag.as_ref().is_some_and(|d| d.src == *src);
        match ev {
            wl_data_source::Event::Send { mime_type, fd } => {
                let data = st.drag.as_ref().filter(|_| mine).and_then(|d| d.items.iter().find(|(m, _)| *m == mime_type).map(|(_, b)| b.clone()));
                if let Some(d) = data {
                    std::thread::spawn(move || {
                        let mut f = std::fs::File::from(fd);
                        let _ = f.write_all(&d);
                    });
                }
            }
            // dropped somewhere that took it, or given up (Esc, dropped on nothing)
            wl_data_source::Event::DndFinished | wl_data_source::Event::Cancelled => {
                if mine {
                    if let Some(d) = st.drag.take() {
                        d.end();
                    }
                    st.events.push(Ev::DragDone { dropped: matches!(ev, wl_data_source::Event::DndFinished) });
                } else {
                    src.destroy();
                }
            }
            _ => {}
        }
    }
}

delegate_noop!(St: ignore WlDataDeviceManager);
delegate_noop!(St: ignore WlDataOffer);
delegate_noop!(St: ignore WlCompositor);
delegate_noop!(St: ignore WlShm);
delegate_noop!(St: ignore WlShmPool);
delegate_noop!(St: ignore WlRegion);
delegate_noop!(St: ignore ZwlrLayerShellV1);
delegate_noop!(St: ignore WpViewporter);
delegate_noop!(St: ignore WpViewport);
delegate_noop!(St: ignore WpFractionalScaleManagerV1);
delegate_noop!(St: ignore WpCursorShapeManagerV1);
delegate_noop!(St: ignore WpCursorShapeDeviceV1);
delegate_noop!(St: ignore ZxdgOutputManagerV1);
delegate_noop!(St: ignore ZwlrDataControlManagerV1);
delegate_noop!(St: ignore ExtDataControlManagerV1);
