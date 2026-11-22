//! Keys on the frozen screen: one key event shape for every platform + the changeable key map.
//!
//! Why keys often fail in Linux screenshot tools (notes section 3) and what this file does about it:
//! - Ctrl+letter arrives as a control character (Ctrl+B = "\u{2}") in Slint / winit: `from_slint()` maps 1..26
//!   back to a..z (Echo's edui.rs fix). On KDE the overlay is Lasso's own layer-shell surface and keys come
//!   straight from xkb as key names, so there is no control character at all.
//! - Keyboard layouts: the letter is matched first (AZERTY users press the key that SAYS Z), and only when no
//!   binding matches, the physical key's US letter is tried (Russian / Greek layouts give "я" for Ctrl+Z).
//! - No Meta / Print keys in the defaults: the desktop keeps those.

use std::collections::BTreeMap;

#[derive(Clone, Debug, Default, PartialEq)]
pub struct KeyEv {
    /// "a".."z", "1", "[", "?", or a name: Escape Return Tab BackSpace Delete Left Right Up Down F1..F12 space
    /// Home End plus minus equal
    pub key: String,
    /// The physical key's letter / digit on a US keyboard ('a', '1', ...), for the layout fallback.
    pub phys: Option<char>,
    /// What the key types (the text tool), empty when Ctrl / Alt is held.
    pub text: String,
    pub ctrl: bool,
    pub shift: bool,
    pub alt: bool,
    pub repeat: bool,
}

impl KeyEv {
    pub fn named(key: &str) -> KeyEv {
        KeyEv { key: key.into(), ..Default::default() }
    }
    pub fn is(&self, k: &str) -> bool {
        self.key.eq_ignore_ascii_case(k)
    }
    pub fn plain(&self) -> bool {
        !self.ctrl && !self.alt
    }
}

/// Every action that has a key. The id is what config.ini stores (`key.<id>=...`), never rename.
pub const ACTIONS: &[(&str, &str, &[&str])] = &[
    ("select", "Select / move", &["V"]),
    ("crop", "Crop (Enter applies, Esc cancels)", &["C"]),
    ("pen", "Pen", &["F", "Alt+1"]),
    ("highlight", "Highlighter", &["H", "Alt+2"]),
    ("line", "Line", &["L", "Alt+3"]),
    ("arrow", "Arrow", &["A", "Alt+4"]),
    ("rect", "Rectangle", &["R", "Alt+5"]),
    ("ellipse", "Ellipse", &["E", "Alt+6"]),
    ("text", "Text", &["T", "Alt+7"]),
    ("step", "Number step (1, 2, 3...)", &["N", "Alt+8"]),
    ("blur", "Blur / pixelate (B again switches)", &["B", "Alt+9"]),
    ("spotlight", "Spotlight", &["S", "Alt+0"]),
    ("picker", "Colour picker (copies #rrggbb, draws with it)", &["I"]),
    ("color1", "Colour 1", &["1"]),
    ("color2", "Colour 2", &["2"]),
    ("color3", "Colour 3", &["3"]),
    ("color4", "Colour 4", &["4"]),
    ("color5", "Colour 5", &["5"]),
    ("color6", "Colour 6", &["6"]),
    ("color7", "Colour 7", &["7"]),
    ("color8", "Colour 8", &["8"]),
    ("thinner", "Thinner / smaller (Shift: big steps)", &["["]),
    ("thicker", "Thicker / bigger (Shift: big steps)", &["]"]),
    ("delete", "Remove the selected shape", &["Delete", "BackSpace"]),
    ("select_all", "Select all shapes", &["Ctrl+A"]),
    ("duplicate", "Duplicate the selected shape", &["Ctrl+D"]),
    ("undo", "Undo", &["Ctrl+Z"]),
    ("redo", "Redo", &["Ctrl+Shift+Z", "Ctrl+Y"]),
    ("copy", "Copy the picture", &["Ctrl+C"]),
    ("copy_text", "Copy the text in it (OCR)", &["Ctrl+Shift+C"]),
    ("redact", "Find and blur emails, numbers, keys", &["Ctrl+Shift+R"]),
    ("paste", "Paste a picture as a shape", &["Ctrl+V"]),
    ("save", "Save (no dialog)", &["Ctrl+S"]),
    ("save_as", "Save as...", &["Ctrl+Shift+S"]),
    ("save_copy", "Save as a copy (a new picture in the library, this one stays as it was)", &["Ctrl+Shift+D"]),
    ("link", "Copy share link", &["Ctrl+L"]),
    ("pin", "Pin to the screen", &["Ctrl+T"]),
    ("new_box", "Throw this away, pick a new box", &["Ctrl+N"]),
    ("open", "Open a picture", &["Ctrl+O"]),
    ("library", "Library", &["Ctrl+R"]),
    ("bold", "Bold (text)", &["Ctrl+B"]),
    ("italic", "Italic (text)", &["Ctrl+I"]),
    ("zoom_in", "Zoom in", &["Ctrl+plus", "Ctrl+equal"]),
    ("zoom_out", "Zoom out", &["Ctrl+minus"]),
    ("zoom_fit", "Fit to the screen", &["Ctrl+0"]),
    ("zoom_100", "100 %", &["Ctrl+1"]),
    ("background", "Background (padding + shadow)", &["Ctrl+G"]),
    // power-user keys (the user, 13 Nov 2026: "some cool power user control keybinds")
    ("copy_save", "Copy + save + close", &["Ctrl+Return"]),
    ("copy_stay", "Copy, keep editing", &["Ctrl+Alt+C"]),
    ("hide_bars", "Hide / show the bars (see the whole picture)", &["Ctrl+H"]),
    ("last_box", "Use the last box again", &["Ctrl+Shift+B"]),
    ("whole_screen", "Box = the whole screen", &["Ctrl+Shift+A"]),
    ("scroll", "Scrolling capture (scroll the page, Lasso adds it)", &["Ctrl+Alt+S"]),
    ("take_colour", "Draw with the colour under the mouse", &["Ctrl+Shift+I"]),
    ("next_shape", "Select the next shape", &["Ctrl+Tab"]),
    ("prev_shape", "Select the previous shape", &["Ctrl+Shift+Tab"]),
    ("front", "Bring the shape to the front", &["Ctrl+]"]),
    ("back", "Send the shape to the back", &["Ctrl+["]),
    ("done", "Done (the main action)", &["Return"]),
    ("close", "Close (Restore brings it back)", &["Escape"]),
    ("discard", "Throw it away (no Restore, not in the library)", &["Shift+Escape"]),
    ("mode", "Area / Window / Screen", &["Tab"]),
    ("mode_back", "Area / Window / Screen, backwards", &["Shift+Tab"]),
    ("keys", "Show all keys", &["?", "F1"]),
];

