//! Windows: start-up (one Lasso, DPI, the window hook), the screen grab (DXGI / Windows.Graphics.Capture with HDR
//! tone-mapping in win/dxgi.rs, GDI as the fallback; per monitor, in parallel, physical px), the frozen screen's
//! window (win/overlay.rs), the window list for Window mode, tray icon, global hotkeys (RegisterHotKey - no
//! keyboard hook), clipboard (win/clip.rs), recycle bin, file dialogs, Start with your PC.
//! ⚠ Type-checked from Linux only (cargo check --target x86_64-pc-windows-gnu); the real test is on Windows.

mod clip;
mod dxgi;
mod geom;
mod overlay;

use crate::app::OutInfo;
use crate::shapes::{Pic, R};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Mutex;
use std::time::Instant;
use windows_sys::Win32::Foundation::{BOOL, HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows_sys::Win32::Graphics::Gdi::*;
use windows_sys::Win32::UI::WindowsAndMessaging::*;

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

// ---------------------------------------------------------------------------------------------------------------
// Start / end of the always-running part (main.rs daemon / main)

static INSTANCE: Mutex<usize> = Mutex::new(0);

/// Before any Slint window exists. False = another Lasso is starting at this very moment: it got `first`
/// (or nothing could be sent) and this process should end.
pub fn init(first: Option<&str>) -> bool {
    unsafe {
        windows_sys::Win32::UI::HiDpi::SetProcessDpiAwarenessContext(windows_sys::Win32::UI::HiDpi::DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
    }
    // One Lasso per user session. The port file (ipc.rs) finds a running one; this mutex also covers two starting
    // at the same moment (autostart + a hotkey tool, a double-click).
    let mut name = String::from("Local\\SonelaLasso");
    if let Some(h) = std::env::var_os("LASSO_HOME") {
        // test copies have their own home -> their own instance
        name.push_str(&format!("-{:x}", h.to_string_lossy().bytes().fold(0u64, |a, b| a.wrapping_mul(31).wrapping_add(b as u64))));
    }
    let n = wide(&name);
    let h = unsafe { windows_sys::Win32::System::Threading::CreateMutexW(std::ptr::null(), 0, n.as_ptr()) };
    let err = unsafe { windows_sys::Win32::Foundation::GetLastError() };
    if !h.is_null() && err == windows_sys::Win32::Foundation::ERROR_ALREADY_EXISTS {
        for _ in 0..30 {
            std::thread::sleep(std::time::Duration::from_millis(100));
            if crate::ipc::send(first.unwrap_or("ping")).is_ok() {
                break;
            }
        }
        crate::log::line("app", "another Lasso is starting - this one ends");
        return false;
    }
    *INSTANCE.lock().unwrap() = h as usize; // kept open until the process ends
    // Every window of this process (frozen screen, corner popups, pins): no taskbar button, and not activated when
    // it first shows (the frozen screen takes the focus itself, see win/overlay.rs; popups never steal it)
    let r = slint::BackendSelector::new()
        .backend_name("winit".into())
        .with_winit_window_attributes_hook(|a| {
            use slint::winit_030::winit::platform::windows::WindowAttributesExtWindows;
            a.with_skip_taskbar(true).with_active(false)
        })
        .select();
    if let Err(e) = r {
        crate::log::line("app", &format!("window backend: {e}"));
    }
    true
}

/// After the event loop ended: the tray icon goes at once (else it stays until the mouse passes over it)
pub fn shutdown() {
    let h = tray_hwnd();
    if !h.is_null() {
        let nid = tray_data(h);
        unsafe { Shell_NotifyIconW(NIM_DELETE, &nid) };
    }
}

/// The sending side of `lasso --region` (another program had the focus): let the running Lasso take it
pub fn allow_foreground() {
    unsafe { AllowSetForegroundWindow(ASFW_ANY) };
}

// ---------------------------------------------------------------------------------------------------------------
// Monitors

struct Mon {
    rect: RECT,
    dpi: u32,
    hmon: usize,
    primary: bool,
}

impl Mon {
    fn geo(&self) -> geom::Geo {
        geom::Geo { l: self.rect.left, t: self.rect.top, r: self.rect.right, b: self.rect.bottom, s: self.dpi as f32 / 96.0 }
    }
}

unsafe extern "system" fn mon_cb(h: HMONITOR, _: HDC, _: *mut RECT, data: LPARAM) -> BOOL {
    let v = &mut *(data as *mut Vec<Mon>);
    let mut mi: MONITORINFO = std::mem::zeroed();
    mi.cbSize = std::mem::size_of::<MONITORINFO>() as u32;
    if GetMonitorInfoW(h, &mut mi) != 0 {
        let (mut dx, mut dy) = (96u32, 96u32);
        let _ = windows_sys::Win32::UI::HiDpi::GetDpiForMonitor(h, windows_sys::Win32::UI::HiDpi::MDT_EFFECTIVE_DPI, &mut dx, &mut dy);
        v.push(Mon { rect: mi.rcMonitor, dpi: dx.max(48), hmon: h as usize, primary: mi.dwFlags & MONITORINFOF_PRIMARY != 0 });
    }
    1
}

fn monitors() -> Vec<Mon> {
    unsafe {
        windows_sys::Win32::UI::HiDpi::SetProcessDpiAwarenessContext(windows_sys::Win32::UI::HiDpi::DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
        let mut v: Vec<Mon> = vec![];
        EnumDisplayMonitors(std::ptr::null_mut(), std::ptr::null(), Some(mon_cb), &mut v as *mut _ as LPARAM);
        geom::primary_first(&mut v, |m| m.primary);
        v
    }
}

/// The monitors for app.rs (key = place in `monitors()`, the primary first). x / y = the PHYSICAL origin, w / h =
/// physical size / scale: logical areas that never overlap with mixed scaling (see win/geom.rs)
pub fn outputs() -> Vec<OutInfo> {
    monitors()
        .iter()
        .enumerate()
        .map(|(i, m)| {
            let (x, y, w, h) = m.geo().logical();
            OutInfo { key: i as u32, name: format!("monitor {}", i + 1), x, y, w, h, pw: m.rect.right - m.rect.left, ph: m.rect.bottom - m.rect.top }
        })
        .collect()
}

/// app.rs stitch ("all monitors" full screenshot) on Windows: the monitors at their physical places, each at its
/// own pixel size (no logical desktop to scale to; the origins in `outputs()` are physical already)
pub fn physical_layout(infos: &[OutInfo]) -> Vec<OutInfo> {
    infos.iter().map(|o| OutInfo { w: o.pw, h: o.ph, ..o.clone() }).collect()
}

/// The monitor (output key) the mouse is on - Full screenshot "monitor under the mouse"
pub fn monitor_under_mouse() -> Option<u32> {
    let mut p = POINT { x: 0, y: 0 };
    if unsafe { GetCursorPos(&mut p) } == 0 {
        return None;
    }
    monitors().iter().position(|m| p.x >= m.rect.left && p.x < m.rect.right && p.y >= m.rect.top && p.y < m.rect.bottom).map(|i| i as u32)
}

// ---------------------------------------------------------------------------------------------------------------
// Screen grab: every monitor at once (one thread each); per monitor DXGI -> Windows.Graphics.Capture -> GDI.
// LASSO_CAPTURE=gdi forces the old way (for trouble-shooting).

pub fn grab(infos: &[OutInfo], cursor: bool) -> Result<Vec<(u32, Pic)>, String> {
    let t0 = Instant::now();
    let mons = monitors();
    let jobs: Vec<(u32, RECT, usize)> = infos.iter().filter_map(|i| mons.get(i.key as usize).map(|m| (i.key, m.rect, m.hmon))).collect();
    let gdi_only = std::env::var("LASSO_CAPTURE").map(|v| v.eq_ignore_ascii_case("gdi")).unwrap_or(false);
    let shots: Vec<(u32, RECT, Pic, &'static str)> = std::thread::scope(|s| {
        let hs: Vec<_> = jobs
            .iter()
            .map(|&(k, r, hm)| {
                s.spawn(move || {
                    if !gdi_only {
                        match dxgi::grab(hm, (r.left, r.top, r.right, r.bottom)) {
                            Ok((p, how)) if p.w as i32 == r.right - r.left && p.h as i32 == r.bottom - r.top => return (k, r, p, how),
                            Ok((p, how)) => crate::log::line("capture", &format!("{how} gave {} x {} for a {} x {} monitor - GDI instead", p.w, p.h, r.right - r.left, r.bottom - r.top)),
                            Err(e) => crate::log::line("capture", &format!("monitor {}: {e} - GDI instead", k + 1)),
                        }
                    }
                    (k, r, gdi(&r), "gdi")
                })
            })
            .collect();
        hs.into_iter().filter_map(|h| h.join().ok()).collect()
    });
    let mut out = vec![];
    let mut how = vec![];
    for (k, r, mut p, h) in shots {
        if cursor {
            draw_cursor(&mut p, r.left, r.top);
        }
        how.push(h);
        out.push((k, p));
    }
    if out.is_empty() {
        return Err("no monitor".into());
    }
    crate::log::line("capture", &format!("grab {} in {} ms", how.join(" + "), t0.elapsed().as_millis()));
    Ok(out)
}

/// BitBlt of one monitor (physical px)
fn gdi(r: &RECT) -> Pic {
    let (x, y, w, h) = (r.left, r.top, (r.right - r.left).max(0), (r.bottom - r.top).max(0));
    unsafe {
        let screen = GetDC(std::ptr::null_mut());
        let mem = CreateCompatibleDC(screen);
        let bmp = CreateCompatibleBitmap(screen, w, h);
        let old = SelectObject(mem, bmp);
        BitBlt(mem, 0, 0, w, h, screen, x, y, SRCCOPY | CAPTUREBLT);
        let mut bi: BITMAPINFO = std::mem::zeroed();
        bi.bmiHeader.biSize = std::mem::size_of::<BITMAPINFOHEADER>() as u32;
        bi.bmiHeader.biWidth = w;
        bi.bmiHeader.biHeight = -h;
        bi.bmiHeader.biPlanes = 1;
        bi.bmiHeader.biBitCount = 32;
        bi.bmiHeader.biCompression = BI_RGB;
        let mut px = vec![0u8; (w * h * 4) as usize];
        GetDIBits(mem, bmp, 0, h as u32, px.as_mut_ptr() as *mut _, &mut bi, DIB_RGB_COLORS);
        SelectObject(mem, old);
        DeleteObject(bmp);
        DeleteDC(mem);
        ReleaseDC(std::ptr::null_mut(), screen);
        for p in px.chunks_exact_mut(4) {
            p.swap(0, 2);
            p[3] = 255;
        }
        Pic { w: w as u32, h: h as u32, px }
    }
}

/// The mouse pointer onto a monitor picture (`ox`, `oy` = the monitor's top-left, physical px). GDI draws it, so
/// black-and-white and inverting pointers (text I-beam) come out right.
fn draw_cursor(pic: &mut Pic, ox: i32, oy: i32) {
    unsafe {
        let mut ci: CURSORINFO = std::mem::zeroed();
        ci.cbSize = std::mem::size_of::<CURSORINFO>() as u32;
        if GetCursorInfo(&mut ci) == 0 || ci.flags & CURSOR_SHOWING == 0 || ci.hCursor.is_null() {
            return;
        }
        let mut ii: ICONINFO = std::mem::zeroed();
        if GetIconInfo(ci.hCursor, &mut ii) == 0 {
            return;
        }
        let mut bm: BITMAP = std::mem::zeroed();
        let src = if ii.hbmColor.is_null() { ii.hbmMask } else { ii.hbmColor };
        GetObjectW(src, std::mem::size_of::<BITMAP>() as i32, &mut bm as *mut _ as *mut _);
        let (cw, ch) = (bm.bmWidth, if ii.hbmColor.is_null() { bm.bmHeight / 2 } else { bm.bmHeight });
        for b in [ii.hbmMask, ii.hbmColor] {
            if !b.is_null() {
                DeleteObject(b);
            }
        }
        let x0 = ci.ptScreenPos.x - ox - ii.xHotspot as i32;
        let y0 = ci.ptScreenPos.y - oy - ii.yHotspot as i32;
        let (pw, ph) = (pic.w as i32, pic.h as i32);
        let (l, t, r, b) = (x0.max(0), y0.max(0), (x0 + cw).min(pw), (y0 + ch).min(ph));
        if r <= l || b <= t {
            return;
        }
        let (rw, rh) = ((r - l) as usize, (b - t) as usize);
        let dc = CreateCompatibleDC(std::ptr::null_mut());
        let mut bi: BITMAPINFO = std::mem::zeroed();
        bi.bmiHeader.biSize = std::mem::size_of::<BITMAPINFOHEADER>() as u32;
        bi.bmiHeader.biWidth = rw as i32;
        bi.bmiHeader.biHeight = -(rh as i32);
        bi.bmiHeader.biPlanes = 1;
        bi.bmiHeader.biBitCount = 32;
        bi.bmiHeader.biCompression = BI_RGB;
        let mut bits: *mut core::ffi::c_void = std::ptr::null_mut();
        let bmp = CreateDIBSection(dc, &bi, DIB_RGB_COLORS, &mut bits, std::ptr::null_mut(), 0);
        if bmp.is_null() || bits.is_null() {
            DeleteDC(dc);
            return;
        }
        let old = SelectObject(dc, bmp);
        let area = std::slice::from_raw_parts_mut(bits as *mut u8, rw * rh * 4);
        let stride = pic.w as usize * 4;
        for y in 0..rh {
            let s = (t as usize + y) * stride + l as usize * 4;
            for x in 0..rw {
                let (p, q) = (&pic.px[s + x * 4..s + x * 4 + 4], y * rw * 4 + x * 4);
                area[q] = p[2];
                area[q + 1] = p[1];
                area[q + 2] = p[0];
                area[q + 3] = 255;
            }
        }
        DrawIconEx(dc, x0 - l, y0 - t, ci.hCursor, 0, 0, 0, std::ptr::null_mut(), DI_NORMAL);
        GdiFlush();
        for y in 0..rh {
            let s = (t as usize + y) * stride + l as usize * 4;
            for x in 0..rw {
                let q = y * rw * 4 + x * 4;
                let d = &mut pic.px[s + x * 4..s + x * 4 + 4];
                d[0] = area[q + 2];
                d[1] = area[q + 1];
                d[2] = area[q];
                d[3] = 255;
            }
        }
        SelectObject(dc, old);
        DeleteObject(bmp);
        DeleteDC(dc);
    }
}

// ---------------------------------------------------------------------------------------------------------------
// The frozen screen's window (win/overlay.rs), called from main.rs show_overlay and app.rs make_overlay

/// main.rs show_overlay on Windows: over monitor `key`, topmost, focused, no flash
pub fn show_overlay<C: slint::ComponentHandle + 'static>(comp: &C, key: u32) {
    let mons = monitors();
    let r = mons.get(key as usize).or(mons.first()).map(|m| m.rect).unwrap_or(RECT { left: 0, top: 0, right: 1920, bottom: 1080 });
    overlay::show(comp, r);
}

/// app.rs make_overlay on Windows: make the window now, hidden, so the first freeze is fast too
pub fn prewarm_overlay<C: slint::ComponentHandle + 'static>(comp: &C) {
    overlay::prewarm(comp);
}

