//! Sonela Lasso - screenshots with every editing tool right on the frozen screen.
//!
//! One program, several roles (chosen by the first argument):
//! - `lasso` / `lasso --region|--full|--window|--screen|--repeat|--restore|--library|--settings|--open <file>`:
//!   the always-running part (tray, hotkeys, the frozen screen). A second start just tells the running one.
//! - `lasso --settings-window` / `--library-window`: the two normal windows, each its own short-lived process
//!   (destroyed when closed - the tray part stays small, Aimdot's lesson).

#![cfg_attr(windows, windows_subsystem = "windows")]

mod app;
mod clipboard;
mod config;
mod dialogs;
mod echo_link;
mod editor;
mod export;
mod clouds;
mod gamepics;
mod ipc;
mod keys;
mod library;
mod log;
mod ocr;
mod paths;
mod ping;
mod shapes;
mod scroll;
mod share;
mod sound;
mod theme;
mod update;
mod util;
mod windows_ui;
mod winkeys;

#[cfg(target_os = "linux")]
mod capture_linux;
#[cfg(target_os = "linux")]
mod desktop;
#[cfg(target_os = "linux")]
mod hotkeys_linux;
#[cfg(target_os = "linux")]
mod tray_linux;
#[cfg(target_os = "linux")]
mod wl;
#[cfg(target_os = "linux")]
mod wl_dnd;
#[cfg(windows)]
mod win;
// the Windows monitor math is plain Rust: its tests run on Linux too
#[cfg(all(test, not(windows)))]
#[path = "win/geom.rs"]
mod win_geom;

slint::include_modules!();

use app::{App, AppRc, OutInfo, PopupSpec};
use editor::Mode;
use std::cell::{Cell, RefCell};

thread_local! {
    static APP: RefCell<Option<AppRc>> = const { RefCell::new(None) };
    pub static MODS: Cell<(bool, bool, bool)> = const { Cell::new((false, false, false)) };
    #[cfg(target_os = "linux")]
    static HOTKEYS: RefCell<Option<hotkeys_linux::Hotkeys>> = const { RefCell::new(None) };
    #[cfg(target_os = "linux")]
    static TRAY: RefCell<Option<tray_linux::Tray>> = const { RefCell::new(None) };
}

/// Runs `f` with the app (from Slint callbacks, timers, other threads' invoke_from_event_loop).
pub fn with_app(f: impl FnOnce(&mut App)) {
    let Some(a) = APP.with(|a| a.borrow().clone()) else { return };
    match a.try_borrow_mut() {
        Ok(mut g) => f(&mut g),
        Err(_) => log::line("app", "busy (nested call) - skipped"),
    };
}

/// Dark / light + the accent for the floating bars of every Lasso surface
pub fn set_bars_look(b: &B, dark: bool) {
    b.set_dark(dark);
    let p = config::Config::load().palette();
    b.set_accent(config::color(p.accent));
    b.set_accent2(config::color(p.accent2));
    b.set_accent3(config::color(p.grad[2]));
    b.set_g0(config::color(p.grad[0]));
    b.set_on_accent(config::color(p.on_accent));
}

/// After the frozen screen / a popup closed: give the freed memory back to the system (glibc keeps it otherwise)
pub fn trim_memory_soon() {
    slint::Timer::single_shot(std::time::Duration::from_millis(400), || {
        #[cfg(target_os = "linux")]
        unsafe {
            libc::malloc_trim(0);
        }
    });
}

pub fn notify(title: &str, body: &str) {
    log::line("notify", &format!("{title}: {body}"));
    #[cfg(target_os = "linux")]
    {
        let (t, b) = (title.to_string(), body.to_string());
        std::thread::spawn(move || {
            if let Ok(c) = dbus::blocking::Connection::new_session() {
                let p = c.with_proxy("org.freedesktop.Notifications", "/org/freedesktop/Notifications", std::time::Duration::from_secs(2));
                // short + transient (21 Nov 2026: "make the notifications last less time"): gone after 2.5 s and not
                // kept in the notification history
                let mut hints: dbus::arg::PropMap = Default::default();
                hints.insert("transient".into(), dbus::arg::Variant(Box::new(true)));
                let _: Result<(u32,), _> = p.method_call("org.freedesktop.Notifications", "Notify", ("Lasso", 0u32, desktop::APP_ID, t, b, Vec::<String>::new(), hints, 2500i32));
            }
        });
    }
    #[cfg(windows)]
    win::notify(title, body);
}

