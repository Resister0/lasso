//! Normal (winit) windows: the Settings / Library processes (each its own short-lived process, destroyed when
//! closed), and the frozen screen / popups / pins on platforms without layer-shell (Windows, GNOME).

use crate::app::OutInfo;
use crate::config::Config;
use crate::keys;
use crate::library;
use crate::{FormField, HkRow, KeyBind, LassoWindow, LibGroup, LibItem, PlaceRow, ToolToggle, LL, SS, T};
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
/// The theme's colours into the window (every colour every time: Light -> Dark in an open window must go dark again)
fn apply_theme(t: &T, cfg: &Config) {
    let p = cfg.palette();
    let col = crate::config::color;
    t.set_accent(col(p.accent));
    t.set_accent2(col(p.accent2));
    t.set_on_accent(col(p.on_accent));
    t.set_g0(col(p.grad[0]));
    t.set_g1(col(p.grad[1]));
    t.set_g2(col(p.grad[2]));
    t.set_bg(col(p.bg));
    t.set_rail(col(p.rail));
    t.set_panel(col(p.panel));
    t.set_panel2(col(p.panel2));
    t.set_line(col(p.line));
    t.set_hover(col(p.hover));
    t.set_text(col(p.text));
    t.set_muted(col(p.muted));
    t.set_dim(col(p.dim));
}

/// The theme cards for Settings > Appearance > Look (Echo's / Aimdot's grid)
fn theme_cards(c: &Config) -> ModelRc<crate::ThemeCard> {
    let cur = c.theme_id();
    let dark = crate::platform_theme_dark();
    let col = crate::config::color;
    let v: Vec<crate::ThemeCard> = crate::theme::list()
        .into_iter()
        .map(|(id, name)| {
            let p = crate::theme::palette(id, c.get("ui_accent"), c.get("ui_bg"), || !dark);
            crate::ThemeCard { id: sh(id), name: sh(name), on: id == cur, bg: col(p.bg), rail: col(p.rail), panel: col(p.panel), accent: col(p.accent), text: col(p.text) }
        })
        .collect();
    ModelRc::new(VecModel::from(v))
}