/// Every action that can be a button in the bar under the box (Settings > Capture > "Buttons under the box";
/// 21 Nov 2026: "why not all the functions available in the app addable and removable"). (id, button text).
/// The id is the editor action, so a button does exactly what its key does.
pub const BAR_ACTIONS: [(&str, &str); 23] = [
    ("pin", "Pin"),
    ("save", "Save"),
    ("save_as", "Save as"),
    ("save_copy", "Save as a copy"),
    ("copy", "Copy"),
    ("copy_stay", "Copy, keep editing"),
    ("copy_save", "Copy + save"),
    ("copy_text", "Copy text"),
    ("link", "Share link"),
    ("redact", "Redact"),
    ("background", "Background"),
    ("scroll", "Scrolling capture"),
    ("new_box", "New box"),
    ("last_box", "Last box"),
    ("whole_screen", "Whole screen"),
    ("hide_bars", "Hide bars"),
    ("undo", "Undo"),
    ("redo", "Redo"),
    ("library", "Library"),
    ("open", "Open a picture"),
    ("discard", "Throw away"),
    ("close", "Close"),
    ("done", "Done"),
];

/// The bar's buttons in order, from config `bar` ("save,copy,link,done"); unknown ids dropped, no doubles
pub fn bar_list(cfg_value: &str) -> Vec<&'static str> {
    let mut v: Vec<&'static str> = vec![];
    for id in cfg_value.split(',').map(str::trim) {
        if let Some((k, _)) = BAR_ACTIONS.iter().find(|(k, _)| *k == id) {
            if !v.contains(k) {
                v.push(k);
            }
        }
    }
    v
}

/// "add:pin" / "remove:pin" / "left:pin" / "right:pin" on the list -> the new config value
pub fn bar_edit(cfg_value: &str, op: &str, id: &str) -> String {
    let mut v = bar_list(cfg_value);
    let at = v.iter().position(|k| *k == id);
    match (op, at) {
        ("add", None) => {
            if let Some((k, _)) = BAR_ACTIONS.iter().find(|(k, _)| *k == id) {
                // before Done when Done is last (Done stays the right-most button)
                let pos = if v.last() == Some(&"done") { v.len() - 1 } else { v.len() };
                v.insert(pos, k);
            }
        }
        ("remove", Some(i)) => {
            v.remove(i);
        }
        ("left", Some(i)) if i > 0 => v.swap(i, i - 1),
        ("right", Some(i)) if i + 1 < v.len() => v.swap(i, i + 1),
        _ => {}
    }
    v.join(",")
}

