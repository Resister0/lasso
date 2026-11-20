//! Normal (winit) windows: the Settings / Library processes (each its own short-lived process, destroyed when
//! closed), and the frozen screen / popups / pins on platforms without layer-shell (Windows, GNOME).

use crate::app::OutInfo;
use crate::config::Config;
use crate::keys;
use crate::library;
use crate::{HkRow, KeyBind, LibGroup, LibItem, LibraryWindow, SettingsWindow, ToolToggle, T};
use slint::winit_030::{winit, WinitWindowAccessor};
use slint::{ComponentHandle, Image, ModelRc, Rgba8Pixel, SharedPixelBuffer, SharedString, VecModel};
use std::cell::RefCell;
use std::rc::Rc;

fn sh(s: impl Into<SharedString>) -> SharedString {
    s.into()
}

pub fn show_fullscreen(w: &slint::Window, info: &OutInfo) {
    w.set_position(slint::PhysicalPosition::new((info.x as f32 * info.scale()) as i32, (info.y as f32 * info.scale()) as i32));
    w.set_size(slint::PhysicalSize::new(info.pw.max(1) as u32, info.ph.max(1) as u32));
    w.set_fullscreen(true);
}

pub fn raise(w: &slint::Window) {
    w.with_winit_window(|ww| {
        ww.set_window_level(winit::window::WindowLevel::AlwaysOnTop);
        ww.focus_window();
    });
}

pub fn place_corner(w: &slint::Window, lw: i32, lh: i32) {
    let o = crate::platform_outputs().into_iter().next().unwrap_or_default();
    let s = o.scale();
    w.set_size(slint::LogicalSize::new(lw as f32, lh as f32));
    let x = o.x as f32 + o.w as f32 - lw as f32 - 18.0;
    let y = o.y as f32 + o.h as f32 - lh as f32 - 64.0;
    w.set_position(slint::PhysicalPosition::new((x * s) as i32, (y * s) as i32));
    w.with_winit_window(|ww| ww.set_window_level(winit::window::WindowLevel::AlwaysOnTop));
}

// Windows places pins per monitor in physical px: win.rs place_pin
#[cfg(not(windows))]
pub fn place_at(w: &slint::Window, pos: (i32, i32), size: (i32, i32)) {
    w.set_size(slint::LogicalSize::new(size.0 as f32, size.1 as f32));
    w.set_position(slint::LogicalPosition::new(pos.0 as f32, pos.1 as f32));
    w.with_winit_window(|ww| ww.set_window_level(winit::window::WindowLevel::AlwaysOnTop));
}

// -------------------------------------------------------------------------------------------------------------
// The own title bar (Aimdot's titlebar.rs, trimmed): drag, double-click maximizes, min / max / close, edges resize

/// After the desktop took over a move / resize it never sends the button release: give Slint one so the title bar /
/// edge lets go. AFTER the current mouse event (inside it, Slint took the grab back once the callback returned, and
/// from then on any drag anywhere in the window moved / resized it again - the user, 20 Nov 2026).
fn end_grab<C: ComponentHandle + 'static>(c: &C) {
    #[cfg(not(windows))]
    {
        let weak = c.as_weak();
        slint::Timer::single_shot(std::time::Duration::ZERO, move || {
            if let Some(c) = weak.upgrade() {
                let w = c.window();
                w.dispatch_event(slint::platform::WindowEvent::PointerReleased {
                    position: slint::LogicalPosition::new(-10.0, -10.0),
                    button: slint::platform::PointerEventButton::Left,
                });
                w.dispatch_event(slint::platform::WindowEvent::PointerExited);
            }
        });
    }
    #[cfg(windows)]
    let _ = c;
}

fn win_act<C: ComponentHandle + 'static>(c: &C, what: &str, set_max: impl Fn(bool)) {
    let win = c.window();
    match what {
        "min" => win.set_minimized(true),
        "max" => {
            win.set_maximized(!win.is_maximized());
            set_max(win.is_maximized());
        }
        "close" => {
            let _ = slint::quit_event_loop();
        }
        "drag" => {
            win.with_winit_window(|ww| {
                let _ = ww.drag_window();
            });
            end_grab(c);
        }
        r if r.starts_with("resize:") => {
            use winit::window::ResizeDirection as D;
            let d = match &r[7..] {
                "n" => D::North,
                "s" => D::South,
                "e" => D::East,
                "w" => D::West,
                "ne" => D::NorthEast,
                "nw" => D::NorthWest,
                "se" => D::SouthEast,
                _ => D::SouthWest,
            };
            win.with_winit_window(|ww| {
                let _ = ww.drag_resize_window(d);
            });
            end_grab(c);
        }
        _ => {}
    }
}

/// Light colours for the normal windows (the dark ones are theme.slint's defaults)
fn apply_theme(t: &T, cfg: &Config) {
    let (a, a2, a3) = cfg.accent();
    t.set_accent(crate::config::color(a));
    t.set_accent2(crate::config::color(a2));
    t.set_g0(crate::config::color(a2));
    t.set_g1(crate::config::color(a));
    t.set_g2(crate::config::color(a3));
    // both palettes set every time: Light -> Dark in an open window must go dark again (found headless, 13 Nov 2026)
    let [bg, rail, panel, panel2, line, hover, text, muted, dim] = palette(cfg.dark());
    let col = |h: u32| slint::Color::from_rgb_u8((h >> 16) as u8, (h >> 8) as u8, h as u8);
    t.set_bg(col(bg));
    t.set_rail(col(rail));
    t.set_panel(col(panel));
    t.set_panel2(col(panel2));
    t.set_line(col(line));
    t.set_hover(col(hover));
    t.set_text(col(text));
    t.set_muted(col(muted));
    t.set_dim(col(dim));
}