/// main.rs make_pin / move_pin on Windows: `pos` = logical px on monitor `out` (None = the primary) -> physical
/// screen px on THAT monitor (a plain logical position lands on the primary); the size stays logical, winit applies
/// the scale of the monitor the pin is on
pub fn place_pin(w: &slint::Window, out: Option<u32>, pos: (i32, i32), size: (i32, i32)) {
    use slint::winit_030::{winit, WinitWindowAccessor};
    let mons = monitors();
    let m = out.and_then(|k| mons.get(k as usize)).or(mons.first());
    let (x, y) = m.map(|m| geom::pin_origin(&m.geo(), pos)).unwrap_or(pos);
    w.set_position(slint::PhysicalPosition::new(x, y));
    w.set_size(slint::LogicalSize::new(size.0 as f32, size.1 as f32));
    w.with_winit_window(|ww| ww.set_window_level(winit::window::WindowLevel::AlwaysOnTop));
}

// ---------------------------------------------------------------------------------------------------------------
// Window list (top first, real frame without the invisible resize border; hidden, cloaked, click-through and
// desktop-background windows left out). The string is the window class (app.rs drops Lasso's own).

unsafe extern "system" fn win_cb(h: HWND, data: LPARAM) -> BOOL {
    let v = &mut *(data as *mut Vec<(R, String)>);
    if IsWindowVisible(h) == 0 || IsIconic(h) != 0 {
        return 1;
    }
    let mut cloaked: u32 = 0;
    windows_sys::Win32::Graphics::Dwm::DwmGetWindowAttribute(h, windows_sys::Win32::Graphics::Dwm::DWMWA_CLOAKED as u32, &mut cloaked as *mut _ as *mut _, 4);
    if cloaked != 0 {
        return 1;
    }
    // overlays that let clicks through (game / GPU overlays, Echo's own) aren't windows to capture
    let ex = GetWindowLongW(h, GWL_EXSTYLE) as u32;
    if ex & WS_EX_TRANSPARENT != 0 && ex & WS_EX_LAYERED != 0 {
        return 1;
    }
    let mut pid = 0u32;
    GetWindowThreadProcessId(h, &mut pid);
    if pid == std::process::id() {
        return 1;
    }
    let mut cls = [0u16; 128];
    let n = GetClassNameW(h, cls.as_mut_ptr(), cls.len() as i32).max(0) as usize;
    let cls = String::from_utf16_lossy(&cls[..n]);
    if cls == "Progman" || cls == "WorkerW" {
        return 1; // the desktop background: a click there = the whole monitor
    }
    let mut r: RECT = std::mem::zeroed();
    if windows_sys::Win32::Graphics::Dwm::DwmGetWindowAttribute(h, windows_sys::Win32::Graphics::Dwm::DWMWA_EXTENDED_FRAME_BOUNDS as u32, &mut r as *mut _ as *mut _, std::mem::size_of::<RECT>() as u32) != 0 {
        GetWindowRect(h, &mut r);
    }
    if r.right - r.left < 8 || r.bottom - r.top < 8 {
        return 1;
    }
    v.push((R::new(r.left as f32, r.top as f32, (r.right - r.left) as f32, (r.bottom - r.top) as f32), cls));
    1
}