/// The frozen pictures of these monitors: (output key, picture)
pub fn grab(infos: &[OutInfo], cursor: bool) -> Result<Vec<(u32, shapes::Pic)>, String> {
    #[cfg(target_os = "linux")]
    {
        if wl::active() {
            let outs: Vec<wl::OutputInfo> = wl::outputs().into_iter().filter(|o| infos.iter().any(|i| i.key == o.reg)).collect();
            return capture_linux::grab_outputs(&outs, cursor).map(|v| v.into_iter().map(|s| (s.output.reg, s.pic)).collect());
        }
        let _ = cursor;
        let pic = if let Some(p) = std::env::var_os("LASSO_TEST_PICTURE") {
            export::decode(&std::fs::read(p).map_err(|e| e.to_string())?)?
        } else {
            capture_linux::portal_screenshot()?
        };
        Ok(vec![(infos.first().map(|i| i.key).unwrap_or(0), pic)])
    }
    #[cfg(windows)]
    {
        win::grab(infos, cursor)
    }
}

/// What a worker thread needs to grab one monitor again and again (made on the UI thread: the Wayland monitor list
/// lives there)
#[derive(Clone)]
pub struct LiveTarget {
    #[cfg_attr(target_os = "linux", allow(dead_code))]
    info: OutInfo,
    #[cfg(target_os = "linux")]
    wlo: Option<wl::OutputInfo>,
}

pub fn live_target(info: &OutInfo) -> LiveTarget {
    LiveTarget {
        info: info.clone(),
        #[cfg(target_os = "linux")]
        wlo: wl::outputs().into_iter().find(|o| o.reg == info.key),
    }
}

/// One monitor's picture right now, never the test picture (scrolling capture) - any thread
pub fn grab_live(t: &LiveTarget) -> Result<shapes::Pic, String> {
    #[cfg(target_os = "linux")]
    {
        match &t.wlo {
            Some(o) => capture_linux::grab_real(std::slice::from_ref(o), false)?.into_iter().next().map(|s| s.pic).ok_or_else(|| "no screen".to_string()),
            None => capture_linux::portal_screenshot(),
        }
    }
    #[cfg(windows)]
    {
        win::grab(std::slice::from_ref(&t.info), false)?.into_iter().next().map(|s| s.1).ok_or_else(|| "no screen".to_string())
    }
}

/// Visible windows (logical desktop px, top first) for Window mode
pub fn window_list() -> Vec<(shapes::R, String)> {
    #[cfg(target_os = "linux")]
    {
        if std::env::var_os("LASSO_TEST_PICTURE").is_some() {
            return vec![];
        }
        capture_linux::kwin_windows()
    }
    #[cfg(windows)]
    {
        win::windows()
    }
}

pub fn platform_outputs() -> Vec<OutInfo> {
    #[cfg(windows)]
    {
        win::outputs()
    }
    #[cfg(not(windows))]
    {
        // GNOME & co. (winit): one screen ⚠ (size from the desktop's first picture would be better)
        vec![OutInfo { key: 0, name: String::new(), x: 0, y: 0, w: 1920, h: 1080, pw: 1920, ph: 1080 }]
    }
}

