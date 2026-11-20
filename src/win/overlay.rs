//! The frozen screen's window on Windows (Slint on winit): borderless, topmost, exactly over its monitor, keyboard
//! focus at once, and up fast.
//! - Made ahead of time: `prewarm` shows each monitor's overlay once far off screen (no taskbar button, not
//!   activated - see `init`'s window hook in win.rs) and hides it again, so the winit window and its OpenGL context
//!   exist before the first capture.
//! - No fullscreen switch and no fade: placed with SetWindowPos (HWND_TOPMOST, physical px of the monitor) and
//!   DWM's show / hide animation is turned off for it.
//! - No flash of the previous capture: a shown-again window first shows what it drew last time, so it is cloaked
//!   (DWM keeps it invisible) until Slint has drawn the new frame, then uncloaked (a 150 ms safety timer uncloaks
//!   it in any case - never an invisible window over the screen).
//! - Out of the taskbar (winit hook + set_skip_taskbar) and out of Alt+Tab (WS_EX_TOOLWINDOW after each show).
//! - Keys: Windows only lets the foreground app give the focus away; the hotkey / tray click makes Lasso that app,
//!   a `lasso --region` from another program allows it (main side, `allow_foreground`), and as a last resort the
//!   input queues are joined for a moment (AttachThreadInput).
//! ⚠ Type-checked from Linux only; never run on a real Windows PC yet.

use slint::winit_030::winit::raw_window_handle::{HasWindowHandle, RawWindowHandle};
use slint::winit_030::{winit, WinitWindowAccessor};
use slint::ComponentHandle;
use std::cell::RefCell;
use std::time::Duration;
use windows_sys::Win32::Foundation::{HWND, RECT};
use windows_sys::Win32::Graphics::Dwm::{DwmSetWindowAttribute, DWMWA_CLOAK, DWMWA_TRANSITIONS_FORCEDISABLED};
use windows_sys::Win32::System::Threading::{AttachThreadInput, GetCurrentThreadId};
use windows_sys::Win32::UI::Input::KeyboardAndMouse::SetFocus;
use windows_sys::Win32::UI::WindowsAndMessaging::*;

thread_local! {
    /// Slint windows a real capture has shown (by address): the warm-up never touches those again
    static USED: RefCell<Vec<usize>> = const { RefCell::new(Vec::new()) };
    /// overlay windows set up (DWM attributes + the "drawn" notifier)
    static READY: RefCell<Vec<usize>> = const { RefCell::new(Vec::new()) };
    /// Slint windows whose close request (Alt+F4) is Esc
    static CLOSE_HOOKED: RefCell<Vec<usize>> = const { RefCell::new(Vec::new()) };
    /// cloaked until their next frame is drawn
    static CLOAKED: RefCell<Vec<usize>> = const { RefCell::new(Vec::new()) };
}

fn key(w: &slint::Window) -> usize {
    w as *const slint::Window as usize
}

/// The Win32 window of a Slint window (None until winit made it)
pub fn hwnd(w: &slint::Window) -> Option<HWND> {
    w.with_winit_window(|ww| match ww.window_handle().ok()?.as_raw() {
        RawWindowHandle::Win32(h) => Some(h.hwnd.get() as HWND),
        _ => None,
    })
    .flatten()
}

fn dwm_set(h: HWND, attr: i32, on: bool) {
    let v: i32 = on as i32;
    unsafe { DwmSetWindowAttribute(h, attr as u32, &v as *const i32 as *const _, 4) };
}

fn uncloak(h: HWND) {
    let was = CLOAKED.with(|c| {
        let mut c = c.borrow_mut();
        let n = c.len();
        c.retain(|x| *x != h as usize);
        n != c.len()
    });
    if was {
        dwm_set(h, DWMWA_CLOAK, false);
        if unsafe { IsWindowVisible(h) } != 0 && wants_focus(h) {
            force_foreground(h);
        }
    }
}

/// With several monitors every one gets a frozen screen; the keyboard goes to the one under the mouse
fn wants_focus(h: HWND) -> bool {
    unsafe {
        let mut p = windows_sys::Win32::Foundation::POINT { x: 0, y: 0 };
        let mut r: RECT = std::mem::zeroed();
        if GetCursorPos(&mut p) == 0 || GetWindowRect(h, &mut r) == 0 {
            return true;
        }
        p.x >= r.left && p.x < r.right && p.y >= r.top && p.y < r.bottom
    }
}

/// Once per overlay window: no DWM animation, and "a frame was drawn" ends the cloak
fn setup(w: &slint::Window, h: HWND) {
    let k = h as usize;
    if READY.with(|r| r.borrow().contains(&k)) {
        return;
    }
    READY.with(|r| r.borrow_mut().push(k));
    dwm_set(h, DWMWA_TRANSITIONS_FORCEDISABLED, true);
    w.with_winit_window(|ww| {
        use winit::platform::windows::WindowExtWindows;
        ww.set_skip_taskbar(true);
        ww.set_window_level(winit::window::WindowLevel::AlwaysOnTop);
    });
    let r = w.set_rendering_notifier(move |state, _| {
        if matches!(state, slint::RenderingState::AfterRendering) && CLOAKED.with(|c| c.borrow().contains(&k)) {
            // next turn of the loop = after the frame was handed to the screen
            slint::Timer::single_shot(Duration::ZERO, move || uncloak(k as HWND));
        }
    });
    if r.is_err() {
        crate::log::line("overlay", "no draw notifier - uncloaking by timer only");
    }
}