/// bg, rail, panel, panel2, line, hover, text, muted, dim (dark = theme.slint's defaults)
fn palette(dark: bool) -> [u32; 9] {
    if dark {
        [0x0d0d10, 0x0a0a0c, 0x16161a, 0x1e1e23, 0x26262c, 0x34343c, 0xf5f5f7, 0xa0a0ab, 0x6b6b76]
    } else {
        [0xf5f5f2, 0xecece8, 0xffffff, 0xf0f0ed, 0xdcdcd6, 0xe4e4de, 0x111317, 0x5d636b, 0x9aa0a8]
    }
}

fn tell(cmd: &str) {
    if let Err(e) = crate::ipc::send(cmd) {
        eprintln!("lasso: Lasso isn't running ({e})");
    }
}

fn open_path(p: &std::path::Path) {
    #[cfg(target_os = "linux")]
    let _ = std::process::Command::new("xdg-open").arg(p).spawn();
    #[cfg(windows)]
    let _ = std::process::Command::new("explorer").arg(p).spawn();
}

// -------------------------------------------------------------------------------------------------------------
// Settings

fn fill_settings(w: &SettingsWindow, listening: &str) {
    let c = Config::load();
    w.set_version(sh(env!("CARGO_PKG_VERSION")));
    w.set_enter_does(match c.get("enter_does") {
        "save" => 1,
        "copysave" => 2,
        "link" => 3,
        _ => 0,
    });
    w.set_always_copy(c.on("always_copy"));
    w.set_save_dir(sh(crate::util::scrub_home(&c.save_dir().to_string_lossy())));
    w.set_name_pattern(sh(c.get("name_pattern")));
    w.set_name_example(sh(format!("{}.png", crate::util::name_from_pattern(c.get("name_pattern"), "Lasso"))));
    w.set_format(match c.get("format") {
        "jpg" | "jpeg" => 1,
        "webp" => 2,
        _ => 0,
    });
    w.set_fits_discord(c.on("fits_discord"));
    w.set_accent_id(sh(c.get("accent")));
    w.set_sounds(c.on("sounds"));
    w.set_theme(match c.get("theme") {
        "dark" => 1,
        "light" => 2,
        _ => 0,
    });
    w.set_autostart(c.on("autostart"));
    w.set_delay(c.num("delay") as i32);
    w.set_pointer(c.on("pointer"));
    w.set_full_mode(match c.get("full_mode") {
        "all" => 1,
        n => n.parse::<i32>().map(|n| (n + 1).clamp(2, 4)).unwrap_or(0),
    });
    w.set_thumb_after_full(c.on("thumb_after_full"));
    w.set_remember_tool(c.on("remember_tool"));
    w.set_magnifier(c.on("magnifier"));
    w.set_usage_ping(c.on("usage_ping"));
    w.set_library_keep(match c.get("library_keep") {
        "all" => 1,
        "none" => 2,
        _ => 0,
    });
    w.set_edits_as_copy(c.on("edits_as_copy"));
    // keys
    let km = c.keymap();
    let clashes = km.clashes();
    let binds: Vec<KeyBind> = keys::ACTIONS
        .iter()
        .map(|(id, name, _)| KeyBind {
            id: sh(*id),
            name: sh(*name),
            keys: sh(km.all(id)),
            clash: clashes.iter().any(|(_, a, b)| a == id || b == id),
        })
        .collect();
    w.set_binds(ModelRc::new(VecModel::from(binds)));
    w.set_clash_text(sh(if clashes.is_empty() {
        String::new()
    } else {
        clashes.iter().map(|(k, a, b)| format!("{} is on both \"{}\" and \"{}\"", keys::pretty(k), keys::action_name(a), keys::action_name(b))).collect::<Vec<_>>().join(" · ")
    }));
    w.set_listening(sh(listening));
    // hotkeys
    w.set_hotkeys_on(c.on("hotkeys"));
    let kde: std::collections::HashMap<String, String> = std::fs::read_to_string(crate::paths::config_dir().join("kde_keys.txt"))
        .unwrap_or_default()
        .lines()
        .filter_map(|l| l.split_once('=').map(|(a, b)| (a.to_string(), b.to_string())))
        .collect();
    let names = [("region", "Capture an area"), ("full", "Full screenshot"), ("window", "Capture a window"), ("repeat", "Repeat the last box"), ("restore", "Restore the last capture")];
    let hks: Vec<HkRow> = names
        .iter()
        .map(|(id, n)| HkRow { id: sh(*id), name: sh(*n), key: sh(c.get(&format!("hk.{id}"))), kde: sh(kde.get(*id).cloned().unwrap_or_default()) })
        .collect();
    w.set_hotkeys(ModelRc::new(VecModel::from(hks)));
    w.set_hk_note(sh(kde.get("error").map(|e| format!("Not working yet: {e}")).unwrap_or_default()));
    // library
    w.set_keep_count(c.num("keep_count") as f32);
    w.set_keep_days(c.num("keep_days") as f32);
    w.set_link_expiry(c.num("link_expiry") as i32);
    w.set_share_on(c.on("share_on"));
    w.set_share_email(sh(crate::share::signed_in().unwrap_or_default()));
    w.set_share_check(c.on("share_check"));
    w.set_share_both(c.get("share_copy") == "both");
    w.set_share_once(c.on("share_once"));
    let hidden: Vec<&str> = c.get("hidden_tools").split(',').map(|s| s.trim()).collect();
    let tt: Vec<ToolToggle> = c
        .tool_order()
        .iter()
        .filter(|t| **t != crate::shapes::Tool::Select)
        .map(|t| ToolToggle { id: sh(t.id()), name: sh(t.name()), on: !hidden.contains(&t.id()) })
        .collect();
    w.set_tool_toggles(ModelRc::new(VecModel::from(tt)));
    w.set_folder_note(sh(format!("Captures are kept in {} (never uploaded).", crate::util::scrub_home(&crate::paths::library_dir().to_string_lossy()))));
}