pub fn platform_theme_dark() -> bool {
    #[cfg(windows)]
    {
        win::dark_theme()
    }
    #[cfg(not(windows))]
    {
        // KDE: the colour scheme's window background
        let home = std::env::var_os("HOME").map(std::path::PathBuf::from).unwrap_or_default();
        let cfg = std::env::var_os("XDG_CONFIG_HOME").map(std::path::PathBuf::from).unwrap_or_else(|| home.join(".config"));
        if let Ok(s) = std::fs::read_to_string(cfg.join("kdeglobals")) {
            let mut in_win = false;
            for l in s.lines() {
                if l.starts_with('[') {
                    in_win = l.trim() == "[Colors:Window]";
                } else if in_win {
                    if let Some(v) = l.strip_prefix("BackgroundNormal=") {
                        let p: Vec<f32> = v.split(',').filter_map(|x| x.trim().parse().ok()).collect();
                        if p.len() >= 3 {
                            return 0.3 * p[0] + 0.59 * p[1] + 0.11 * p[2] < 128.0;
                        }
                    }
                }
            }
        }
        true
    }
}

pub fn show_overlay(comp: &Overlay, info: &OutInfo) {
    use slint::ComponentHandle;
    #[cfg(target_os = "linux")]
    if wl::active() {
        let _ = comp.show();
        return;
    }
    #[cfg(windows)]
    {
        win::show_overlay(comp, info.key);
        return;
    }
    #[allow(unreachable_code)]
    {
        windows_ui::show_fullscreen(comp.window(), info);
        let _ = comp.show();
        windows_ui::raise(comp.window());
    }
}

pub fn make_popup<T: slint::ComponentHandle>(spec: PopupSpec, ctor: impl FnOnce() -> Result<T, slint::PlatformError>) -> Result<(T, u32), String> {
    #[cfg(target_os = "linux")]
    if wl::active() {
        let s = wl::Spec { place: wl::Place::Corner { output: None, w: spec.w, h: spec.h, margin: 18 }, keyboard: spec.kb, namespace: "lasso-popup" };
        let (c, id) = wl::create(s, ctor);
        return c.map(|c| (c, id)).map_err(|e| e.to_string());
    }
    let c = ctor().map_err(|e| e.to_string())?;
    windows_ui::place_corner(c.window(), spec.w, spec.h);
    Ok((c, 0))
}

pub fn make_pin<T: slint::ComponentHandle>(out: Option<u32>, pos: (i32, i32), size: (i32, i32), ctor: impl FnOnce() -> Result<T, slint::PlatformError>) -> Result<(T, u32), String> {
    #[cfg(target_os = "linux")]
    if wl::active() {
        let s = wl::Spec { place: wl::Place::At { output: out, x: pos.0, y: pos.1, w: size.0, h: size.1 }, keyboard: 2, namespace: "lasso-pin" };
        let (c, id) = wl::create(s, ctor);
        return c.map(|c| (c, id)).map_err(|e| e.to_string());
    }
    let _ = out;
    let c = ctor().map_err(|e| e.to_string())?;
    #[cfg(windows)]
    win::place_pin(c.window(), out, pos, size);
    #[cfg(not(windows))]
    windows_ui::place_at(c.window(), pos, size);
    Ok((c, 0))
}

/// Can a picture file be dragged out of a Lasso popup here? (KDE / wlroots: wl_data_device; Windows: the shell's
/// drag and drop; GNOME's normal-window path: not yet)
pub fn can_drag_out() -> bool {
    #[cfg(target_os = "linux")]
    if wl::active() {
        return wl::can_drag();
    }
    cfg!(windows)
}

/// Drag a picture file out of popup `win` (the corner thumbnail). `png` = the same picture for apps that take raw
/// pictures, `icon` = a small RGBA picture under the mouse. Windows: runs the shell's drag loop and returns after the
/// drop (Some(dropped)); KDE: returns at once (None) and the end comes as wl::Input::DragDone.
pub fn drag_out(win: u32, path: &std::path::Path, png: std::sync::Arc<Vec<u8>>, icon: Option<(u32, u32, Vec<u8>)>) -> Result<Option<bool>, ()> {
    #[cfg(target_os = "linux")]
    if wl::active() {
        let uri = format!("file://{}\r\n", crate::util::url_encode_path(&path.to_string_lossy()));
        let items = vec![("text/uri-list".to_string(), std::sync::Arc::new(uri.into_bytes())), ("image/png".to_string(), png)];
        return if wl::start_drag(win, items, icon) { Ok(None) } else { Err(()) };
    }
    let _ = (win, png, icon);
    #[cfg(windows)]
    return win::drag_file(path).map(Some).ok_or(());
    #[cfg(not(windows))]
    {
        let _ = path;
        Err(())
    }
}