/// The window says it's Lasso (Wayland app_id = the .desktop file's name): KDE's alt-tab / task bar then show Lasso's
/// icon and name, not a blank one (21 Nov 2026: "there's no Lasso icon in alt tab")
fn set_app_id() {
    #[cfg(target_os = "linux")]
    let _ = slint::set_xdg_app_id(crate::desktop::APP_ID);
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

fn fill_settings(w: &LassoWindow, listening: &str) {
    let c = Config::load();
    w.global::<SS>().set_version(sh(env!("CARGO_PKG_VERSION")));
    w.global::<SS>().set_win(cfg!(windows));
    w.global::<SS>().set_enter_does(match c.get("enter_does") {
        "save" => 1,
        "copysave" => 2,
        "link" => 3,
        _ => 0,
    });
    w.global::<SS>().set_always_copy(c.on("always_copy"));
    w.global::<SS>().set_save_dir(sh(crate::util::scrub_home(&c.save_dir().to_string_lossy())));
    w.global::<SS>().set_name_pattern(sh(c.get("name_pattern")));
    w.global::<SS>().set_name_example(sh(format!("{}.png", crate::util::name_from_pattern(c.get("name_pattern"), "Lasso"))));
    w.global::<SS>().set_format(match c.get("format") {
        "jpg" | "jpeg" => 1,
        "webp" => 2,
        _ => 0,
    });
    w.global::<SS>().set_fits_discord(c.on("fits_discord"));
    w.global::<SS>().set_echo_shots(c.on("echo_shots"));
    w.global::<SS>().set_echo_dir(sh(crate::echo_link::shots_dir().map(|d| crate::util::scrub_home(&d.to_string_lossy())).unwrap_or_default()));
    w.global::<SS>().set_theme_cards(theme_cards(&c));
    w.global::<SS>().set_theme_id(sh(c.theme_id()));
    w.global::<SS>().set_ui_accent(sh(c.get("ui_accent")));
    w.global::<SS>().set_ui_bg(sh(c.get("ui_bg")));
    w.global::<SS>().set_sounds(c.on("sounds"));
    w.global::<SS>().set_steam_shots(c.on("steam_shots"));
    w.global::<SS>().set_steam_note(sh(match crate::gamepics::steam_roots().first() {
        Some(r) => format!("found Steam: {} - shown by game", crate::util::scrub_home(&r.to_string_lossy())),
        None => "Steam isn't installed on this PC (or not where it usually is)".to_string(),
    }));
    let dirs: Vec<SharedString> = crate::gamepics::dirs(&c).iter().map(|d| sh(crate::util::scrub_home(&d.to_string_lossy()))).collect();
    w.global::<SS>().set_game_dirs(ModelRc::new(VecModel::from(dirs)));
    w.global::<SS>().set_backup_captures(c.on("backup_captures"));
    w.global::<SS>().set_backup_games(c.on("backup_games"));
    // the worker's line only while a switch is on (an old "Paused: ..." means nothing once it's off)
    let bon = c.on("backup_captures") || c.on("backup_games");
    let bst = crate::share::backup::status();
    w.global::<SS>().set_backup_status(sh(if !bon { String::new() } else if bst.is_empty() { "Starts in a moment...".to_string() } else { bst }));
    w.global::<SS>().set_backup_count(crate::share::backup::count() as i32);
    w.global::<SS>().set_backup_ec(c.on("backup_ec"));
    fill_places(w, &c);
    let kinds: Vec<ToolToggle> = crate::clouds::KINDS.iter().map(|(id, n)| ToolToggle { id: sh(id.to_string()), name: sh(n.to_string()), on: false }).collect();
    w.global::<SS>().set_place_kinds(ModelRc::new(VecModel::from(kinds)));
    w.global::<SS>().set_autostart(c.on("autostart"));
    w.global::<SS>().set_delay(c.num("delay") as i32);
    w.global::<SS>().set_pointer(c.on("pointer"));
    w.global::<SS>().set_full_mode(match c.get("full_mode") {
        "all" => 1,
        n => n.parse::<i32>().map(|n| (n + 1).clamp(2, 4)).unwrap_or(0),
    });
    w.global::<SS>().set_thumb_after_full(c.on("thumb_after_full"));
    w.global::<SS>().set_remember_tool(c.on("remember_tool"));
    w.global::<SS>().set_magnifier(c.on("magnifier"));
    let on = crate::keys::bar_list(c.get("bar"));
    let chip = |id: &str| ToolToggle { id: sh(id), name: sh(crate::keys::bar_text(id)), on: true };
    w.global::<SS>().set_bar_on(ModelRc::new(VecModel::from(on.iter().map(|id| chip(id)).collect::<Vec<_>>())));
    w.global::<SS>().set_bar_off(ModelRc::new(VecModel::from(
        crate::keys::BAR_ACTIONS.iter().filter(|(id, _)| !on.contains(id)).map(|(id, _)| ToolToggle { on: false, ..chip(id) }).collect::<Vec<_>>(),
    )));
    w.global::<SS>().set_usage_ping(c.on("usage_ping"));
    w.global::<SS>().set_library_keep(match c.get("library_keep") {
        "all" => 1,
        "none" => 2,
        _ => 0,
    });
    w.global::<SS>().set_edits_as_copy(c.on("edits_as_copy"));
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
    w.global::<SS>().set_binds(ModelRc::new(VecModel::from(binds)));
    w.global::<SS>().set_clash_text(sh(if clashes.is_empty() {
        String::new()
    } else {
        clashes.iter().map(|(k, a, b)| format!("{} is on both \"{}\" and \"{}\"", keys::pretty(k), keys::action_name(a), keys::action_name(b))).collect::<Vec<_>>().join(" · ")
    }));
    w.global::<SS>().set_listening(sh(listening));
    // hotkeys
    w.global::<SS>().set_hotkeys_on(c.on("hotkeys"));
    let kde: std::collections::HashMap<String, String> = std::fs::read_to_string(crate::paths::config_dir().join("kde_keys.txt"))
        .unwrap_or_default()
        .lines()
        .filter_map(|l| l.split_once('=').map(|(a, b)| (a.to_string(), b.to_string())))
        .collect();
    let names = [("region", "Capture an area"), ("full", "Full screenshot"), ("window", "Capture a window"), ("repeat", "Repeat the last box"), ("restore", "Restore the last capture"), ("delete", "Delete the last capture")];
    let hks: Vec<HkRow> = names
        .iter()
        .map(|(id, n)| HkRow { id: sh(*id), name: sh(*n), key: sh(c.get(&format!("hk.{id}"))), kde: sh(kde.get(*id).cloned().unwrap_or_default()) })
        .collect();
    w.global::<SS>().set_hotkeys(ModelRc::new(VecModel::from(hks)));
    w.global::<SS>().set_hk_note(sh(kde.get("error").map(|e| format!("Not working yet: {e}")).unwrap_or_default()));
    // library
    w.global::<SS>().set_keep_count(c.num("keep_count") as f32);
    w.global::<SS>().set_keep_days(c.num("keep_days") as f32);
    w.global::<SS>().set_link_expiry(c.num("link_expiry") as i32);
    w.global::<SS>().set_share_on(c.on("share_on"));
    w.global::<SS>().set_share_email(sh(crate::share::signed_in().unwrap_or_default()));
    w.global::<SS>().set_share_check(c.on("share_check"));
    w.global::<SS>().set_share_both(c.get("share_copy") == "both");
    w.global::<SS>().set_share_once(c.on("share_once"));
    let hidden: Vec<&str> = c.get("hidden_tools").split(',').map(|s| s.trim()).collect();
    let tt: Vec<ToolToggle> = c
        .tool_order()
        .iter()
        .filter(|t| **t != crate::shapes::Tool::Select)
        .map(|t| ToolToggle { id: sh(t.id()), name: sh(t.name()), on: !hidden.contains(&t.id()) })
        .collect();
    w.global::<SS>().set_tool_toggles(ModelRc::new(VecModel::from(tt)));
    w.global::<SS>().set_folder_note(sh(format!("Captures are kept in {} - only uploaded when you share one or switch on the backup below.", crate::util::scrub_home(&crate::paths::library_dir().to_string_lossy()))));
}

thread_local! {
    static SHARE_EMAIL: RefCell<String> = const { RefCell::new(String::new()) };
}

/// Sign-in for share links (email -> 6-digit code), sign-out. The network calls run on a worker thread.
fn share_act(w: &LassoWindow, a: &str) {
    let ww = w.as_weak();
    let done = move |msg: String, code_step: Option<bool>| {
        let _ = slint::invoke_from_event_loop(move || {
            if let Some(w) = ww.upgrade() {
                w.global::<SS>().set_share_busy(false);
                w.global::<SS>().set_share_msg(sh(msg));
                if let Some(c) = code_step {
                    // the code step / signed in / out: the box is empty again (the user, 22 Nov 2026: "code sent,
                    // input it... where?" - it still held the email)
                    if c != w.global::<SS>().get_share_code_step() || !c {
                        w.global::<SS>().set_share_input(sh(""));
                    }
                    w.global::<SS>().set_share_code_step(c);
                }
                fill_settings(&w, "");
            }
        });
    };
    w.global::<SS>().set_share_busy(true);
    w.global::<SS>().set_share_msg(sh("..."));
    let cfg = Config::load();
    if let Some(email) = a.strip_prefix("share-send:") {
        let email = email.trim().to_string();
        std::thread::spawn(move || match crate::share::sign_in_start(&cfg, &email) {
            Ok(clean) => {
                let m = format!("Code sent to {clean} - type the 6 digits in the box above");
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
        w.global::<SS>().set_share_busy(false);
    }
}

/// where "Add a folder" starts: the home folder
fn dirs_home() -> std::path::PathBuf {
    std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" }).map(std::path::PathBuf::from).unwrap_or_default()
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
    // a theme picked: the old accent setting (before the theme cards) must not map it to another theme
    if k == "theme" {
        c.set("accent", "");
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

/// Settings > Updates (src/update.rs fills it; keep the Updater as long as the window)
fn settings_updater(w: &LassoWindow) -> std::sync::Arc<crate::update::Updater> {
    let ww = w.as_weak();
    crate::update::start_window(std::sync::Arc::new(move |v: crate::update::View| {
        let _ = ww.upgrade_in_event_loop(move |w| {
            w.global::<SS>().set_upd_available(v.available);
            w.global::<SS>().set_upd_version(sh(v.version));
            w.global::<SS>().set_upd_notes(sh(v.notes));
            w.global::<SS>().set_upd_status(sh(v.status));
            w.global::<SS>().set_upd_button(sh(v.button));
            w.global::<SS>().set_upd_busy(v.busy);
            w.global::<SS>().set_upd_can_check(v.can_check);
            w.global::<SS>().set_upd_channel(v.channel);
        });
    }))
}

/// Settings and Library in one window (21 Nov 2026), its own short-lived process: `view` 0 = Settings, 1 = Library.
/// `--settings-window` / `--library-window` both land here.
pub fn window_main(view: i32) {
    set_app_id();
    let w = match LassoWindow::new() {
        Ok(w) => w,
        Err(e) => return eprintln!("lasso: window: {e}"),
    };
    apply_theme(&w.global::<T>(), &Config::load());
    w.set_view(view);
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
    // the settings search (Ctrl+F): every word somewhere in the text, any case (Echo's / Aimdot's Find)
    w.global::<crate::Find>().on_matches(|q, text| {
        let t = text.to_lowercase();
        q.to_lowercase().split_whitespace().all(|w| t.contains(w))
    });
    setup_settings(&w);
    let _timer = setup_library(&w);
    let _ = w.run();
}

fn setup_settings(w: &LassoWindow) {
    // tests: open at a section - its name ("appearance") or an old page number (2 = editor keys)
    if let Ok(p) = std::env::var("LASSO_PAGE") {
        let sec = match p.as_str() {
            "0" | "1" => "capture",
            "2" => "keys",
            "3" => "hotkeys",
            "4" => "library",
            "5" => "about",
            s => s,
        };
        w.global::<SS>().set_sec(sh(sec.to_string()));
    }
    // the tray's "Update to Lasso X": open at Updates, so the install is seen (26 Nov 2026)
    if crate::update::install_asked() {
        w.global::<SS>().set_sec(sh("updates".to_string()));
    }
    fill_settings(w, "");
    backup_watch(w);
    let upd = settings_updater(w);
    let ww = w.as_weak();
    w.global::<SS>().on_set(move |k, v| {
        set_key(&k, &v);
        if matches!(k.as_str(), "accent" | "theme" | "ui_accent" | "ui_bg") {
            if let Some(w) = ww.upgrade() {
                apply_theme(&w.global::<T>(), &Config::load());
            }
        }
        if let Some(w) = ww.upgrade() {
            // the file name field: don't reset while the user types in it
            if k.as_str() != "name_pattern" {
                fill_settings(&w, "");
            } else {
                w.global::<SS>().set_name_example(sh(format!("{}.png", crate::util::name_from_pattern(&v, "Lasso"))));
            }
        }
    });
    let ww = w.as_weak();
    w.global::<SS>().on_listen(move |id| {
        if let Some(w) = ww.upgrade() {
            w.global::<SS>().set_listening(id);
        }
    });
    let ww = w.as_weak();
    w.global::<SS>().on_key_captured(move |text, ctrl, shift, alt| {
        let Some(w) = ww.upgrade() else { return false };
        let id = w.global::<SS>().get_listening().to_string();
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
    w.global::<SS>().on_act(move |a| {
        let Some(w) = ww.upgrade() else { return };
        if let Some(rest) = a.strip_prefix("bar:") {
            // "bar:add:pin" / remove / left / right -> the new order in config `bar`, the overlay reloads it
            let (op, id) = rest.split_once(':').unwrap_or((rest, ""));
            let v = crate::keys::bar_edit(Config::load().get("bar"), op, id);
            set_key("bar", &v);
            fill_settings(&w, "");
            return;
        }
        match a.as_str() {
            "library" => w.set_view(1),
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
            "game-dir-add" => {
                let start = dirs_home();
                let w2 = w.as_weak();
                std::thread::spawn(move || {
                    if let Some(p) = crate::dialogs::pick(false, "A folder with game screenshots", "<folder>", &start) {
                        let p = p.to_string_lossy().to_string();
                        let _ = slint::invoke_from_event_loop(move || {
                            let c = Config::load();
                            let mut v: Vec<String> = crate::gamepics::dirs(&c).iter().map(|d| d.to_string_lossy().to_string()).collect();
                            if !v.contains(&p) {
                                v.push(p);
                            }
                            set_key("game_dirs", &v.join("|"));
                            if let Some(w) = w2.upgrade() {
                                fill_settings(&w, "");
                            }
                        });
                    }
                });
            }
            m if m.starts_with("game-dir-remove:") => {
                let i: usize = m["game-dir-remove:".len()..].parse().unwrap_or(usize::MAX);
                let c = Config::load();
                let mut v: Vec<String> = crate::gamepics::dirs(&c).iter().map(|d| d.to_string_lossy().to_string()).collect();
                if i < v.len() {
                    v.remove(i);
                    set_key("game_dirs", &v.join("|"));
                    fill_settings(&w, "");
                }
            }
            m if m.starts_with("place-add:") => place_form(&w, &m["place-add:".len()..]),
            "place-cancel" => {
                w.global::<SS>().set_place_adding(SharedString::new());
                w.global::<SS>().set_place_msg(SharedString::new());
            }
            "place-connect" => {
                let ss = w.global::<SS>();
                let kind = ss.get_place_adding().to_string();
                let vals = [ss.get_pf0(), ss.get_pf1(), ss.get_pf2(), ss.get_pf3(), ss.get_pf4()];
                let keys = crate::clouds::fields(&kind);
                let vals: Vec<(String, String)> = keys.iter().zip(vals.iter()).map(|(f, v)| (f.0.to_string(), v.to_string())).collect();
                place_connect(&w, kind, vals);
            }
            m if m.starts_with("place-on:") => {
                let mut it = m["place-on:".len()..].split(':');
                if let (Some(Ok(id)), Some(v)) = (it.next().map(|x| x.parse::<u32>()), it.next()) {
                    let _ = crate::clouds::Rc::list_only().set_on(id, v == "1");
                    tell("reload");
                    fill_settings(&w, "");
                }
            }
            m if m.starts_with("place-remove:") => {
                if let Ok(id) = m["place-remove:".len()..].parse::<u32>() {
                    let w2 = w.as_weak();
                    std::thread::spawn(move || {
                        // its login goes too (rclone is there: it was used to add the place)
                        let r = match crate::clouds::Rc::app() {
                            Ok(rc) => rc.remove(id),
                            Err(_) => crate::clouds::Rc::list_only().remove_from_list(id),
                        };
                        crate::share::backup::forget_place(id);
                        let _ = slint::invoke_from_event_loop(move || {
                            if let Some(w) = w2.upgrade() {
                                w.global::<SS>().set_place_msg(sh(match r {
                                    Ok(()) => "Removed. The pictures already there stay.".to_string(),
                                    Err(e) => format!("Couldn't remove it: {e}"),
                                }));
                                tell("reload");
                                fill_settings(&w, "");
                            }
                        });
                    });
                }
            }
            "backup-delete" => {
                // Echo Cloud off first: the always-running Lasso's next round must not upload it all again
                set_key("backup_ec", "0");
                fill_settings(&w, "");
                w.global::<SS>().set_backup_status(sh("Deleting the backup from Echo Cloud...".to_string()));
                let w2 = w.as_weak();
                std::thread::spawn(move || {
                    let r = crate::share::backup::delete_all(&Config::load());
                    let _ = slint::invoke_from_event_loop(move || {
                        if let Some(w) = w2.upgrade() {
                            fill_settings(&w, "");
                            let msg = match r {
                                Ok(n) => format!("Deleted {n} pictures from Echo Cloud"),
                                Err(e) => format!("Couldn't delete the backup: {e}"),
                            };
                            w.global::<SS>().set_backup_status(sh(msg));
                        }
                    });
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
            // update channel (21 Nov 2026): Stable / Beta at once; Dev after sonela.app said the code is right
            a if a.starts_with("upd-channel:") => upd.set_channel(a["upd-channel:".len()..].parse().unwrap_or(0)),
            a if a.starts_with("upd-code:") => {
                let ww = ww.clone();
                upd.dev_code(a["upd-code:".len()..].to_string(), move |msg| {
                    let _ = ww.upgrade_in_event_loop(move |w| {
                        let g = w.global::<SS>();
                        g.set_upd_code_busy(false);
                        if msg.is_empty() {
                            g.set_upd_asking_code(false);
                        }
                        g.set_upd_code_msg(msg.into());
                    });
                });
            }
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

/// The key a tile's share link is kept under: a capture's id, or for a picture from the save folder a hash of its
/// path (share::file_key - never the path itself)
fn share_key(key: &str, is_lib: bool) -> String {
    if is_lib {
        key.to_string()
    } else {
        crate::share::file_key(std::path::Path::new(key))
    }
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

fn fill_library(w: &LassoWindow, lib: &Lib) {
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
        let sk = share_key(&k, it.editable);
        let li = LibItem {
            key: sh(k.clone()),
            lib: it.editable,
            thumb: lib.thumbs.get(&k).cloned().unwrap_or_default(),
            time: sh(format!("{h:02}:{mi:02}")),
            info: sh(if it.editable { "capture · shapes editable".to_string() } else { it.path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default() }),
            shared: it.shared || shared.contains(&sk),
            expiry: sh(crate::share::link_of(&sk).map(|l| if l.once { format!("View once · {}", crate::share::expiry_label(l.days)) } else { crate::share::expiry_label(l.days) }).unwrap_or_default()),
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
    w.global::<LL>().set_empty(empty);
    w.global::<LL>().set_searching(!lib.query.trim().is_empty());
    w.global::<LL>().set_group_by(lib.by_app as i32);
    w.global::<LL>().set_count(sh(format!("{} picture{}", lib.items.len(), if lib.items.len() == 1 { "" } else { "s" })));
    w.global::<LL>().set_selected(lib.sel.len() as i32);
    w.global::<LL>().set_share_on(crate::share::enabled(&Config::load()));
    w.global::<LL>().set_groups(ModelRc::new(VecModel::from(groups)));
}

thread_local! {
    static LIB: RefCell<Option<Rc<RefCell<Lib>>>> = const { RefCell::new(None) };
    /// the Library's last open (picture, time): a double-click must not open it twice
    static LAST_OPEN: RefCell<Option<(String, std::time::Instant)>> = const { RefCell::new(None) };
}

/// Changes the shown tiles in place (selection, thumbnails): rebuilding the model would make new tiles under the
/// mouse, and a double-click would never reach one tile twice.
fn patch_tiles(w: &LassoWindow, lib: &Lib) {
    use slint::Model;
    let groups = w.global::<LL>().get_groups();
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
    w.global::<LL>().set_selected(lib.sel.len() as i32);
}

/// Thumbnails in the background (export::tile_thumb; plain files get a cached one in the library's .thumbs folder)
fn load_thumbs(w: &LassoWindow, items: Vec<(String, std::path::PathBuf, bool)>) {
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
                let p = std::fs::read(&path).ok().and_then(|b| crate::export::decode(&b).ok());
                // a thumb from before 22 Nov 2026 (256 px) is too soft for the bigger tiles: made again from the picture
                let dir = path.parent().map(|d| d.to_path_buf()).unwrap_or_default();
                match p {
                    Some(t) if !dir.join(library::THUMB_MARK).exists() => {
                        let id = dir.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
                        match library::finished(&id).map(|f| crate::export::tile_thumb(&f)) {
                            Ok(big) => {
                                let _ = crate::export::write_new(&path, &crate::export::png(&big, true));
                                let _ = std::fs::write(dir.join(library::THUMB_MARK), b"");
                                Some(big)
                            }
                            _ => Some(t),
                        }
                    }
                    p => p,
                }
            } else {
                let stamp = std::fs::metadata(&path).ok().map(|m| (m.len(), m.modified().ok().and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok()).map(|d| d.as_secs()).unwrap_or(0))).unwrap_or((0, 0));
                let cp = cache.join(format!("{:x}.png", fnv(&format!("{key}|{}|{}|tile", stamp.0, stamp.1))));
                match std::fs::read(&cp).ok().and_then(|b| crate::export::decode(&b).ok()) {
                    Some(p) => Some(p),
                    None => {
                        let p = std::fs::read(&path).ok().and_then(|b| crate::export::decode(&b).ok()).map(|p| crate::export::tile_thumb(&p));
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
            format!("{:x}.png", fnv(&format!("{k}|{}|{}|tile", stamp.0, stamp.1)))
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

fn refresh(w: &LassoWindow, lib: &Rc<RefCell<Lib>>) {
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

/// The own backup places with what each one is doing ("Google Drive" / "you@... · 120 pictures backed up · last 14:02")
fn fill_places(w: &LassoWindow, c: &Config) {
    let on = c.on("backup_captures") || c.on("backup_games");
    let rows: Vec<PlaceRow> = crate::clouds::Rc::list_only()
        .places()
        .iter()
        .map(|p| {
            let st = crate::share::backup::place_status(p.id);
            let n = crate::share::backup::place_count(p.id);
            let what = if !p.on {
                "off".to_string()
            } else if on && !st.is_empty() {
                st
            } else if n > 0 {
                format!("{} there", crate::share::backup::pics(n))
            } else if on {
                "starts in a moment...".to_string()
            } else {
                "ready - switch on what to back up above".to_string()
            };
            PlaceRow { id: p.id as i32, name: sh(crate::clouds::kind_name(&p.kind).to_string()), sub: sh(format!("{} · {what}", crate::util::scrub_home(&p.label))), on: p.on }
        })
        .collect();
    w.global::<SS>().set_places(ModelRc::new(VecModel::from(rows)));
}

/// "Add a place": the form for that kind (or, for a folder, the folder picker right away)
fn place_form(w: &LassoWindow, kind: &str) {
    let ss = w.global::<SS>();
    if kind == "folder" {
        let start = dirs_home();
        let w2 = w.as_weak();
        std::thread::spawn(move || {
            if let Some(p) = crate::dialogs::pick(false, "A folder for the backup (a NAS, or your cloud's folder on this PC)", "<folder>", &start) {
                let p = p.to_string_lossy().to_string();
                let _ = slint::invoke_from_event_loop(move || {
                    if let Some(w) = w2.upgrade() {
                        place_connect(&w, "folder".into(), vec![("path".into(), p)]);
                    }
                });
            }
        });
        return;
    }
    let f: Vec<FormField> = crate::clouds::fields(kind).iter().map(|(_, l, h, sec)| FormField { label: sh(l.to_string()), hint: sh(h.to_string()), secret: *sec }).collect();
    ss.set_place_browser(f.is_empty());
    ss.set_place_fields(ModelRc::new(VecModel::from(f)));
    for set in [SS::set_pf0, SS::set_pf1, SS::set_pf2, SS::set_pf3, SS::set_pf4] {
        set(&ss, SharedString::new());
    }
    ss.set_place_msg(SharedString::new());
    ss.set_place_adding_name(sh(crate::clouds::kind_name(kind).to_string()));
    ss.set_place_adding(sh(kind.to_string()));
}

/// Connect in a worker thread (rclone may be downloaded first, a browser sign-in waits for the user)
fn place_connect(w: &LassoWindow, kind: String, vals: Vec<(String, String)>) {
    let ss = w.global::<SS>();
    if ss.get_place_busy() {
        return;
    }
    ss.set_place_busy(true);
    let first = !crate::clouds::rclone_ready();
    ss.set_place_msg(sh(match (first, kind.as_str()) {
        (true, _) => "Getting rclone first (the cloud tool Lasso uses, about 25 MB, only once)...".to_string(),
        (_, "gdrive" | "pcloud") => "Sign in in your browser - Lasso waits here...".to_string(),
        _ => "Connecting...".to_string(),
    }));
    let w2 = w.as_weak();
    std::thread::spawn(move || {
        let r = crate::clouds::Rc::app().and_then(|rc| {
            if first && matches!(kind.as_str(), "gdrive" | "pcloud") {
                let w3 = w2.clone();
                let _ = slint::invoke_from_event_loop(move || {
                    if let Some(w) = w3.upgrade() {
                        w.global::<SS>().set_place_msg(sh("Sign in in your browser - Lasso waits here...".to_string()));
                    }
                });
            }
            rc.connect(&kind, &vals)
        });
        let _ = slint::invoke_from_event_loop(move || {
            let Some(w) = w2.upgrade() else { return };
            let ss = w.global::<SS>();
            ss.set_place_busy(false);
            match r {
                Ok(p) => {
                    ss.set_place_adding(SharedString::new());
                    ss.set_place_msg(sh(format!("Added {} - the backup goes there from now on.", crate::clouds::kind_name(&p.kind))));
                    tell("reload");
                }
                Err(e) => ss.set_place_msg(sh(format!("Couldn't add it: {e}"))),
            }
            fill_settings(&w, "");
        });
    });
}

thread_local! {
    static BACKUP_WATCH: RefCell<Option<slint::Timer>> = const { RefCell::new(None) };
}

/// Settings: the backup's status line follows the worker (it runs in the always-running Lasso) every 3 seconds
fn backup_watch(w: &LassoWindow) {
    let ww = w.as_weak();
    let t = slint::Timer::default();
    t.start(slint::TimerMode::Repeated, std::time::Duration::from_secs(3), move || {
        let Some(w) = ww.upgrade() else { return };
        let ss = w.global::<SS>();
        // off: nothing runs (and "Deleted N pictures" stays put)
        if !ss.get_backup_captures() && !ss.get_backup_games() {
            return;
        }
        let st = crate::share::backup::status();
        if !st.is_empty() {
            ss.set_backup_status(sh(st));
        }
        ss.set_backup_count(crate::share::backup::count() as i32);
        // the own places' lines (not while the form is open: rebuilding the model would be harmless, but keep it calm)
        if !ss.get_place_busy() {
            fill_places(&w, &Config::load());
        }
    });
    BACKUP_WATCH.with(|x| *x.borrow_mut() = Some(t));
}

thread_local! {
    // the Library's Share link: looks once a second whether the running Lasso made the link (max. 2 minutes)
    static LINK_WATCH: RefCell<Option<slint::Timer>> = const { RefCell::new(None) };
}

/// After Share link: once the link exists, the tile shows it (link icon, Copy link / expiry / Stop sharing)
fn watch_link(w: &LassoWindow, sk: String) {
    let ww = w.as_weak();
    let t = slint::Timer::default();
    let mut tries = 0;
    t.start(slint::TimerMode::Repeated, std::time::Duration::from_secs(1), move || {
        tries += 1;
        let done = crate::share::link_of(&sk).is_some();
        if done {
            if let Some(w) = ww.upgrade() {
                lib_toast(&w, "Link copied - paste it anywhere");
                LIB.with(|l| {
                    if let Some(l) = l.borrow().as_ref() {
                        fill_library(&w, &l.borrow());
                    }
                });
            }
        }
        if done || tries >= 120 {
            // stop from outside this callback (a timer can't drop itself while it runs)
            slint::Timer::single_shot(std::time::Duration::ZERO, || LINK_WATCH.with(|x| *x.borrow_mut() = None));
        }
    });
    LINK_WATCH.with(|x| *x.borrow_mut() = Some(t));
}

/// Delete from the Library (one or many). A shared capture's public link is stopped FIRST (network, own thread) and
/// the capture goes only when that worked - else a "never" link would stay public with no way to stop it (review D1).
fn delete_keys(w: &LassoWindow, lib: &Rc<RefCell<Lib>>, keys: Vec<(String, bool)>) {
    // shared ones (captures and, since 24 Nov 2026, save-folder pictures): their link is stopped first
    let (shared, plain): (Vec<_>, Vec<_>) = keys.into_iter().partition(|(k, is_lib)| crate::share::link_of(&share_key(k, *is_lib)).is_some());
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
        let res: Vec<(String, bool, Result<(), String>)> = shared.into_iter().map(|(k, is_lib)| {
            let r = crate::share::stop(&cfg, &share_key(&k, is_lib));
            (k, is_lib, r)
        }).collect();
        let _ = slint::invoke_from_event_loop(move || {
            let mut fail = None;
            let mut done = 0;
            for (k, is_lib, r) in res {
                match r {
                    Ok(()) => done += delete_item(&k, is_lib).map(|_| 1).unwrap_or(0),
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

fn lib_toast(w: &LassoWindow, t: &str) {
    w.global::<LL>().set_toast(sh(t));
    let ww = w.as_weak();
    slint::Timer::single_shot(std::time::Duration::from_millis(2200), move || {
        if let Some(w) = ww.upgrade() {
            w.global::<LL>().set_toast(sh(""));
        }
    });
}

thread_local! {
    static LIB_WIN: RefCell<Option<slint::Weak<LassoWindow>>> = const { RefCell::new(None) };
}

/// A drag with files is over the library (Wayland: wl_dnd.rs; Windows / X11: winit)
pub fn lib_drag_over(on: bool) {
    LIB_WIN.with(|w| {
        if let Some(w) = w.borrow().as_ref().and_then(|w| w.upgrade()) {
            w.global::<LL>().set_drag_over(on);
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

/// The Library page's setup; the returned timer (new captures show up) lives as long as the window
fn setup_library(w: &LassoWindow) -> slint::Timer {
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
    refresh(w, &lib);
    let (ww, l2) = (w.as_weak(), lib.clone());
    w.global::<LL>().on_search(move |q| {
        if let Some(w) = ww.upgrade() {
            l2.borrow_mut().query = q.to_string();
            fill_library(&w, &l2.borrow());
        }
    });
    let (ww, l2) = (w.as_weak(), lib.clone());
    w.global::<LL>().on_group(move |v| {
        if let Some(w) = ww.upgrade() {
            l2.borrow_mut().by_app = v == 1;
            let mut c = Config::load();
            c.set("library_group", if v == 1 { "app" } else { "day" });
            c.save();
            fill_library(&w, &l2.borrow());
        }
    });
    let (ww, l2) = (w.as_weak(), lib.clone());
    w.global::<LL>().on_pick(move |key, add| {
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
    w.global::<LL>().on_act(move |a, key| {
        let Some(w) = ww.upgrade() else { return };
        let key = key.to_string();
        let is_lib = l2.borrow().items.iter().any(|it| it.id == key && !key.is_empty());
        match a.as_str() {
            // opened on the monitor the Library is on (the user, 22 Nov 2026: "double-clicking opens it on the 2nd
            // monitor" - it went where the mouse last was over Lasso's own screens)
            "edit" => {
                // one open per picture per 0.8 s: a double-click (two clicks) opened it twice
                let now = std::time::Instant::now();
                let again = LAST_OPEN.with(|l| {
                    let mut l = l.borrow_mut();
                    let again = l.as_ref().is_some_and(|(k, t)| *k == key && now.duration_since(*t).as_millis() < 800);
                    *l = Some((key.clone(), now));
                    again
                });
                if again {
                    return;
                }
                // the monitor's name + the window's centre (Windows' names don't match Lasso's, app::out_for)
                let mon = w
                    .window()
                    .with_winit_window(|ww| {
                        let name = ww.current_monitor().and_then(|m| m.name()).unwrap_or_default();
                        match ww.outer_position() {
                            Ok(p) => {
                                let s = ww.outer_size();
                                format!("{name}@{},{}", p.x + s.width as i32 / 2, p.y + s.height as i32 / 2)
                            }
                            Err(_) => name,
                        }
                    })
                    .unwrap_or_default();
                tell(&format!("{}:{key}\u{1f}{mon}", if is_lib { "edit" } else { "open" }))
            }
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
            "copy-link" => match crate::share::link_of(&share_key(&key, is_lib)) {
                Some(l) => {
                    tell(&format!("copy-text:{}", l.url));
                    lib_toast(&w, "Link copied");
                }
                None => lib_toast(&w, "Not shared"),
            },
            // 24 Nov 2026 (the user: "is there no share link for existing pictures in the library?"): shared already =
            // its link is copied; else the running Lasso uploads it (privacy check, link on the clipboard, a
            // notification), and the tile shows the link once it's there
            "share" => {
                let sk = share_key(&key, is_lib);
                if let Some(l) = crate::share::link_of(&sk) {
                    tell(&format!("copy-text:{}", l.url));
                    lib_toast(&w, "Link copied");
                } else if !crate::share::enabled(&Config::load()) {
                    lib_toast(&w, "Share links are off - switch them on in Settings > Share links");
                } else {
                    tell(&if is_lib { format!("share-lib:{key}") } else { format!("share-file:{key}") });
                    lib_toast(&w, "Making a link...");
                    watch_link(&w, sk);
                }
            }
            "stop-share" | "expiry" => {
                let cfg = Config::load();
                let k = share_key(&key, is_lib);
                let what = a.to_string();
                let ww = w.as_weak();
                let next = crate::share::link_of(&k).map(|l| {
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
    w.global::<LL>().on_bulk(move |a| {
        let Some(w) = ww.upgrade() else { return };
        match a.as_str() {
            "open" => tell("open-dialog"),
            "settings" => w.set_view(0),
            "capture" => {
                w.window().set_minimized(true);
                tell("region");
            }
            "clear" => {
                l2.borrow_mut().sel.clear();
                patch_tiles(&w, &l2.borrow());
            }
            // Ctrl+A (24 Nov 2026): every picture shown right now (the search filters what "all" is)
            "all" => {
                use slint::Model;
                let groups = w.global::<LL>().get_groups();
                let keys: Vec<String> = groups.iter().flat_map(|g| g.items.iter().map(|it| it.key.to_string()).collect::<Vec<_>>()).collect();
                l2.borrow_mut().sel = keys;
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
    timer
}

#[cfg(test)]
mod tests {
    /// The Teal theme's palette = theme.slint's defaults (the window looks the same before the theme is applied)
    #[test]
    fn teal_palette_matches_theme_slint() {
        let src = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/ui/theme.slint")).unwrap();
        let p = crate::theme::palette("", "", "", || false);
        for (n, v) in [("bg", p.bg), ("text", p.text), ("muted", p.muted), ("dim", p.dim), ("accent", p.accent), ("accent2", p.accent2)] {
            let line = src.lines().find(|l| l.contains(&format!("property <color> {n}:"))).unwrap();
            assert!(line.contains(&format!("#{v:06x}")), "{n}: {line}");
        }
    }
}