pub fn windows() -> Vec<(R, String)> {
    let mut v: Vec<(R, String)> = vec![];
    unsafe {
        EnumWindows(Some(win_cb), &mut v as *mut _ as LPARAM);
    }
    // physical px -> the logical layout of `outputs()`: one piece per monitor the window is on, each with that
    // monitor's scale (a window across two monitors of different scale gets the right box on both)
    let geos: Vec<geom::Geo> = monitors().iter().map(Mon::geo).collect();
    v.into_iter()
        .flat_map(|(r, n)| geom::window_pieces((r.x, r.y, r.w, r.h), &geos).into_iter().map(move |(x, y, w, h)| (R::new(x, y, w, h), n.clone())))
        .collect()
}

pub fn dark_theme() -> bool {
    use windows_sys::Win32::System::Registry::{RegGetValueW, HKEY_CURRENT_USER, RRF_RT_REG_DWORD};
    let key = wide("Software\\Microsoft\\Windows\\CurrentVersion\\Themes\\Personalize");
    let val = wide("AppsUseLightTheme");
    let mut data: u32 = 1;
    let mut len: u32 = 4;
    let r = unsafe { RegGetValueW(HKEY_CURRENT_USER, key.as_ptr(), val.as_ptr(), RRF_RT_REG_DWORD, std::ptr::null_mut(), &mut data as *mut _ as *mut _, &mut len) };
    r != 0 || data == 0
}