/// A pin lets clicks through (over a game) or takes them again
pub fn pin_click_through<T: slint::ComponentHandle>(comp: &T, win: u32, on: bool) {
    #[cfg(target_os = "linux")]
    if wl::active() {
        let _ = comp;
        wl::set_click_through(win, on);
        return;
    }
    let _ = win;
    use slint::winit_030::WinitWindowAccessor;
    comp.window().with_winit_window(|w| {
        let _ = w.set_cursor_hittest(!on);
    });
}

pub fn move_pin<T: slint::ComponentHandle>(comp: &T, win: u32, out: Option<u32>, pos: (i32, i32), size: (i32, i32)) {
    #[cfg(target_os = "linux")]
    if wl::active() {
        let _ = comp;
        wl::set_place(win, wl::Place::At { output: out, x: pos.0, y: pos.1, w: size.0, h: size.1 });
        return;
    }
    let _ = (win, out);
    #[cfg(windows)]
    win::place_pin(comp.window(), out, pos, size);
    #[cfg(not(windows))]
    windows_ui::place_at(comp.window(), pos, size);
}

pub fn open_dialog() {
    let dir = config::Config::load().save_dir();
    std::thread::spawn(move || {
        if let Some(p) = dialogs::pick(false, "Open a picture in Lasso", "", &dir) {
            let p = p.to_string_lossy().to_string();
            let _ = slint::invoke_from_event_loop(move || {
                with_app(|a| a.open_file(&p));
            });
        }
    });
}

pub fn save_dialog(dir: std::path::PathBuf, name: String, done: impl FnOnce(std::path::PathBuf) + Send + 'static) {
    std::thread::spawn(move || {
        if let Some(p) = dialogs::pick(true, "Save the screenshot", &name, &dir) {
            done(p);
        }
    });
}

/// A command from the tray, a hotkey, a second `lasso` start, or the Settings / Library windows.
fn handle(cmd: &str) {
    log::line("cmd", cmd.split(':').next().unwrap_or(""));
    let (c, arg) = cmd.split_once(':').unwrap_or((cmd, ""));
    with_app(|a| match c {
        // left-click on the tray icon (26 Nov 2026): the flyout with the latest captures
        "flyout" => a.toggle_flyout(),
        "region" => a.capture(Mode::Area, true),
        "window" => a.capture(Mode::Window, true),
        "screen" => a.capture(Mode::Screen, true),
        "full" => a.full_shot(),
        "repeat" => a.repeat_last(),
        "restore" => a.restore(),
        "delete" => a.delete_last(),
        "library" => a.open_window("--library-window"),
        "settings" => a.open_window("--settings-window"),
        // "edit:<id>\u{1f}<monitor>" from the Library: open it on that monitor
        "edit" | "open" => {
            let (arg, mon) = arg.split_once('\u{1f}').unwrap_or((arg, ""));
            a.want_monitor(mon);
            if c == "edit" { a.edit_library(arg) } else { a.open_file(arg) }
        }
        "open-dialog" => open_dialog(),
        "copy-file" => a.copy_file(arg),
        "pin-file" => a.pin_file(arg),
        "copy-text" => clipboard::set_text(arg),
        "pins-clickable" => a.pins_clickable(),
        "copy-lib" => a.copy_lib(arg),
        "pin-lib" => a.pin_lib(arg),
        // the Library's Share link (24 Nov 2026): a capture by id, a save-folder picture by path
        "share-lib" => a.share_lib(arg),
        "share-file" => a.share_file(arg),
        "copy-files" => clipboard::set_files(&arg.split('|').filter(|s| !s.is_empty()).collect::<Vec<_>>()),
        "reload" => a.reload_config(),
        // the tray's "Update to Lasso X": Settings > Updates installs it
        "update" => update::open_settings_and_install(|| a.open_window_page("--settings-window", 5)),
        // the Settings window checked: show / drop the tray item now
        "update-known" => set_tray_update(update::tray_label()),
        "quit" => {
            let _ = slint::quit_event_loop();
        }
        "forget" => a.forget(arg),
        "test-shot" if test_mode() => test_shot(arg),
        "test-state" if test_mode() => test_state(a, arg),
        "test-action" if test_mode() => {
            let fx = a.test_editor().map(|e| e.action(arg)).unwrap_or_default();
            a.test_sync();
            a.test_fx(fx);
        }
        _ => {}
    });
    if c == "reload" {
        start_hotkeys();
    }
}