thread_local! {
    static SHARE_EMAIL: RefCell<String> = const { RefCell::new(String::new()) };
}

/// Sign-in for share links (email -> 6-digit code), sign-out. The network calls run on a worker thread.
fn share_act(w: &SettingsWindow, a: &str) {
    let ww = w.as_weak();
    let done = move |msg: String, code_step: Option<bool>| {
        let _ = slint::invoke_from_event_loop(move || {
            if let Some(w) = ww.upgrade() {
                w.set_share_busy(false);
                w.set_share_msg(sh(msg));
                if let Some(c) = code_step {
                    w.set_share_code_step(c);
                }
                fill_settings(&w, "");
            }
        });
    };
    w.set_share_busy(true);
    w.set_share_msg(sh("..."));
    let cfg = Config::load();
    if let Some(email) = a.strip_prefix("share-send:") {
        let email = email.trim().to_string();
        std::thread::spawn(move || match crate::share::sign_in_start(&cfg, &email) {
            Ok(clean) => {
                let m = format!("Code sent to {clean} - type it here");
                let _ = slint::invoke_from_event_loop(move || SHARE_EMAIL.with(|e| *e.borrow_mut() = clean));
                done(m, Some(true))
            }
            Err(e) => done(e, Some(false)),
        });
    } else if let Some(code) = a.strip_prefix("share-code:") {
        let code = code.trim().to_string();
        let email = SHARE_EMAIL.with(|e| e.borrow().clone());
        std::thread::spawn(move || match crate::share::sign_in_finish(&cfg, &email, &code) {
            Ok(()) => done("Signed in".into(), Some(false)),
            Err(e) => done(e, None),
        });
    } else if a == "share-out" {
        std::thread::spawn(move || {
            crate::share::sign_out(&cfg);
            done("Signed out".into(), Some(false))
        });
    } else {
        w.set_share_busy(false);
    }
}

fn set_key(k: &str, v: &str) {
    let mut c = Config::load();
    if let Some(tool) = k.strip_prefix("tool.") {
        let mut hidden: Vec<String> = c.get("hidden_tools").split(',').map(|s| s.trim().to_string()).filter(|s| !s.is_empty() && s != tool).collect();
        if v == "0" {
            hidden.push(tool.to_string());
        }
        c.set("hidden_tools", hidden.join(","));
    } else {
        c.set(k, v);
    }
    if k == "autostart" {
        #[cfg(target_os = "linux")]
        crate::desktop::set_autostart(v == "1");
        #[cfg(windows)]
        crate::win::set_autostart(v == "1");
    }
    c.save();
    tell("reload");
}

/// Settings > About > Updates (src/update.rs fills it; keep the Updater as long as the window)
fn settings_updater(w: &SettingsWindow) -> std::sync::Arc<crate::update::Updater> {
    let ww = w.as_weak();
    crate::update::start_window(std::sync::Arc::new(move |v: crate::update::View| {
        let _ = ww.upgrade_in_event_loop(move |w| {
            w.set_upd_available(v.available);
            w.set_upd_version(sh(v.version));
            w.set_upd_notes(sh(v.notes));
            w.set_upd_status(sh(v.status));
            w.set_upd_button(sh(v.button));
            w.set_upd_busy(v.busy);
            w.set_upd_can_check(v.can_check);
        });
    }))
}