// ---------------------------------------------------------------------------------------------------------------
// Global hotkeys: RegisterHotKey on a thread with its own message loop (no hook)

fn parse_hotkey(s: &str) -> Option<(u32, u32)> {
    use windows_sys::Win32::UI::Input::KeyboardAndMouse::*;
    let named = |k: &str| -> Option<u16> {
        Some(match k {
            "print" | "printscreen" | "prtsc" | "sysrq" => VK_SNAPSHOT,
            "pause" => VK_PAUSE,
            "scrolllock" | "scroll_lock" => VK_SCROLL,
            "insert" => VK_INSERT,
            "delete" => VK_DELETE,
            "home" => VK_HOME,
            "end" => VK_END,
            "pageup" | "page_up" | "prior" => VK_PRIOR,
            "pagedown" | "page_down" | "next" => VK_NEXT,
            "space" => VK_SPACE,
            _ => return None,
        })
    };
    let mut mods = MOD_NOREPEAT;
    let mut vk = 0u32;
    for p in s.split('+').map(|x| x.trim()) {
        let k = p.to_ascii_lowercase();
        match k.as_str() {
            "ctrl" | "control" => mods |= MOD_CONTROL,
            "alt" => mods |= MOD_ALT,
            "shift" => mods |= MOD_SHIFT,
            "win" | "meta" | "super" => mods |= MOD_WIN,
            // letters and digits: the virtual-key code is the upper-case ASCII
            k if k.len() == 1 && k.chars().all(|c| c.is_ascii_alphanumeric()) => vk = k.to_ascii_uppercase().chars().next()? as u32,
            k if k.len() > 1 && k.starts_with('f') && k[1..].chars().all(|c| c.is_ascii_digit()) => {
                let n = k[1..].parse::<u32>().ok()?;
                if !(1..=24).contains(&n) {
                    return None;
                }
                vk = VK_F1 as u32 + n - 1;
            }
            k => vk = named(k)? as u32,
        }
    }
    (vk != 0).then_some((mods, vk))
}

/// The running hotkey thread: its id (for WM_QUIT) and its handle (the next one waits for it to end = keys free)
static HK_THREAD: Mutex<Option<(u32, std::thread::JoinHandle<()>)>> = Mutex::new(None);

/// What the Settings window shows under "Global hotkeys" (shared file name with hotkeys_linux.rs)
fn hk_status_file() -> std::path::PathBuf {
    crate::paths::config_dir().join("kde_keys.txt")
}