fn test_mode() -> bool {
    std::env::var_os("LASSO_HOME").is_some()
}

/// Test runs only: the screen (with Lasso's surfaces on it) to a PNG, through KWin like a real capture
fn test_shot(path: &str) {
    #[cfg(target_os = "linux")]
    {
        let outs = wl::outputs();
        let path = path.to_string();
        std::thread::spawn(move || match capture_linux::grab_real(&outs, false) {
            Ok(mut s) if !s.is_empty() => {
                let p = s.remove(0).pic;
                let _ = export::write_new(std::path::Path::new(&path), &export::png(&p, false));
                log::line("test", &format!("screen saved ({} x {})", p.w, p.h));
            }
            Ok(_) => log::line("test", "no screen"),
            Err(e) => log::line("test", &format!("screen shot failed: {e}")),
        });
    }
    #[cfg(windows)]
    let _ = path;
}

fn test_state(a: &mut App, path: &str) {
    let showing = a.showing();
    let s = match a.test_editor() {
        Some(e) => format!(
            "showing={}\nphase={:?}\nmode={:?}\ntool={}\nbox={:?}\nshapes={}\nkinds={}\nsel={:?}\ntyping={}\nkeys_sheet={}\nundo={}\nview={:.2},{:.0},{:.0}\n",
            showing,
            e.phase,
            e.mode,
            e.tool.id(),
            e.box_ir(),
            e.doc.shapes.len(),
            e.doc.shapes.iter().map(|s| format!("{:?}", s.kind).split([' ', '{']).next().unwrap_or("").to_string()).collect::<Vec<_>>().join(","),
            e.sel,
            e.typing.is_some(),
            e.keys_sheet,
            e.can_undo(),
            e.view.s,
            e.view.ox,
            e.view.oy,
        ),
        None => format!("showing={showing}\nno editor\n"),
    };
    let texts: Vec<String> = a
        .test_editor()
        .map(|e| e.doc.shapes.iter().filter_map(|s| if let shapes::Kind::Text { text, bold, italic, .. } = &s.kind { Some(format!("{text:?}/bold={bold}/italic={italic}")) } else { None }).collect())
        .unwrap_or_default();
    let _ = std::fs::write(path, format!("{s}texts={}\n", texts.join(";")));
}

fn start_hotkeys() {
    #[cfg(target_os = "linux")]
    {
        let cfg = config::Config::load();
        if !cfg.on("hotkeys") {
            // switched off: close the portal session now (not only at the next start)
            HOTKEYS.with(|h| {
                if let Some(hk) = h.borrow_mut().take() {
                    hk.stop();
                }
            });
            return;
        }
        let keys: [String; hotkeys_linux::N] = hotkeys_linux::IDS.map(|id| cfg.get(&format!("hk.{id}")).to_string());
        HOTKEYS.with(|h| {
            let mut h = h.borrow_mut();
            match h.as_ref() {
                Some(hk) => hk.set(keys),
                None => {
                    *h = Some(hotkeys_linux::Hotkeys::start(
                        |id| {
                            let id = id.to_string();
                            let _ = slint::invoke_from_event_loop(move || handle(&id));
                        },
                        keys,
                    ))
                }
            }
        });
    }
    #[cfg(windows)]
    win::start_hotkeys(|id| {
        let id = id.to_string();
        let _ = slint::invoke_from_event_loop(move || handle(&id));
    });
}

