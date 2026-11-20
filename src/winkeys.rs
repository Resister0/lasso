//! winit path only (Windows, GNOME): remember the physical key of each key press, for the layout fallback
//! (keys.rs). On KDE the layer-shell platform gets the physical key from Wayland itself.

use slint::ComponentHandle;
use std::cell::RefCell;
use std::rc::Rc;

pub fn hook_physical<C: ComponentHandle>(comp: &C, phys: Rc<RefCell<Option<char>>>) {
    #[cfg(target_os = "linux")]
    if crate::wl::active() {
        let _ = (comp, phys);
        return;
    }
    use slint::winit_030::{winit, EventResult, WinitWindowAccessor};
    comp.window().on_winit_window_event(move |_, ev| {
        if let winit::event::WindowEvent::KeyboardInput { event, .. } = ev {
            if let winit::keyboard::PhysicalKey::Code(c) = event.physical_key {
                *phys.borrow_mut() = us_char(c);
            }
        }
        EventResult::Propagate
    });
}

fn us_char(c: slint::winit_030::winit::keyboard::KeyCode) -> Option<char> {
    let s = format!("{c:?}");
    if let Some(l) = s.strip_prefix("Key") {
        return l.chars().next().map(|x| x.to_ascii_lowercase());
    }
    if let Some(d) = s.strip_prefix("Digit") {
        return d.chars().next();
    }
    match s.as_str() {
        "BracketLeft" => Some('['),
        "BracketRight" => Some(']'),
        _ => None,
    }
}