pub fn start_hotkeys(fire: impl Fn(&str) + Send + Sync + 'static) {
    let cfg = crate::config::Config::load();
    // stop the old thread (keys changed); the new one waits for it to end, then its keys are free
    let old = HK_THREAD.lock().unwrap().take().map(|(tid, h)| {
        unsafe { PostThreadMessageW(tid, WM_QUIT, 0, 0) };
        h
    });
    if !cfg.on("hotkeys") {
        // switched off: no old "taken by another app" note left in Settings
        let _ = std::fs::remove_file(hk_status_file());
        return;
    }
    let ids = ["region", "full", "window", "repeat", "restore", "delete"];
    let keys: Vec<(usize, Option<(u32, u32)>)> = ids.iter().enumerate().map(|(i, id)| (i, parse_hotkey(cfg.get(&format!("hk.{id}"))))).collect();
    let (tx, rx) = std::sync::mpsc::channel::<u32>();
    let h = std::thread::spawn(move || unsafe {
        use windows_sys::Win32::UI::Input::KeyboardAndMouse::{RegisterHotKey, UnregisterHotKey};
        // a message queue first, so a WM_QUIT for this thread can't get lost
        let mut msg: MSG = std::mem::zeroed();
        PeekMessageW(&mut msg, std::ptr::null_mut(), 0, 0, PM_NOREMOVE);
        let _ = tx.send(windows_sys::Win32::System::Threading::GetCurrentThreadId());
        // the old thread unregisters its keys when it leaves its loop: wait for that instead of guessing a time
        if let Some(old) = old {
            let _ = old.join();
        }
        // a key another program holds for a moment (or a slow old thread): a few tries before calling it taken
        let mut bad: Vec<&str> = vec![];
        let mut ok = vec![false; keys.len()];
        for attempt in 0..5 {
            if attempt > 0 {
                std::thread::sleep(std::time::Duration::from_millis(60));
            }
            bad.clear();
            for (n, (i, k)) in keys.iter().enumerate() {
                let Some((m, vk)) = k else { continue };
                if !ok[n] {
                    ok[n] = RegisterHotKey(std::ptr::null_mut(), *i as i32 + 1, *m, *vk) != 0;
                }
                if !ok[n] {
                    bad.push(ids[*i]);
                }
            }
            if bad.is_empty() {
                break;
            }
        }
        if bad.is_empty() {
            // all keys work now: an older error must not stay in Settings
            let _ = std::fs::remove_file(hk_status_file());
        } else {
            // Print is Snipping Tool's on Windows 11 until "Use the Print screen key to open screen capture" is off
            crate::log::line("hotkeys", &format!("taken by another app: {}", bad.join(", ")));
            let _ = std::fs::write(hk_status_file(), format!("error=taken by another app: {} (Print: Settings > Accessibility > Keyboard)\n", bad.join(", ")));
        }
        while GetMessageW(&mut msg, std::ptr::null_mut(), 0, 0) > 0 {
            if msg.message == WM_HOTKEY {
                if let Some(id) = ids.get(msg.wParam.saturating_sub(1)) {
                    fire(id);
                }
            }
        }
        for (n, (i, _)) in keys.iter().enumerate() {
            if ok[n] {
                UnregisterHotKey(std::ptr::null_mut(), *i as i32 + 1);
            }
        }
    });
    match rx.recv_timeout(std::time::Duration::from_secs(2)) {
        Ok(tid) => *HK_THREAD.lock().unwrap() = Some((tid, h)),
        Err(_) => crate::log::line("hotkeys", "hotkey thread didn't start"),
    }
}

// ---------------------------------------------------------------------------------------------------------------
// Tray icon (Shell_NotifyIcon on a hidden window, own thread). Lasso's own icon (assets/icons) at the tray's size;
// comes back by itself when Explorer restarts.

use windows_sys::Win32::UI::Shell::{Shell_NotifyIconW, NIF_ICON, NIF_INFO, NIF_MESSAGE, NIF_TIP, NIIF_INFO, NIM_ADD, NIM_DELETE, NIM_MODIFY, NOTIFYICONDATAW};

static TRAY_RESTORE: Mutex<Option<String>> = Mutex::new(None);
/// "Update to Lasso X" on top of the menu while an update is known (src/update.rs)
static TRAY_UPDATE: Mutex<Option<String>> = Mutex::new(None);
static TRAY_HWND: Mutex<usize> = Mutex::new(0);
type Send2 = Box<dyn Fn(&str) + Send + Sync>;
static TRAY_SEND: Mutex<Option<Send2>> = Mutex::new(None);
static TASKBAR_CREATED: AtomicU32 = AtomicU32::new(0);
const WM_TRAY: u32 = WM_APP + 1;

pub(crate) fn tray_hwnd() -> HWND {
    *TRAY_HWND.lock().unwrap() as HWND
}

pub fn set_tray_restore(label: Option<String>) {
    *TRAY_RESTORE.lock().unwrap() = label;
}

pub fn set_tray_update(label: Option<String>) {
    *TRAY_UPDATE.lock().unwrap() = label;
}

fn tray_send(c: &str) {
    if let Some(s) = TRAY_SEND.lock().unwrap().as_ref() {
        s(c);
    }
}

/// Lasso's icon from the PNGs in assets/icons, closest to `size` px (null = none could be made)
fn lasso_icon(size: i32) -> HICON {
    let png: &[u8] = match size {
        ..=16 => include_bytes!("../assets/icons/lasso-16.png"),
        17..=22 => include_bytes!("../assets/icons/lasso-22.png"),
        23..=24 => include_bytes!("../assets/icons/lasso-24.png"),
        25..=32 => include_bytes!("../assets/icons/lasso-32.png"),
        33..=48 => include_bytes!("../assets/icons/lasso-48.png"),
        _ => include_bytes!("../assets/icons/lasso-64.png"),
    };
    let Ok(img) = image::load_from_memory(png) else { return std::ptr::null_mut() };
    let img = img.to_rgba8();
    let (w, h) = (img.width() as i32, img.height() as i32);
    unsafe {
        let mut bi: BITMAPINFO = std::mem::zeroed();
        bi.bmiHeader.biSize = std::mem::size_of::<BITMAPINFOHEADER>() as u32;
        bi.bmiHeader.biWidth = w;
        bi.bmiHeader.biHeight = -h;
        bi.bmiHeader.biPlanes = 1;
        bi.bmiHeader.biBitCount = 32;
        bi.bmiHeader.biCompression = BI_RGB;
        let mut bits: *mut core::ffi::c_void = std::ptr::null_mut();
        let color = CreateDIBSection(std::ptr::null_mut(), &bi, DIB_RGB_COLORS, &mut bits, std::ptr::null_mut(), 0);
        if color.is_null() || bits.is_null() {
            return std::ptr::null_mut();
        }
        let dst = std::slice::from_raw_parts_mut(bits as *mut u8, (w * h * 4) as usize);
        for (d, s) in dst.chunks_exact_mut(4).zip(img.as_raw().chunks_exact(4)) {
            d.copy_from_slice(&[s[2], s[1], s[0], s[3]]);
        }
        let mask = CreateBitmap(w, h, 1, 1, std::ptr::null());
        let ii = ICONINFO { fIcon: 1, xHotspot: 0, yHotspot: 0, hbmMask: mask, hbmColor: color };
        let icon = CreateIconIndirect(&ii);
        DeleteObject(color);
        DeleteObject(mask);
        icon
    }
}