pub fn bar_text(id: &str) -> &'static str {
    BAR_ACTIONS.iter().find(|(k, _)| *k == id).map(|(_, t)| *t).unwrap_or("")
}

pub fn action_name(id: &str) -> &'static str {
    ACTIONS.iter().find(|a| a.0 == id).map(|a| a.1).unwrap_or("")
}

/// "ctrl+shift+Z" -> "Ctrl+Shift+z" (modifiers in a fixed order, key lower case). None for junk.
pub fn canon(combo: &str) -> Option<String> {
    let mut ctrl = false;
    let mut alt = false;
    let mut shift = false;
    let mut key = None;
    let parts: Vec<&str> = if combo.trim() == "+" { vec!["plus"] } else { combo.split('+').map(|s| s.trim()).collect() };
    for p in parts {
        match p.to_ascii_lowercase().as_str() {
            "ctrl" | "control" => ctrl = true,
            "alt" => alt = true,
            "shift" => shift = true,
            "" => return None,
            _ => key = Some(norm_key(p)),
        }
    }
    let key = key?;
    Some(build(ctrl, alt, shift, &key))
}

fn norm_key(k: &str) -> String {
    let l = k.to_lowercase();
    match l.as_str() {
        "esc" => "escape".into(),
        "enter" | "kp_enter" => "return".into(),
        "del" => "delete".into(),
        "backspace" => "backspace".into(),
        "=" => "equal".into(),
        "-" => "minus".into(),
        " " => "space".into(),
        "kp_add" => "plus".into(),
        "kp_subtract" => "minus".into(),
        "iso_left_tab" => "tab".into(),
        "left" | "right" | "up" | "down" => l,
        _ => l,
    }
}

fn build(ctrl: bool, alt: bool, shift: bool, key: &str) -> String {
    let mut s = String::new();
    if ctrl {
        s += "Ctrl+";
    }
    if alt {
        s += "Alt+";
    }
    if shift {
        s += "Shift+";
    }
    s + key
}

/// Shift counts for letters and named keys; for symbols the character already says it ("?" is Shift+/).
fn shift_counts(key: &str) -> bool {
    let mut it = key.chars();
    match (it.next(), it.next()) {
        (Some(c), None) => c.is_ascii_alphabetic(),
        _ => key != "plus" && key != "minus" && key != "equal",
    }
}