/// "Update to Lasso X" in the tray menu (None = no update known)
fn set_tray_update(label: Option<String>) {
    #[cfg(target_os = "linux")]
    TRAY.with(|t| {
        if let Some(t) = t.borrow().as_ref() {
            t.set_update(label);
        }
    });
    #[cfg(windows)]
    win::set_tray_update(label);
}

fn restore_label(t: i64) -> String {
    let (_, _, _, h, m, _) = util::local_from_unix(t);
    format!("Restore last capture · {h:02}:{m:02}")
}

fn daemon(first: Option<String>) {
    // another Lasso started at the same moment and won: hand it the command and stop
    #[cfg(unix)]
    if !ipc::lock_single() {
        // the winner may still be starting (its socket isn't there yet): keep trying ~3 s, so a hotkey's --region
        // during its first start isn't lost (review D12)
        let cmd = first.as_deref().unwrap_or("ping");
        for _ in 0..30 {
            std::thread::sleep(std::time::Duration::from_millis(100));
            if ipc::send(cmd).is_ok() {
                break;
            }
        }
        return;
    }
    // Windows: the winit backend with Lasso's window hook + the single-instance mutex, before anything Slint
    #[cfg(windows)]
    if !win::init(first.as_deref()) {
        return;
    }
    log::trim();
    log::line("app", &format!("Lasso {} starting", env!("CARGO_PKG_VERSION")));
    #[cfg(target_os = "linux")]
    {
        desktop::appimage_reexec();
        desktop::install();
        match wl::WlPlatform::new() {
            Ok(p) => {
                if slint::platform::set_platform(Box::new(p)).is_err() {
                    log::line("app", "platform already set");
                }
                wl::set_hook(|i| match i {
                    wl::Input::Key { win, ev, pressed } => {
                        with_app(|a| a.key_for_win(win, ev, pressed));
                    }
                    wl::Input::Mods { ctrl, shift, alt } => {
                        with_app(|a| a.mods(ctrl, shift, alt));
                    }
                    wl::Input::PointerOn { win } => {
                        with_app(|a| a.pointer_on(win));
                    }
                    wl::Input::Configured { win } => {
                        with_app(|a| a.configured(win));
                    }
                    wl::Input::Closed { win } => {
                        with_app(|a| a.surface_closed(win));
                    }
                    wl::Input::DragDone { dropped } => {
                        with_app(|a| a.thumb_drag_done(dropped));
                    }
                });
                log::line("app", "KDE / wlroots: layer-shell surfaces");
            }
            Err(e) => log::line("app", &format!("no layer-shell ({e}): normal windows")),
        }
    }
    let cfg = config::Config::load();
    let app = App::new(cfg);
    APP.with(|a| *a.borrow_mut() = Some(app.clone()));
    app.borrow_mut().prepare();
    share::backup::start();
    let served = ipc::serve(|cmd| {
        let _ = slint::invoke_from_event_loop(move || handle(&cmd));
    });
    if let Err(e) = &served {
        log::line("ipc", e);
    }
    #[cfg(target_os = "linux")]
    {
        let tray = tray_linux::Tray::start(|c| {
            let c = c.to_string();
            let _ = slint::invoke_from_event_loop(move || handle(&c));
        });
        TRAY.with(|t| *t.borrow_mut() = Some(tray));
        app.borrow_mut().on_tray_changed = Some(Box::new(|t| {
            TRAY.with(|tr| {
                if let Some(tr) = tr.borrow().as_ref() {
                    tr.set_restore(t.map(restore_label));
                }
            })
        }));
    }
    #[cfg(windows)]
    {
        win::start_tray(|c| {
            let c = c.to_string();
            let _ = slint::invoke_from_event_loop(move || handle(&c));
        });
        app.borrow_mut().on_tray_changed = Some(Box::new(|t| win::set_tray_restore(t.map(restore_label))));
    }
    start_hotkeys();
    // after a restart the tray's "Restore last capture" is there too (the newest library capture)
    {
        let newest = library::list(&app.borrow().cfg.save_dir()).into_iter().find(|i| i.editable).map(|i| i.created);
        if let (Some(t), Some(f)) = (newest, app.borrow().on_tray_changed.as_ref()) {
            f(Some(t));
        }
    }
    {
        let c = &app.borrow().cfg;
        library::prune(c.num("keep_count").max(1) as usize, c.num("keep_days"));
    }
    let had_cmd = first.is_some();
    if let Some(c) = first {
        let _ = slint::invoke_from_event_loop(move || handle(&c));
    }
    if !had_cmd && std::env::var_os("LASSO_TEST_PICTURE").is_none() {
        let _ = slint::invoke_from_event_loop(|| with_app(|a| a.first_run()));
    }
    ping::start();
    update::start(|label| {
        let _ = slint::invoke_from_event_loop(move || set_tray_update(label));
    });
    if let Err(e) = slint::run_event_loop_until_quit() {
        log::line("app", &format!("event loop: {e}"));
    }
    with_app(|a| a.close_windows());
    // a capture closed just before Quit: its writes finish first (review D4)
    if !library::drain(std::time::Duration::from_secs(5)) {
        log::line("app", "quit: some library writes didn't finish in 5 s");
    }
    #[cfg(windows)]
    win::shutdown();
    // only the Lasso that made the socket removes it
    #[cfg(unix)]
    if served.is_ok() {
        let _ = std::fs::remove_file(paths::socket_path());
    }
    log::line("app", "quit");
}