pub fn settings_main() {
    let w = match SettingsWindow::new() {
        Ok(w) => w,
        Err(e) => return eprintln!("lasso: settings window: {e}"),
    };
    apply_theme(&w.global::<T>(), &Config::load());
    if let Ok(p) = std::env::var("LASSO_PAGE") {
        w.set_page(p.parse().unwrap_or(0));
    }
    fill_settings(&w, "");
    let upd = settings_updater(&w);
    let ww = w.as_weak();
    w.on_win_act(move |a| {
        if let Some(w) = ww.upgrade() {
            let w2 = w.as_weak();
            win_act(&w, &a, move |m| {
                if let Some(w) = w2.upgrade() {
                    w.set_win_max(m)
                }
            });
        }
    });
    let ww = w.as_weak();
    w.on_set(move |k, v| {
        set_key(&k, &v);
        if k.as_str() == "accent" || k.as_str() == "theme" {
            if let Some(w) = ww.upgrade() {
                apply_theme(&w.global::<T>(), &Config::load());
            }
        }
        if let Some(w) = ww.upgrade() {
            // the file name field: don't reset while the user types in it
            if k.as_str() != "name_pattern" {
                fill_settings(&w, "");
            } else {
                w.set_name_example(sh(format!("{}.png", crate::util::name_from_pattern(&v, "Lasso"))));
            }
        }
    });
    let ww = w.as_weak();
    w.on_listen(move |id| {
        if let Some(w) = ww.upgrade() {
            w.set_listening(id);
        }
    });
    let ww = w.as_weak();
    w.on_key_captured(move |text, ctrl, shift, alt| {
        let Some(w) = ww.upgrade() else { return false };
        let id = w.get_listening().to_string();
        let ev = keys::from_slint(&text, ctrl, alt, shift, false, None);
        if ev.key.starts_with("Shift") || ev.key.starts_with("Control") || ev.key.starts_with("Alt") {
            return true;
        }
        // Esc / Backspace / Delete = no key (the user, 20 Nov 2026: "can't press Esc on a keybind to unbind it")
        let clear = (ev.is("Escape") || ev.is("BackSpace") || ev.is("Delete")) && !ctrl && !alt;
        let combo = if clear {
            String::new()
        } else {
            let mut s = String::new();
            if ctrl {
                s += "Ctrl+";
            }
            if alt {
                s += "Alt+";
            }
            let single = ev.key.chars().count() == 1;
            if shift && (!single || ev.key.chars().all(|c| c.is_ascii_alphabetic())) {
                s += "Shift+";
            }
            s + &if single { ev.key.to_uppercase() } else { ev.key.clone() }
        };
        if let Some(hk) = id.strip_prefix("hk.") {
            set_key(&format!("hk.{hk}"), &combo);
        } else {
            // the new key replaces the FIRST one; extra keys (Alt+1 for the pen, Ctrl+Y for redo) stay.
            // Backspace removes them all.
            let km = Config::load().keymap();
            let mut list: Vec<String> = km.binds.get(&id).cloned().unwrap_or_default();
            if combo.is_empty() {
                list.clear();
            } else if list.is_empty() {
                list.push(combo.clone());
            } else {
                list[0] = keys::canon(&combo).unwrap_or(combo.clone());
            }
            set_key(&format!("key.{id}"), &list.join(", "));
        }
        fill_settings(&w, "");
        true
    });
    let ww = w.as_weak();
    w.on_act(move |a| {
        let Some(w) = ww.upgrade() else { return };
        match a.as_str() {
            "pick-folder" => {
                let dir = Config::load().save_dir();
                let w2 = w.as_weak();
                std::thread::spawn(move || {
                    if let Some(p) = crate::dialogs::pick(false, "Save folder", "<folder>", &dir) {
                        let p = p.to_string_lossy().to_string();
                        let _ = slint::invoke_from_event_loop(move || {
                            set_key("save_dir", &p);
                            if let Some(w) = w2.upgrade() {
                                fill_settings(&w, "");
                            }
                        });
                    }
                });
            }
            "open-folder" => {
                let d = Config::load().save_dir();
                let _ = std::fs::create_dir_all(&d);
                open_path(&d);
            }
            m if m.starts_with("tool-up:") || m.starts_with("tool-down:") || m == "tool-order-reset" => {
                let mut c = Config::load();
                let mut order: Vec<String> = c.tool_order().iter().map(|t| t.id().to_string()).collect();
                if let Some((dir, id)) = m.split_once(':') {
                    if let Some(i) = order.iter().position(|x| x == id) {
                        let j = if dir == "tool-up" { i.checked_sub(1) } else { Some(i + 1).filter(|j| *j < order.len()) };
                        if let Some(j) = j {
                            order.swap(i, j);
                        }
                    }
                    c.set("tool_order", order.join(","));
                } else {
                    c.set("tool_order", "");
                }
                c.save();
                tell("reload");
                fill_settings(&w, "");
            }
            "reset-keys" => {
                let mut c = Config::load();
                c.v.retain(|k, _| !k.starts_with("key."));
                c.save();
                tell("reload");
                fill_settings(&w, "");
            }
            "open-log" => open_path(&crate::paths::config_dir()),
            "upd-check" => upd.check(true, |_| {}),
            "upd-install" => upd.install(),
            a if a.starts_with("share-") => share_act(&w, a),
            "system-keys" => {
                // KDE: System Settings > Keyboard > Shortcuts; Windows: the "Print screen key opens screen capture" page
                #[cfg(target_os = "linux")]
                let _ = std::process::Command::new("systemsettings").arg("kcm_keys").spawn();
                #[cfg(windows)]
                let _ = std::process::Command::new("explorer").arg("ms-settings:easeofaccess-keyboard").spawn();
            }
            "capture" => {
                let _ = w.hide();
                let _ = slint::quit_event_loop();
                tell("region");
            }
            _ => {}
        }
    });
    let _ = w.run();
}

// -------------------------------------------------------------------------------------------------------------
// Library

struct Lib {
    items: Vec<library::Item>,
    sel: Vec<String>,
    thumbs: std::collections::HashMap<String, Image>,
    query: String,
    by_app: bool,
}

/// Does the item match every word of the search (text in the picture, app, file name)?
fn matches(it: &library::Item, q: &str) -> bool {
    let hay = format!("{} {} {}", it.text, it.app, it.path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default()).to_lowercase();
    q.split_whitespace().all(|w| hay.contains(&w.to_lowercase()))
}

fn key_of(it: &library::Item) -> String {
    if it.id.is_empty() {
        it.path.to_string_lossy().to_string()
    } else {
        it.id.clone()
    }
}

fn day_title(t: i64) -> String {
    let (y, m, d, _, _, _) = crate::util::local_from_unix(t);
    let now = crate::util::unix_now();
    let today = crate::util::local_from_unix(now);
    let yest = crate::util::local_from_unix(now - 86400);
    if (y, m, d) == (today.0, today.1, today.2) {
        "Today".into()
    } else if (y, m, d) == (yest.0, yest.1, yest.2) {
        "Yesterday".into()
    } else {
        const M: [&str; 12] = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];
        format!("{d} {} {y}", M[(m as usize).saturating_sub(1).min(11)])
    }
}