unsafe fn tray_add(h: HWND) {
    let mut nid = tray_data(h);
    nid.uFlags = NIF_ICON | NIF_MESSAGE | NIF_TIP;
    nid.uCallbackMessage = WM_TRAY;
    nid.hIcon = lasso_icon(GetSystemMetrics(SM_CXSMICON));
    if nid.hIcon.is_null() {
        let hinst = windows_sys::Win32::System::LibraryLoader::GetModuleHandleW(std::ptr::null());
        nid.hIcon = LoadIconW(hinst, 1 as _);
    }
    if nid.hIcon.is_null() {
        nid.hIcon = LoadIconW(std::ptr::null_mut(), IDI_APPLICATION);
    }
    for (i, c) in "Lasso - click: capture an area".encode_utf16().take(127).enumerate() {
        nid.szTip[i] = c;
    }
    Shell_NotifyIconW(NIM_ADD, &nid);
}

unsafe extern "system" fn tray_proc(h: HWND, msg: u32, w: WPARAM, l: LPARAM) -> LRESULT {
    let tc = TASKBAR_CREATED.load(Ordering::Relaxed);
    if tc != 0 && msg == tc {
        // Explorer restarted: the icon is gone, add it again
        tray_add(h);
        return 0;
    }
    if msg == WM_TRAY {
        let ev = (l & 0xffff) as u32;
        if ev == WM_LBUTTONUP {
            tray_send("flyout");
        } else if ev == WM_RBUTTONUP || ev == WM_CONTEXTMENU {
            let m = CreatePopupMenu();
            if let Some(u) = TRAY_UPDATE.lock().unwrap().clone() {
                let t = wide(&u);
                AppendMenuW(m, MF_STRING, 8, t.as_ptr());
                AppendMenuW(m, MF_SEPARATOR, 0, std::ptr::null());
            }
            let restore = TRAY_RESTORE.lock().unwrap().clone();
            let label = wide(&restore.clone().unwrap_or_else(|| "Restore last capture".into()));
            AppendMenuW(m, MF_STRING | if restore.is_some() { 0 } else { MF_GRAYED }, 1, label.as_ptr());
            AppendMenuW(m, MF_SEPARATOR, 0, std::ptr::null());
            for (id, t) in [(2, "Capture an area"), (3, "Full screenshot"), (4, "Capture a window")] {
                let t = wide(t);
                AppendMenuW(m, MF_STRING, id, t.as_ptr());
            }
            AppendMenuW(m, MF_SEPARATOR, 0, std::ptr::null());
            for (id, t) in [(5, "Library"), (6, "Settings"), (7, "Quit Lasso")] {
                let t = wide(t);
                AppendMenuW(m, MF_STRING, id, t.as_ptr());
            }
            let mut p = POINT { x: 0, y: 0 };
            GetCursorPos(&mut p);
            SetForegroundWindow(h);
            let cmd = TrackPopupMenu(m, TPM_RETURNCMD | TPM_RIGHTBUTTON, p.x, p.y, 0, h, std::ptr::null());
            // so the menu closes properly when the user clicks elsewhere next time (Shell_NotifyIcon docs)
            PostMessageW(h, WM_NULL, 0, 0);
            DestroyMenu(m);
            let c = match cmd {
                1 => "restore",
                2 => "region",
                3 => "full",
                4 => "window",
                5 => "library",
                6 => "settings",
                7 => "quit",
                8 => "update",
                _ => "",
            };
            if !c.is_empty() {
                tray_send(c);
            }
        }
        return 0;
    }
    DefWindowProcW(h, msg, w, l)
}

fn tray_data(h: HWND) -> NOTIFYICONDATAW {
    let mut nid: NOTIFYICONDATAW = unsafe { std::mem::zeroed() };
    nid.cbSize = std::mem::size_of::<NOTIFYICONDATAW>() as u32;
    nid.hWnd = h;
    nid.uID = 1;
    nid
}