fn usage() {
    println!(
        "Sonela Lasso {}\n\nlasso              start (tray icon)\nlasso --region     capture an area\nlasso --window     capture a window\nlasso --screen     capture a whole screen, with every tool\nlasso --full       full screenshot, no editing (copied + saved)\nlasso --repeat     the last box again\nlasso --restore    bring back the last capture\nlasso --library    the library\nlasso --cmd flyout the tray flyout (latest captures)\nlasso --settings   settings\nlasso --open FILE  mark up a picture\nlasso --quit       quit the running Lasso",
        env!("CARGO_PKG_VERSION")
    );
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let a0 = args.first().map(|s| s.as_str()).unwrap_or("");
    match a0 {
        "--help" | "-h" => return usage(),
        "--version" | "-V" => return println!("Lasso {}", env!("CARGO_PKG_VERSION")),
        "--settings-window" => return windows_ui::window_main(0),
        "--library-window" => return windows_ui::window_main(1),
        _ => {}
    }
    let abs = |p: &str| std::fs::canonicalize(p).map(|x| x.to_string_lossy().to_string()).unwrap_or_else(|_| p.to_string());
    let cmd = match a0 {
        "" => None,
        "--region" | "-r" => Some("region".to_string()),
        "--window" | "-w" => Some("window".into()),
        "--screen" => Some("screen".into()),
        "--full" | "-f" => Some("full".into()),
        "--repeat" => Some("repeat".into()),
        "--restore" => Some("restore".into()),
        "--library" => Some("library".into()),
        "--settings" => Some("settings".into()),
        "--quit" => Some("quit".into()),
        "--open" => args.get(1).map(|p| format!("open:{}", abs(p))),
        "--cmd" => args.get(1).cloned(),
        other if !other.starts_with('-') => Some(format!("open:{}", abs(other))),
        _ => {
            usage();
            return;
        }
    };
    // the running Lasso may take the focus when this start came from another program (Windows)
    #[cfg(windows)]
    win::allow_foreground();
    // a new AppImage started by an update: the old Lasso quits first
    update::wait_for_old();
    // the user's PC: the release and a hand build ("Lasso (dev)") - starting one replaces the other
    #[cfg(target_os = "linux")]
    desktop::take_over_other_copy();
    // already running: just tell it
    if let Ok(r) = ipc::send(cmd.as_deref().unwrap_or("ping")) {
        if r == "ok" {
            return;
        }
    }
    if cmd.as_deref() == Some("quit") {
        return;
    }
    daemon(cmd);
}