fn fill_library(w: &LibraryWindow, lib: &Lib) {
    let mut groups: Vec<LibGroup> = vec![];
    let mut cur: Option<(String, Vec<LibItem>)> = None;
    let shared = crate::share::shared_captures();
    let mut shown: Vec<&library::Item> = lib.items.iter().filter(|it| lib.query.trim().is_empty() || matches(it, &lib.query)).collect();
    let app_of = |it: &library::Item| if it.app.is_empty() { if it.editable { "Other".to_string() } else { "Saved pictures".to_string() } } else { it.app.clone() };
    if lib.by_app {
        // apps by name, newest first inside each
        shown.sort_by(|a, b| app_of(a).to_lowercase().cmp(&app_of(b).to_lowercase()).then(b.created.cmp(&a.created)));
    }
    let empty = shown.is_empty();
    for it in shown {
        let title = if lib.by_app { app_of(it) } else { day_title(it.created) };
        let (_, _, _, h, mi, _) = crate::util::local_from_unix(it.created);
        let k = key_of(it);
        let li = LibItem {
            key: sh(k.clone()),
            lib: it.editable,
            thumb: lib.thumbs.get(&k).cloned().unwrap_or_default(),
            time: sh(format!("{h:02}:{mi:02}")),
            info: sh(if it.editable { "capture · shapes editable".to_string() } else { it.path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default() }),
            shared: it.shared || shared.contains(&it.id),
            expiry: sh(crate::share::link_of(&it.id).map(|l| if l.once { format!("View once · {}", crate::share::expiry_label(l.days)) } else { crate::share::expiry_label(l.days) }).unwrap_or_default()),
            saved: !it.editable,
            sel: lib.sel.contains(&k),
        };
        match &mut cur {
            Some((t, v)) if *t == title => v.push(li),
            _ => {
                if let Some((t, v)) = cur.take() {
                    groups.push(LibGroup { title: sh(t), items: ModelRc::new(VecModel::from(v)) });
                }
                cur = Some((title, vec![li]));
            }
        }
    }
    if let Some((t, v)) = cur.take() {
        groups.push(LibGroup { title: sh(t), items: ModelRc::new(VecModel::from(v)) });
    }
    w.set_empty(empty);
    w.set_searching(!lib.query.trim().is_empty());
    w.set_group_by(lib.by_app as i32);
    w.set_count(sh(format!("{} picture{}", lib.items.len(), if lib.items.len() == 1 { "" } else { "s" })));
    w.set_selected(lib.sel.len() as i32);
    w.set_groups(ModelRc::new(VecModel::from(groups)));
}

thread_local! {
    static LIB: RefCell<Option<Rc<RefCell<Lib>>>> = const { RefCell::new(None) };
}

/// Changes the shown tiles in place (selection, thumbnails): rebuilding the model would make new tiles under the
/// mouse, and a double-click would never reach one tile twice.
fn patch_tiles(w: &LibraryWindow, lib: &Lib) {
    use slint::Model;
    let groups = w.get_groups();
    for gi in 0..groups.row_count() {
        let Some(g) = groups.row_data(gi) else { continue };
        for ii in 0..g.items.row_count() {
            let Some(mut it) = g.items.row_data(ii) else { continue };
            let k = it.key.to_string();
            let sel = lib.sel.contains(&k);
            let thumb = lib.thumbs.get(&k).cloned();
            let new_thumb = thumb.is_some() && it.thumb.size().width == 0;
            if it.sel != sel || new_thumb {
                it.sel = sel;
                if let Some(t) = thumb {
                    it.thumb = t;
                }
                g.items.set_row_data(ii, it);
            }
        }
    }
    w.set_selected(lib.sel.len() as i32);
}

/// Thumbnails in the background (256 px; plain files get a cached one in the library's .thumbs folder)
fn load_thumbs(w: &LibraryWindow, items: Vec<(String, std::path::PathBuf, bool)>) {
    if items.is_empty() {
        return;
    }
    let ww = w.as_weak();
    std::thread::spawn(move || {
        let cache = crate::paths::library_dir().join(".thumbs");
        let _ = std::fs::create_dir_all(&cache);
        let mut batch: Vec<(String, SharedPixelBuffer<Rgba8Pixel>)> = vec![];
        let n = items.len();
        for (i, (key, path, lib)) in items.into_iter().enumerate() {
            let pic = if lib {
                std::fs::read(&path).ok().and_then(|b| crate::export::decode(&b).ok())
            } else {
                let stamp = std::fs::metadata(&path).ok().map(|m| (m.len(), m.modified().ok().and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok()).map(|d| d.as_secs()).unwrap_or(0))).unwrap_or((0, 0));
                let cp = cache.join(format!("{:x}.png", fnv(&format!("{key}|{}|{}", stamp.0, stamp.1))));
                match std::fs::read(&cp).ok().and_then(|b| crate::export::decode(&b).ok()) {
                    Some(p) => Some(p),
                    None => {
                        let p = std::fs::read(&path).ok().and_then(|b| crate::export::decode(&b).ok()).map(|p| crate::export::thumbnail(&p, 256));
                        if let Some(p) = &p {
                            let _ = std::fs::write(&cp, crate::export::png(p, true));
                        }
                        p
                    }
                }
            };
            if let Some(p) = pic {
                batch.push((key, SharedPixelBuffer::<Rgba8Pixel>::clone_from_slice(&p.px, p.w, p.h)));
            }
            if batch.len() >= 12 || i + 1 == n {
                let b = std::mem::take(&mut batch);
                let ww = ww.clone();
                let _ = slint::invoke_from_event_loop(move || {
                    LIB.with(|l| {
                        if let Some(l) = l.borrow().as_ref() {
                            let mut l = l.borrow_mut();
                            for (k, buf) in b {
                                l.thumbs.insert(k, Image::from_rgba8_premultiplied(buf));
                            }
                            if let Some(w) = ww.upgrade() {
                                patch_tiles(&w, &l);
                            }
                        }
                    })
                });
            }
        }
    });
}

fn fnv(s: &str) -> u64 {
    let mut h: u64 = 0xcbf29ce484222325;
    for b in s.bytes() {
        h ^= b as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    h
}

/// The thumbnail cache keeps only what the library shows now
fn prune_thumb_cache(items: &[library::Item]) {
    let keep: std::collections::HashSet<String> = items
        .iter()
        .filter(|it| !it.editable)
        .map(|it| {
            let k = key_of(it);
            let stamp = std::fs::metadata(&it.path).ok().map(|m| (m.len(), m.modified().ok().and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok()).map(|d| d.as_secs()).unwrap_or(0))).unwrap_or((0, 0));
            format!("{:x}.png", fnv(&format!("{k}|{}|{}", stamp.0, stamp.1)))
        })
        .collect();
    if let Ok(rd) = std::fs::read_dir(crate::paths::library_dir().join(".thumbs")) {
        for e in rd.flatten() {
            if !keep.contains(&e.file_name().to_string_lossy().to_string()) {
                let _ = std::fs::remove_file(e.path());
            }
        }
    }
}