/// Alt+F4 = Esc: Slint would just hide the window and Lasso would think the frozen screen is still up
fn hook_close<C: ComponentHandle + 'static>(comp: &C) {
    let k = key(comp.window());
    if CLOSE_HOOKED.with(|c| c.borrow().contains(&k)) {
        return;
    }
    CLOSE_HOOKED.with(|c| c.borrow_mut().push(k));
    let weak = comp.as_weak();
    comp.window().on_close_requested(move || {
        if let Some(c) = weak.upgrade() {
            let esc: slint::SharedString = slint::platform::Key::Escape.into();
            c.window().dispatch_event(slint::platform::WindowEvent::KeyPressed { text: esc.clone() });
            c.window().dispatch_event(slint::platform::WindowEvent::KeyReleased { text: esc });
        }
        slint::CloseRequestResponse::KeepWindowShown
    });
}

/// Out of Alt+Tab: a tool window. winit rewrites the extended style on every show / hide (any change of its window
/// flags), so this is set again after each show; a hidden window isn't in Alt+Tab anyway.
fn tool_window(h: HWND) {
    unsafe {
        let ex = GetWindowLongPtrW(h, GWL_EXSTYLE) as u32;
        let want = (ex | WS_EX_TOOLWINDOW) & !WS_EX_APPWINDOW;
        if want != ex {
            SetWindowLongPtrW(h, GWL_EXSTYLE, want as isize);
        }
    }
}

/// SetWindowPos twice: moving onto a monitor with another scale makes Windows (and winit) resize the window for
/// the new DPI; the second call puts the exact size back.
fn place(h: HWND, r: &RECT, show: bool) {
    let flags = SWP_NOACTIVATE | if show { SWP_SHOWWINDOW | SWP_FRAMECHANGED } else { 0 };
    for _ in 0..2 {
        unsafe { SetWindowPos(h, HWND_TOPMOST, r.left, r.top, r.right - r.left, r.bottom - r.top, flags) };
    }
}

/// Bring our window to the front with the keyboard focus, also when Windows' foreground lock says no.
pub fn force_foreground(h: HWND) {
    unsafe {
        if GetForegroundWindow() == h {
            return;
        }
        if SetForegroundWindow(h) != 0 && GetForegroundWindow() == h {
            return;
        }
        let fg = GetForegroundWindow();
        let fg_thread = if fg.is_null() { 0 } else { GetWindowThreadProcessId(fg, std::ptr::null_mut()) };
        let me = GetCurrentThreadId();
        if fg_thread != 0 && fg_thread != me && AttachThreadInput(me, fg_thread, 1) != 0 {
            BringWindowToTop(h);
            SetForegroundWindow(h);
            SetFocus(h);
            AttachThreadInput(me, fg_thread, 0);
        } else {
            BringWindowToTop(h);
            SetForegroundWindow(h);
        }
    }
}

/// Make the overlay's window now (off screen, never seen), so the first capture doesn't wait for it.
pub fn prewarm<C: ComponentHandle + 'static>(comp: &C) {
    let weak = comp.as_weak();
    slint::Timer::single_shot(Duration::from_millis(250), move || {
        let Some(c) = weak.upgrade() else { return };
        let w = c.window();
        if USED.with(|u| u.borrow().contains(&key(w))) || w.is_visible() || hwnd(w).is_some() {
            return;
        }
        w.set_position(slint::PhysicalPosition::new(-32000, -32000));
        w.set_size(slint::PhysicalSize::new(64, 64));
        let _ = c.show();
        let weak2 = weak.clone();
        slint::Timer::single_shot(Duration::from_millis(300), move || {
            let Some(c) = weak2.upgrade() else { return };
            let w = c.window();
            if let Some(h) = hwnd(w) {
                setup(w, h);
            }
            if !USED.with(|u| u.borrow().contains(&key(w))) {
                let _ = c.hide();
            }
        });
    });
}

/// Show the overlay over the monitor with physical rect `r`, focused.
pub fn show<C: ComponentHandle + 'static>(comp: &C, r: RECT) {
    hook_close(comp);
    let w = comp.window();
    let k = key(w);
    USED.with(|u| {
        if !u.borrow().contains(&k) {
            u.borrow_mut().push(k);
        }
    });
    if let Some(h) = hwnd(w) {
        setup(w, h);
        if !w.is_visible() {
            dwm_set(h, DWMWA_CLOAK, true);
            CLOAKED.with(|c| c.borrow_mut().push(h as usize));
            slint::Timer::single_shot(Duration::from_millis(150), move || uncloak(h));
        }
        place(h, &r, false);
        let _ = comp.show();
        tool_window(h);
        place(h, &r, true);
        if wants_focus(h) {
            force_foreground(h);
        }
        w.request_redraw();
        return;
    }
    // not made yet (a capture right at start): let winit make it at the right place, finish once it exists
    w.set_position(slint::PhysicalPosition::new(r.left, r.top));
    w.set_size(slint::PhysicalSize::new((r.right - r.left).max(1) as u32, (r.bottom - r.top).max(1) as u32));
    let _ = comp.show();
    let weak = comp.as_weak();
    let finish = move || {
        let Some(c) = weak.upgrade() else { return false };
        let w = c.window();
        let Some(h) = hwnd(w) else { return false };
        setup(w, h);
        if w.is_visible() {
            tool_window(h);
            place(h, &r, true);
            if wants_focus(h) {
                force_foreground(h);
            }
        }
        true
    };
    if !finish() {
        let f = std::rc::Rc::new(finish);
        for ms in [0u64, 50, 150] {
            let f = f.clone();
            slint::Timer::single_shot(Duration::from_millis(ms), move || {
                f();
            });
        }
    }
}