/// How a key looks on the key sheet: "Ctrl+Shift+z" -> "Ctrl ⇧ Z"
pub fn pretty(combo: &str) -> String {
    let c = canon(combo).unwrap_or_else(|| combo.to_string());
    c.split('+')
        .map(|p| match p {
            "Shift" => "⇧".to_string(),
            "escape" => "Esc".into(),
            "return" => "Enter".into(),
            "backspace" => "Backspace".into(),
            "delete" => "Del".into(),
            "tab" => "Tab".into(),
            "plus" => "+".into(),
            "minus" => "−".into(),
            "equal" => "=".into(),
            "space" => "Space".into(),
            k if k.len() == 1 => k.to_uppercase(),
            k if k.starts_with('f') && k[1..].parse::<u32>().is_ok() => k.to_uppercase(),
            k => {
                let mut c = k.chars();
                c.next().map(|f| f.to_uppercase().collect::<String>() + c.as_str()).unwrap_or_default()
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

#[derive(Clone, Debug)]
pub struct Keymap {
    /// action id -> combos (canonical)
    pub binds: BTreeMap<String, Vec<String>>,
}

impl Default for Keymap {
    fn default() -> Self {
        let mut binds = BTreeMap::new();
        for (id, _, keys) in ACTIONS {
            binds.insert(id.to_string(), keys.iter().filter_map(|k| canon(k)).collect());
        }
        Keymap { binds }
    }
}

impl Keymap {
    /// The user's changes on top of the defaults (config `key.<id>=Ctrl+Z, Ctrl+Y`; empty = no key).
    pub fn with_overrides<'a>(it: impl Iterator<Item = (&'a str, &'a str)>) -> Keymap {
        let mut m = Keymap::default();
        for (id, v) in it {
            if m.binds.contains_key(id) {
                m.binds.insert(id.to_string(), v.split(',').filter_map(|k| canon(k.trim())).collect());
            }
        }
        m
    }

    fn find(&self, combo: &str) -> Option<&str> {
        self.binds.iter().find(|(_, v)| v.iter().any(|c| c == combo)).map(|(k, _)| k.as_str())
    }

    /// The action for a key press. Letter first, physical key second (layout fallback).
    pub fn lookup(&self, ev: &KeyEv) -> Option<&str> {
        let key = norm_key(&ev.key);
        let shift = ev.shift && shift_counts(&key);
        if let Some(a) = self.find(&build(ev.ctrl, ev.alt, shift, &key)) {
            return Some(a);
        }
        if let Some(p) = ev.phys {
            let pk = p.to_ascii_lowercase().to_string();
            if pk != key {
                let shift = ev.shift && shift_counts(&pk);
                if let Some(a) = self.find(&build(ev.ctrl, ev.alt, shift, &pk)) {
                    return Some(a);
                }
            }
        }
        None
    }

    /// Two actions on one key: [(combo, action a, action b)]
    pub fn clashes(&self) -> Vec<(String, String, String)> {
        let mut seen: BTreeMap<&str, &str> = BTreeMap::new();
        let mut out = vec![];
        for (id, v) in &self.binds {
            for c in v {
                if let Some(o) = seen.get(c.as_str()) {
                    out.push((c.clone(), o.to_string(), id.clone()));
                } else {
                    seen.insert(c, id);
                }
            }
        }
        out
    }

    pub fn first(&self, id: &str) -> String {
        self.binds.get(id).and_then(|v| v.first()).map(|c| pretty(c)).unwrap_or_default()
    }
    pub fn all(&self, id: &str) -> String {
        self.binds.get(id).map(|v| v.iter().map(|c| pretty(c)).collect::<Vec<_>>().join(", ")).unwrap_or_default()
    }
}

/// US layout letter / digit of an evdev key code (Linux), for the layout fallback.
#[cfg_attr(windows, allow(dead_code))]
pub fn evdev_us(code: u32) -> Option<char> {
    const ROW1: &str = "1234567890"; // KEY_1 = 2 .. KEY_0 = 11
    const Q: &str = "qwertyuiop"; // 16..25
    const A: &str = "asdfghjkl"; // 30..38
    const Z: &str = "zxcvbnm"; // 44..50
    match code {
        2..=11 => ROW1.chars().nth((code - 2) as usize),
        16..=25 => Q.chars().nth((code - 16) as usize),
        30..=38 => A.chars().nth((code - 30) as usize),
        44..=50 => Z.chars().nth((code - 44) as usize),
        12 => Some('-'),
        13 => Some('='),
        26 => Some('['),
        27 => Some(']'),
        39 => Some(';'),
        40 => Some('\''),
        41 => Some('`'),
        43 => Some('\\'),
        51 => Some(','),
        52 => Some('.'),
        53 => Some('/'),
        _ => None,
    }
}

/// Slint's key text (winit backend: Windows, GNOME) -> KeyEv. Ctrl+letter control characters are mapped back.
pub fn from_slint(text: &str, ctrl: bool, alt: bool, shift: bool, repeat: bool, phys: Option<char>) -> KeyEv {
    use slint::platform::Key;
    let k = |key: Key| slint::SharedString::from(key);
    let named: &[(Key, &str)] = &[
        (Key::Escape, "Escape"),
        (Key::Return, "Return"),
        (Key::Tab, "Tab"),
        (Key::Backtab, "Tab"),
        (Key::Backspace, "BackSpace"),
        (Key::Delete, "Delete"),
        (Key::LeftArrow, "Left"),
        (Key::RightArrow, "Right"),
        (Key::UpArrow, "Up"),
        (Key::DownArrow, "Down"),
        (Key::Home, "Home"),
        (Key::End, "End"),
        (Key::F1, "F1"),
        (Key::F2, "F2"),
        (Key::F3, "F3"),
        (Key::F4, "F4"),
        (Key::Shift, "Shift_L"),
        (Key::ShiftR, "Shift_R"),
        (Key::Control, "Control_L"),
        (Key::ControlR, "Control_R"),
        (Key::Alt, "Alt_L"),
        (Key::AltGr, "Alt_R"),
    ];
    for (key, name) in named {
        if text == k(key.clone()).as_str() {
            return KeyEv { key: name.to_string(), phys, text: String::new(), ctrl, shift, alt, repeat };
        }
    }
    let mut chars = text.chars();
    let key = match (chars.next(), chars.next()) {
        (Some(c), None) if ctrl && (1..=26).contains(&(c as u32)) => (((c as u8) + 96) as char).to_string(),
        (Some(c), None) => c.to_lowercase().to_string(),
        _ => text.to_lowercase(),
    };
    let typed = text.to_string();
    let key = match key.as_str() {
        " " => "space".to_string(),
        "+" => "plus".into(),
        "-" => "minus".into(),
        "=" => "equal".into(),
        _ => key,
    };
    let typed = if ctrl || alt || typed.chars().any(|c| c.is_control()) { String::new() } else { typed };
    KeyEv { key, phys, text: typed, ctrl, shift, alt, repeat }
}

#[cfg(test)]
mod tests {
    #[test]
    fn bar_list_and_edits() {
        use super::{bar_edit, bar_list};
        assert_eq!(bar_list("save, copy,nope,copy,done"), vec!["save", "copy", "done"]);
        assert_eq!(bar_edit("save,copy,done", "add", "pin"), "save,copy,pin,done");
        assert_eq!(bar_edit("save,copy", "add", "pin"), "save,copy,pin");
        assert_eq!(bar_edit("save,copy,done", "left", "copy"), "copy,save,done");
        assert_eq!(bar_edit("save,copy,done", "right", "done"), "save,copy,done");
        assert_eq!(bar_edit("save,copy,done", "remove", "save"), "copy,done");
        assert_eq!(bar_edit("save,copy,done", "add", "save"), "save,copy,done");
    }

    use super::*;

    fn ev(key: &str, ctrl: bool, shift: bool) -> KeyEv {
        KeyEv { key: key.into(), ctrl, shift, ..Default::default() }
    }

    #[test]
    fn defaults_have_no_clashes() {
        assert!(Keymap::default().clashes().is_empty(), "{:?}", Keymap::default().clashes());
    }

    #[test]
    fn ctrl_b_and_every_ctrl_letter() {
        let m = Keymap::default();
        assert_eq!(m.lookup(&ev("b", true, false)), Some("bold"));
        assert_eq!(m.lookup(&ev("b", false, false)), Some("blur"));
        assert_eq!(m.lookup(&ev("z", true, false)), Some("undo"));
        assert_eq!(m.lookup(&ev("Z", true, true)), Some("redo"));
        assert_eq!(m.lookup(&ev("y", true, false)), Some("redo"));
        assert_eq!(m.lookup(&ev("c", true, true)), Some("copy_text"));
        assert_eq!(m.lookup(&ev("?", false, true)), Some("keys"));
        assert_eq!(m.lookup(&ev("F1", false, false)), Some("keys"));
        assert_eq!(m.lookup(&ev("Escape", false, false)), Some("close"));
        assert_eq!(m.lookup(&ev("plus", true, true)), Some("zoom_in"));
        for (id, _, keys) in ACTIONS {
            for k in *keys {
                let c = canon(k).unwrap();
                let parts: Vec<&str> = c.split('+').collect();
                let key = parts.last().unwrap();
                let e = KeyEv {
                    key: key.to_string(),
                    ctrl: parts.contains(&"Ctrl"),
                    alt: parts.contains(&"Alt"),
                    shift: parts.contains(&"Shift"),
                    ..Default::default()
                };
                assert_eq!(m.lookup(&e), Some(*id), "{k}");
            }
        }
    }

    #[test]
    fn slint_control_chars_map_back() {
        let e = from_slint("\u{2}", true, false, false, false, Some('b'));
        assert_eq!(e.key, "b");
        assert_eq!(Keymap::default().lookup(&e), Some("bold"));
        let z = from_slint("\u{1a}", true, false, true, false, None);
        assert_eq!(Keymap::default().lookup(&z), Some("redo"));
    }

    #[test]
    fn layout_fallback() {
        let m = Keymap::default();
        // Russian layout: Ctrl + the key that is Z on US gives "я"
        let e = KeyEv { key: "я".into(), phys: Some('z'), ctrl: true, ..Default::default() };
        assert_eq!(m.lookup(&e), Some("undo"));
        // AZERTY: the key that SAYS z (physical w) is undo - the letter wins
        let e = KeyEv { key: "z".into(), phys: Some('w'), ctrl: true, ..Default::default() };
        assert_eq!(m.lookup(&e), Some("undo"));
        // AZERTY digits: "&" on the 1 key -> colour 1
        let e = KeyEv { key: "&".into(), phys: Some('1'), ..Default::default() };
        assert_eq!(m.lookup(&e), Some("color1"));
    }

    #[test]
    fn overrides_and_clash() {
        let m = Keymap::with_overrides([("pen", "P"), ("pin", "Ctrl+B")].into_iter());
        assert_eq!(m.lookup(&ev("p", false, false)), Some("pen"));
        assert!(!m.clashes().is_empty());
        assert_eq!(pretty("Ctrl+Shift+Z"), "Ctrl ⇧ Z");
    }
}