fn refresh(w: &LibraryWindow, lib: &Rc<RefCell<Lib>>) {
    let items = library::list(&Config::load().save_dir());
    prune_thumb_cache(&items);
    let todo: Vec<(String, std::path::PathBuf, bool)> = {
        let l = lib.borrow();
        items.iter().filter(|it| !l.thumbs.contains_key(&key_of(it))).map(|it| (key_of(it), it.thumb.clone(), it.editable)).collect()
    };
    {
        let mut l = lib.borrow_mut();
        l.items = items;
        let keys: Vec<String> = l.items.iter().map(key_of).collect();
        l.sel.retain(|k| keys.contains(k));
        fill_library(w, &l);
    }
    load_thumbs(w, todo);
}

/// A library capture as a finished picture file (its saved file if it has one, else a fresh export)
fn finished_file(key: &str, lib: bool) -> Option<std::path::PathBuf> {
    if !lib {
        return Some(std::path::PathBuf::from(key));
    }
    let st = library::load_state(key)?;
    if let Some(s) = st.saved.last().filter(|s| std::path::Path::new(s).exists()) {
        return Some(std::path::PathBuf::from(s));
    }
    let pic = library::finished(key).ok()?;
    let p = library::dir(key).join("export.png");
    crate::export::write_new(&p, &crate::export::png(&pic, false)).ok()?;
    Some(p)
}

/// To the trash: a capture together with the files it was saved as (else they'd come back as "saved pictures"
/// tiles and the delete would look like it failed). Never the user's OWN file a capture was opened from, even when
/// it was edited in place (review D3). Returns how many things went. A shared capture: `delete_keys` stops its link first.
fn delete_item(key: &str, is_lib: bool) -> Result<usize, String> {
    if !is_lib {
        return library::trash(std::path::Path::new(key)).map(|_| 1);
    }
    let n = library::delete_capture(key, &[])?;
    // the running Lasso: no late write brings it back, no Restore of it (review D4)
    tell(&format!("forget:{key}"));
    Ok(n.max(1))
}

/// Delete from the Library (one or many). A shared capture's public link is stopped FIRST (network, own thread) and
/// the capture goes only when that worked - else a "never" link would stay public with no way to stop it (review D1).
fn delete_keys(w: &LibraryWindow, lib: &Rc<RefCell<Lib>>, keys: Vec<(String, bool)>) {
    let (shared, plain): (Vec<_>, Vec<_>) = keys.into_iter().partition(|(k, is_lib)| *is_lib && crate::share::link_of(k).is_some());
    let (mut n, mut with_saved, mut err) = (0, false, None);
    for (k, is_lib) in &plain {
        match delete_item(k, *is_lib) {
            Ok(c) => {
                n += 1;
                with_saved |= c > 1;
            }
            Err(e) => err = Some(e),
        }
    }
    if !plain.is_empty() {
        lib_toast(
            w,
            &match (n, err) {
                (0, Some(e)) => e,
                (1, _) if with_saved => "Moved to the trash (with its saved picture)".into(),
                (1, _) => "Moved to the trash".into(),
                (n, _) => format!("Moved {n} to the trash"),
            },
        );
        refresh(w, lib);
    }
    if shared.is_empty() {
        return;
    }
    lib_toast(w, "Stopping its link first...");
    std::thread::spawn(move || {
        let cfg = Config::load();
        let res: Vec<(String, Result<(), String>)> = shared.into_iter().map(|(k, _)| {
            let r = crate::share::stop(&cfg, &k);
            (k, r)
        }).collect();
        let _ = slint::invoke_from_event_loop(move || {
            let mut fail = None;
            let mut done = 0;
            for (k, r) in res {
                match r {
                    Ok(()) => done += delete_item(&k, true).map(|_| 1).unwrap_or(0),
                    Err(e) => fail = Some(e),
                }
            }
            LIB_WIN.with(|w| {
                if let Some(w) = w.borrow().as_ref().and_then(|w| w.upgrade()) {
                    match fail {
                        Some(e) => lib_toast(&w, &format!("Not deleted: its link couldn't be stopped ({e}) - sign in, then try again")),
                        None => lib_toast(&w, &format!("Link stopped, moved {done} to the trash")),
                    }
                    LIB.with(|l| {
                        if let Some(l) = l.borrow().as_ref() {
                            refresh(&w, l);
                        }
                    });
                }
            });
        });
    });
}

fn lib_toast(w: &LibraryWindow, t: &str) {
    w.set_toast(sh(t));
    let ww = w.as_weak();
    slint::Timer::single_shot(std::time::Duration::from_millis(2200), move || {
        if let Some(w) = ww.upgrade() {
            w.set_toast(sh(""));
        }
    });
}

thread_local! {
    static LIB_WIN: RefCell<Option<slint::Weak<LibraryWindow>>> = const { RefCell::new(None) };
}

/// A drag with files is over the library (Wayland: wl_dnd.rs; Windows / X11: winit)
pub fn lib_drag_over(on: bool) {
    LIB_WIN.with(|w| {
        if let Some(w) = w.borrow().as_ref().and_then(|w| w.upgrade()) {
            w.set_drag_over(on);
        }
    });
}