pub fn start_tray(send: impl Fn(&str) + Send + Sync + 'static) {
    *TRAY_SEND.lock().unwrap() = Some(Box::new(send));
    // start-up work that needs no window: the GPU devices for the fast freeze, the autostart path
    dxgi::warm();
    refresh_autostart();
    std::thread::spawn(|| unsafe {
        let hinst = windows_sys::Win32::System::LibraryLoader::GetModuleHandleW(std::ptr::null());
        let class = wide("LassoTray");
        let mut wc: WNDCLASSW = std::mem::zeroed();
        wc.lpfnWndProc = Some(tray_proc);
        wc.hInstance = hinst;
        wc.lpszClassName = class.as_ptr();
        RegisterClassW(&wc);
        let tc = wide("TaskbarCreated");
        TASKBAR_CREATED.store(RegisterWindowMessageW(tc.as_ptr()), Ordering::Relaxed);
        // a real (hidden) top-level window: message-only windows don't get "TaskbarCreated"
        let h = CreateWindowExW(0, class.as_ptr(), class.as_ptr(), 0, 0, 0, 0, 0, std::ptr::null_mut(), std::ptr::null_mut(), hinst, std::ptr::null());
        *TRAY_HWND.lock().unwrap() = h as usize;
        tray_add(h);
        let mut msg: MSG = std::mem::zeroed();
        while GetMessageW(&mut msg, std::ptr::null_mut(), 0, 0) > 0 {
            TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    });
}

pub fn notify(title: &str, body: &str) {
    let h = tray_hwnd();
    if h.is_null() {
        return;
    }
    let mut nid = tray_data(h);
    nid.uFlags = NIF_INFO;
    nid.dwInfoFlags = NIIF_INFO;
    for (i, c) in title.encode_utf16().take(63).enumerate() {
        nid.szInfoTitle[i] = c;
    }
    for (i, c) in body.encode_utf16().take(255).enumerate() {
        nid.szInfo[i] = c;
    }
    unsafe { Shell_NotifyIconW(NIM_MODIFY, &nid) };
}

// ---------------------------------------------------------------------------------------------------------------
// Clipboard (win/clip.rs), recycle bin, dialogs

/// A finished picture (encoded bytes): PNG + DIB, decoded and written on the clipboard thread, not the UI thread
pub fn clipboard_set_image(bytes: Vec<u8>) {
    clip::set_image(bytes, None);
}

/// Ctrl+V: a picture, a copied picture file, or a DIB
pub fn clipboard_read_image() -> Result<Pic, String> {
    clip::read_image()
}

pub fn clipboard_files(paths: &[&str]) {
    clip::set_files(paths);
}

/// Drag a picture file out of Lasso (the corner thumbnail) with the shell's own data object + drag loop - the same
/// thing Explorer hands out, so Discord / browsers / Explorer / Word take it as a file. Runs the modal drag loop
/// (returns after the drop). None = couldn't start; Some(true) = dropped somewhere that took it. ⚠ untested on Windows.
pub fn drag_file(path: &std::path::Path) -> Option<bool> {
    use windows_sys::core::GUID;
    use windows_sys::Win32::System::Ole::{DROPEFFECT, DROPEFFECT_COPY};
    use windows_sys::Win32::UI::Shell::{ILClone, ILCreateFromPathW, ILFindLastID, ILFree, ILRemoveLastID, SHCreateDataObject, SHDoDragDrop};
    // IID_IDataObject {0000010e-0000-0000-C000-000000000046}
    const IID_IDATAOBJECT: GUID = GUID { data1: 0x0000010e, data2: 0, data3: 0, data4: [0xC0, 0, 0, 0, 0, 0, 0, 0x46] };
    let full = std::path::absolute(path).ok()?;
    let w = wide(&full.to_string_lossy());
    unsafe {
        let abs = ILCreateFromPathW(w.as_ptr());
        if abs.is_null() {
            return None;
        }
        let parent = ILClone(abs);
        ILRemoveLastID(parent);
        let child = ILFindLastID(abs) as *const _;
        let mut obj: *mut core::ffi::c_void = std::ptr::null_mut();
        let hr = SHCreateDataObject(parent, 1, &child, std::ptr::null_mut(), &IID_IDATAOBJECT, &mut obj);
        let mut res = None;
        if hr >= 0 && !obj.is_null() {
            let mut eff: DROPEFFECT = 0;
            // no drop source of our own: the shell's default one (left button, Esc cancels)
            let hr = SHDoDragDrop(std::ptr::null_mut(), obj, std::ptr::null_mut(), DROPEFFECT_COPY, &mut eff);
            // DRAGDROP_S_DROP = 0x40100, DRAGDROP_S_CANCEL = 0x40101
            res = Some(hr == 0x40100 && eff != 0);
            // IUnknown::Release is the 3rd entry of the vtable
            let vtbl = *(obj as *const *const usize);
            let release: extern "system" fn(*mut core::ffi::c_void) -> u32 = std::mem::transmute(*vtbl.add(2));
            release(obj);
        }
        ILFree(parent);
        ILFree(abs);
        res
    }
}

pub fn recycle(p: &std::path::Path) -> Result<(), String> {
    use windows_sys::Win32::UI::Shell::{SHFileOperationW, FOF_ALLOWUNDO, FOF_NOCONFIRMATION, FOF_NOERRORUI, FOF_SILENT, FO_DELETE, SHFILEOPSTRUCTW};
    // SHFileOperation wants a full path (a relative one is deleted for good instead of going to the bin)
    let full = std::path::absolute(p).unwrap_or_else(|_| p.to_path_buf());
    let mut from: Vec<u16> = full.to_string_lossy().encode_utf16().collect();
    from.push(0);
    from.push(0);
    let mut op: SHFILEOPSTRUCTW = unsafe { std::mem::zeroed() };
    op.wFunc = FO_DELETE;
    op.pFrom = from.as_ptr();
    op.fFlags = (FOF_ALLOWUNDO | FOF_NOCONFIRMATION | FOF_SILENT | FOF_NOERRORUI) as u16;
    let r = unsafe { SHFileOperationW(&mut op) };
    if r == 0 && op.fAnyOperationsAborted == 0 {
        Ok(())
    } else {
        Err(format!("can't move to the recycle bin ({r})"))
    }
}

/// The common dialogs (folder picker, Open, Save as). Run on their own thread with COM as single-threaded
/// apartment: SHBrowseForFolder (new dialog style) and the Explorer-style Open / Save dialogs need it, and the
/// callers' threads (std::thread::spawn) have no COM at all or could be MTA.
pub fn file_dialog(save: bool, title: &str, name: &str, folder: &std::path::Path) -> Option<std::path::PathBuf> {
    std::thread::scope(|s| s.spawn(|| in_sta(|| file_dialog_sta(save, title, name, folder))).join().ok().flatten())
}

fn in_sta<T>(f: impl FnOnce() -> T) -> T {
    use windows_sys::Win32::System::Com::{CoInitializeEx, CoUninitialize, COINIT_APARTMENTTHREADED, COINIT_DISABLE_OLE1DDE};
    // S_OK / S_FALSE = initialised here (uninit after); RPC_E_CHANGED_MODE can't happen on this fresh thread
    let hr = unsafe { CoInitializeEx(std::ptr::null(), (COINIT_APARTMENTTHREADED | COINIT_DISABLE_OLE1DDE) as u32) };
    if hr < 0 {
        crate::log::line("dialog", &format!("COM init failed ({hr:#x})"));
    }
    let r = f();
    if hr >= 0 {
        unsafe { CoUninitialize() };
    }
    r
}

fn file_dialog_sta(save: bool, title: &str, name: &str, folder: &std::path::Path) -> Option<std::path::PathBuf> {
    use windows_sys::Win32::UI::Controls::Dialogs::*;
    if name == "<folder>" {
        use windows_sys::Win32::UI::Shell::{SHBrowseForFolderW, SHGetPathFromIDListW, BIF_NEWDIALOGSTYLE, BIF_RETURNONLYFSDIRS, BROWSEINFOW};
        let t = wide(title);
        let mut bi: BROWSEINFOW = unsafe { std::mem::zeroed() };
        bi.lpszTitle = t.as_ptr();
        bi.ulFlags = BIF_RETURNONLYFSDIRS | BIF_NEWDIALOGSTYLE;
        let pidl = unsafe { SHBrowseForFolderW(&bi) };
        if pidl.is_null() {
            return None;
        }
        let mut buf = vec![0u16; 1024];
        unsafe { SHGetPathFromIDListW(pidl, buf.as_mut_ptr()) };
        unsafe { windows_sys::Win32::System::Com::CoTaskMemFree(pidl as *const _) };
        let n = buf.iter().position(|c| *c == 0).unwrap_or(0);
        return (n > 0).then(|| std::path::PathBuf::from(String::from_utf16_lossy(&buf[..n])));
    }
    // Save: the name may carry the kind of the bytes ("shot.jpg"); a name typed without an extension gets that one
    // (lpstrDefExt), never a fixed .png
    let (file, ext) = crate::dialogs::save_name(name, "png");
    let mut buf = vec![0u16; 2048];
    if save {
        for (i, c) in file.encode_utf16().take(2000).enumerate() {
            buf[i] = c;
        }
    }
    let filter: Vec<u16> = if save {
        format!("{} picture\0*.{ext}\0All files\0*.*\0\0", ext.to_ascii_uppercase()).encode_utf16().collect()
    } else {
        "Pictures\0*.png;*.jpg;*.jpeg;*.webp;*.bmp;*.gif\0All files\0*.*\0\0".encode_utf16().collect()
    };
    let t = wide(title);
    let dir = wide(&folder.to_string_lossy());
    let ext = wide(&ext);
    let mut of: OPENFILENAMEW = unsafe { std::mem::zeroed() };
    of.lStructSize = std::mem::size_of::<OPENFILENAMEW>() as u32;
    of.lpstrFilter = filter.as_ptr();
    of.nFilterIndex = 1;
    of.lpstrFile = buf.as_mut_ptr();
    of.nMaxFile = buf.len() as u32;
    of.lpstrTitle = t.as_ptr();
    of.lpstrInitialDir = dir.as_ptr();
    of.lpstrDefExt = ext.as_ptr();
    of.Flags = OFN_EXPLORER | OFN_NOCHANGEDIR | if save { OFN_OVERWRITEPROMPT } else { OFN_FILEMUSTEXIST };
    let ok = unsafe { if save { GetSaveFileNameW(&mut of) } else { GetOpenFileNameW(&mut of) } };
    if ok == 0 {
        return None;
    }
    let n = buf.iter().position(|c| *c == 0).unwrap_or(0);
    Some(std::path::PathBuf::from(String::from_utf16_lossy(&buf[..n])))
}

// ---------------------------------------------------------------------------------------------------------------
// "Start with your PC": HKCU\...\Run, value "Lasso" = "<exe>"

const RUN_KEY: &str = "Software\\Microsoft\\Windows\\CurrentVersion\\Run";

fn run_value() -> Option<String> {
    use windows_sys::Win32::System::Registry::{RegGetValueW, HKEY_CURRENT_USER, RRF_RT_REG_SZ};
    let key = wide(RUN_KEY);
    let name = wide("Lasso");
    let mut buf = vec![0u16; 1024];
    let mut len = (buf.len() * 2) as u32;
    let r = unsafe { RegGetValueW(HKEY_CURRENT_USER, key.as_ptr(), name.as_ptr(), RRF_RT_REG_SZ, std::ptr::null_mut(), buf.as_mut_ptr() as *mut _, &mut len) };
    if r != 0 {
        return None;
    }
    let n = buf.iter().position(|c| *c == 0).unwrap_or(0);
    Some(String::from_utf16_lossy(&buf[..n]))
}

pub fn set_autostart(on: bool) {
    use windows_sys::Win32::System::Registry::{RegDeleteKeyValueW, RegSetKeyValueW, HKEY_CURRENT_USER, REG_SZ};
    let key = wide(RUN_KEY);
    let name = wide("Lasso");
    unsafe {
        if on {
            let exe = std::env::current_exe().map(|p| p.to_string_lossy().to_string()).unwrap_or_default();
            let v = wide(&format!("\"{exe}\""));
            RegSetKeyValueW(HKEY_CURRENT_USER, key.as_ptr(), name.as_ptr(), REG_SZ, v.as_ptr() as *const _, (v.len() * 2) as u32);
        } else {
            RegDeleteKeyValueW(HKEY_CURRENT_USER, key.as_ptr(), name.as_ptr());
        }
    }
}

/// Lasso moved (new install folder): point "Start with your PC" at this exe again. Only when the old path is gone
/// and this is a real install (lasso.exe, no LASSO_HOME test copy) - a test copy never takes the user's entry.
fn refresh_autostart() {
    let Some(v) = run_value() else { return };
    let old = v.trim().trim_matches('"').to_string();
    let Ok(exe) = std::env::current_exe() else { return };
    let is_real = exe.file_name().is_some_and(|n| n.eq_ignore_ascii_case("lasso.exe")) && std::env::var_os("LASSO_HOME").is_none();
    if is_real && !old.is_empty() && !std::path::Path::new(&old).exists() && !exe.to_string_lossy().eq_ignore_ascii_case(&old) {
        crate::log::line("app", "Start with your PC pointed at an old path - updated");
        set_autostart(true);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hotkeys_parse() {
        use windows_sys::Win32::UI::Input::KeyboardAndMouse::*;
        assert_eq!(parse_hotkey("Ctrl+Shift+Print"), Some((MOD_NOREPEAT | MOD_CONTROL | MOD_SHIFT, VK_SNAPSHOT as u32)));
        assert_eq!(parse_hotkey("Alt+F12"), Some((MOD_NOREPEAT | MOD_ALT, VK_F12 as u32)));
        assert_eq!(parse_hotkey("Win+Shift+s"), Some((MOD_NOREPEAT | MOD_WIN | MOD_SHIFT, 'S' as u32)));
        assert_eq!(parse_hotkey("Ctrl+["), None);
        assert_eq!(parse_hotkey("F25"), None);
        assert_eq!(parse_hotkey(""), None);
    }
}