/// Pictures dropped on the library: each one becomes a library item (the file itself stays where it is)
pub fn lib_dropped(paths: Vec<std::path::PathBuf>) {
    std::thread::spawn(move || {
        let mut n = 0;
        for p in &paths {
            let Ok(pic) = std::fs::read(p).map_err(|e| e.to_string()).and_then(|b| crate::export::decode(&b)) else { continue };
            let id = library::new_id() + &format!("-{n}");
            let st = library::State {
                created: crate::util::unix_now(),
                boxr: Some(crate::shapes::R::new(0.0, 0.0, pic.w as f32, pic.h as f32)),
                mode: Some(crate::editor::Mode::Screen),
                source: p.to_string_lossy().to_string(),
                ..Default::default()
            };
            if library::store(&id, &pic, true, &st, &Default::default(), Some(&pic)).is_ok() {
                // like Ctrl+O: the file's own bytes stay in the library ("Back to original" after a Replace-the-file edit
                // gives exactly them back, EXIF and all - not a re-encoded JPEG)
                library::keep_original(&id, p);
                n += 1;
            }
        }
        let _ = slint::invoke_from_event_loop(move || {
            lib_drag_over(false);
            LIB_WIN.with(|w| {
                if let Some(w) = w.borrow().as_ref().and_then(|w| w.upgrade()) {
                    LIB.with(|l| {
                        if let Some(l) = l.borrow().as_ref() {
                            refresh(&w, l);
                        }
                    });
                    lib_toast(&w, &if n == 0 { "Only pictures from this PC can be added".to_string() } else { format!("Added {n} picture{}", if n == 1 { "" } else { "s" }) });
                }
            });
        });
    });
}

pub fn library_main() {
    let w = match LibraryWindow::new() {
        Ok(w) => w,
        Err(e) => return eprintln!("lasso: library window: {e}"),
    };
    apply_theme(&w.global::<T>(), &Config::load());
    let lib = Rc::new(RefCell::new(Lib { items: vec![], sel: vec![], thumbs: Default::default(), query: String::new(), by_app: Config::load().get("library_group") == "app" }));
    LIB.with(|l| *l.borrow_mut() = Some(lib.clone()));
    LIB_WIN.with(|l| *l.borrow_mut() = Some(w.as_weak()));
    // drag and drop: winit has it on Windows / X11, Wayland needs wl_dnd.rs (started on the first window event)
    {
        use slint::winit_030::{winit::event::WindowEvent, EventResult};
        w.window().on_winit_window_event(|_win, ev| {
            #[cfg(target_os = "linux")]
            crate::wl_dnd::start(_win);
            match ev {
                WindowEvent::HoveredFile(_) => lib_drag_over(true),
                WindowEvent::HoveredFileCancelled => lib_drag_over(false),
                WindowEvent::DroppedFile(p) => lib_dropped(vec![p.clone()]),
                _ => {}
            }
            EventResult::Propagate
        });
    }
    refresh(&w, &lib);
    let ww = w.as_weak();
    w.on_win_act(move |a| {
        if let Some(w) = ww.upgrade() {
            let w2 = w.as_weak();
            win_act(&w, &a, move |m| {
                if let Some(w) = w2.upgrade() {
                    w.set_win_max(m)
                }
            });
        }
    });
    let (ww, l2) = (w.as_weak(), lib.clone());
    w.on_search(move |q| {
        if let Some(w) = ww.upgrade() {
            l2.borrow_mut().query = q.to_string();
            fill_library(&w, &l2.borrow());
        }
    });
    let (ww, l2) = (w.as_weak(), lib.clone());
    w.on_group(move |v| {
        if let Some(w) = ww.upgrade() {
            l2.borrow_mut().by_app = v == 1;
            let mut c = Config::load();
            c.set("library_group", if v == 1 { "app" } else { "day" });
            c.save();
            fill_library(&w, &l2.borrow());
        }
    });
    let (ww, l2) = (w.as_weak(), lib.clone());
    w.on_pick(move |key, add| {
        let Some(w) = ww.upgrade() else { return };
        let mut l = l2.borrow_mut();
        let k = key.to_string();
        if add {
            if let Some(i) = l.sel.iter().position(|s| *s == k) {
                l.sel.remove(i);
            } else {
                l.sel.push(k);
            }
        } else if l.sel == [k.clone()] {
            l.sel.clear();
        } else {
            l.sel = vec![k];
        }
        patch_tiles(&w, &l);
    });
    let (ww, l2) = (w.as_weak(), lib.clone());
    w.on_act(move |a, key| {
        let Some(w) = ww.upgrade() else { return };
        let key = key.to_string();
        let is_lib = l2.borrow().items.iter().any(|it| it.id == key && !key.is_empty());
        match a.as_str() {
            "edit" => tell(&if is_lib { format!("edit:{key}") } else { format!("open:{key}") }),
            "copy" => {
                tell(&if is_lib { format!("copy-lib:{key}") } else { format!("copy-file:{key}") });
                lib_toast(&w, "Copied");
            }
            "pin" => tell(&if is_lib { format!("pin-lib:{key}") } else { format!("pin-file:{key}") }),
            "saveas" => {
                let k2 = key.clone();
                let c = Config::load();
                let dir = c.save_dir();
                let name = crate::util::name_from_pattern(c.get("name_pattern"), "Lasso");
                std::thread::spawn(move || {
                    let Some(src) = finished_file(&k2, is_lib) else { return };
                    let src_ext = src.extension().map(|e| e.to_string_lossy().to_lowercase()).unwrap_or_else(|| "png".into());
                    if let Some(dst) = crate::dialogs::pick(true, "Save the picture", &format!("{name}.{src_ext}"), &dir) {
                        let ext = |p: &std::path::Path| p.extension().map(|e| e.to_string_lossy().to_lowercase().replace("jpeg", "jpg")).unwrap_or_default();
                        let Ok(bytes) = std::fs::read(&src) else { return };
                        if ext(&src) == ext(&dst) || !["png", "jpg", "webp"].contains(&ext(&dst).as_str()) {
                            // the same kind (or a name without a kind Lasso writes): the file's own bytes, its own
                            // extension; a path the dialog didn't confirm gets a free name (review D10)
                            let _ = crate::export::write_new(&crate::export::save_as_path(&dst, &ext(&src)), &bytes);
                        } else if let Ok(p) = crate::export::decode(&bytes) {
                            // another kind than the file (photo.png -> photo.jpg): made new in that kind
                            let enc = crate::export::encode(&p, &ext(&dst), false, false);
                            let _ = crate::export::write_new(&crate::export::save_as_path(&dst, enc.ext), &enc.bytes);
                        }
                    }
                });
            }
            "folder" => {
                let p = if is_lib {
                    library::load_state(&key).and_then(|s| s.saved.last().map(std::path::PathBuf::from)).and_then(|p| p.parent().map(|x| x.to_path_buf())).unwrap_or_else(|| library::dir(&key))
                } else {
                    std::path::Path::new(&key).parent().map(|p| p.to_path_buf()).unwrap_or_default()
                };
                open_path(&p);
            }
            "delete" => delete_keys(&w, &l2, vec![(key.clone(), is_lib)]),
            "copy-link" => match crate::share::link_of(&key) {
                Some(l) => {
                    tell(&format!("copy-text:{}", l.url));
                    lib_toast(&w, "Link copied");
                }
                None => lib_toast(&w, "Not shared"),
            },
            "stop-share" | "expiry" => {
                let cfg = Config::load();
                let k = key.clone();
                let what = a.to_string();
                let ww = w.as_weak();
                let next = crate::share::link_of(&key).map(|l| {
                    let ch = crate::share::EXPIRY_CHOICES;
                    let i = ch.iter().position(|d| *d == l.days).map(|i| (i + 1) % ch.len()).unwrap_or(0);
                    ch[i]
                });
                lib_toast(&w, if what == "stop-share" { "Stopping the link..." } else { "Changing the link..." });
                std::thread::spawn(move || {
                    let r = if what == "stop-share" {
                        crate::share::stop(&cfg, &k).map(|_| "Stopped sharing - the link is dead".to_string())
                    } else {
                        crate::share::set_expiry(&cfg, &k, next.unwrap_or(30)).map(|l| format!("The link now expires: {}", crate::share::expiry_label(l.days)))
                    };
                    let _ = slint::invoke_from_event_loop(move || {
                        if let Some(w) = ww.upgrade() {
                            lib_toast(&w, &r.unwrap_or_else(|e| e));
                            LIB.with(|l| {
                                if let Some(l) = l.borrow().as_ref() {
                                    fill_library(&w, &l.borrow());
                                }
                            });
                        }
                    });
                });
            }
            "original" => match library::back_to_original(&key) {
                Ok(()) => lib_toast(&w, "Back to the original"),
                Err(e) => lib_toast(&w, &e),
            },
            _ => {}
        }
    });
    let (ww, l2) = (w.as_weak(), lib.clone());
    w.on_bulk(move |a| {
        let Some(w) = ww.upgrade() else { return };
        match a.as_str() {
            "open" => tell("open-dialog"),
            "capture" => {
                w.window().set_minimized(true);
                tell("region");
            }
            "clear" => {
                l2.borrow_mut().sel.clear();
                patch_tiles(&w, &l2.borrow());
            }
            "copy" => {
                let sel = l2.borrow().sel.clone();
                let files: Vec<String> = sel
                    .iter()
                    .filter_map(|k| {
                        let lib = l2.borrow().items.iter().any(|it| it.id == *k);
                        finished_file(k, lib).map(|p| p.to_string_lossy().to_string())
                    })
                    .collect();
                tell(&format!("copy-files:{}", files.join("|")));
                lib_toast(&w, &format!("Copied {} files - paste them in a chat or a folder", files.len()));
            }
            "delete" => {
                let sel = l2.borrow().sel.clone();
                let keys: Vec<(String, bool)> = sel.iter().map(|k| (k.clone(), l2.borrow().items.iter().any(|it| it.id == *k))).collect();
                delete_keys(&w, &l2, keys);
            }
            _ => {}
        }
    });
    // new captures show up while the window is open (one folder listing every 3 s)
    let (ww, l2) = (w.as_weak(), lib.clone());
    let timer = slint::Timer::default();
    let last = Rc::new(RefCell::new(usize::MAX));
    timer.start(slint::TimerMode::Repeated, std::time::Duration::from_secs(3), move || {
        let n = std::fs::read_dir(crate::paths::library_dir()).map(|r| r.count()).unwrap_or(0) + std::fs::read_dir(Config::load().save_dir()).map(|r| r.count()).unwrap_or(0);
        let mut l = last.borrow_mut();
        if *l != usize::MAX && *l != n {
            if let Some(w) = ww.upgrade() {
                refresh(&w, &l2);
            }
        }
        *l = n;
    });
    let _ = w.run();
}

#[cfg(test)]
mod tests {
    /// Light -> Dark in an open Settings window: the dark palette must be set back, and match theme.slint's defaults
    #[test]
    fn dark_palette_matches_theme_slint() {
        let src = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/ui/theme.slint")).unwrap();
        let names = ["bg", "rail", "panel", "panel2", "line", "hover", "text", "muted", "dim"];
        for (n, v) in names.iter().zip(super::palette(true)) {
            let line = src.lines().find(|l| l.contains(&format!("property <color> {n}:"))).unwrap();
            assert!(line.contains(&format!("#{v:06x}")), "{n}: {line}");
        }
        assert_ne!(super::palette(true), super::palette(false));
    }
}
