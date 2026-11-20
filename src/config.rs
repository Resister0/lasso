//! config.ini (key=value lines). Unknown keys are kept, so an older Lasso doesn't lose a newer one's settings.

use std::collections::BTreeMap;

pub const DEFAULTS: &[(&str, &str)] = &[
    // what Enter (and the ✓ button) does: copy | save | link | copysave
    ("enter_does", "copy"),
    // the picture always goes to the clipboard when done
    ("always_copy", "1"),
    ("save_dir", ""),
    ("name_pattern", "Lasso {yyyy}-{MM}-{dd} {HH}.{mm}.{ss}"),
    ("format", "png"),
    ("delay", "0"),
    ("pointer", "0"),
    // dark | light | system
    ("theme", "system"),
    // teal (Lasso, default) | blue | purple | coral (Aimdot) | pink | amber
    ("accent", "teal"),
    // global hotkeys: off until the user turns them on (no KDE dialog out of nowhere - the user, 13 Nov 2026)
    ("hotkeys", "0"),
    ("hk.region", "Ctrl+Shift+Print"),
    ("hk.full", "Ctrl+Print"),
    ("hk.window", "Alt+Print"),
    ("hk.repeat", "Shift+Print"),
    ("hk.restore", ""),
    // full screenshot: mouse (monitor under the mouse) | all (all monitors in one picture)
    ("full_mode", "mouse"),
    ("esc_hint_seen", "0"),
    ("keep_count", "500"),
    ("keep_days", "90"),
    // share links (need the server - off until it's deployed)
    ("share_on", "0"),
    ("link_expiry", "30"),
    // share links: OCR privacy check before the upload, copy link only or picture + link (KDE), view-once links
    ("share_check", "1"),
    ("share_copy", "link"),
    ("share_once", "0"),
    // shrink pictures over 10 MB (Discord's free limit) on copy / save / link
    ("fits_discord", "1"),
    ("remember_tool", "0"),
    // the zoomed pixel box next to the mouse while picking (the user, 20 Nov 2026: ugly -> off by default)
    ("magnifier", "0"),
    // the once-a-day count (src/ping.rs; only published builds send it)
    ("usage_ping", "1"),
    // what goes into the library: all (every capture) | used (copied / saved / pinned / shared; default) | none
    ("library_keep", "used"),
    ("last_tool", "arrow"),
    ("hidden_tools", ""),
    // the tool strip's order (tool ids, comma separated; empty = the default order)
    ("tool_order", ""),
    // library edits: replace the saved file (keep the original in the library) or save a new copy
    ("edits_as_copy", "0"),
    ("thumb_after_full", "1"),
    ("background_pad", "48"),
    // "Start with your PC" (off until the user turns it on)
    ("autostart", "0"),
    ("first_run_done", "0"),
    ("sounds", "0"),
    ("library_group", "day"),
];

#[derive(Clone, Debug, Default)]
pub struct Config {
    pub v: BTreeMap<String, String>,
}

impl Config {
    pub fn path() -> std::path::PathBuf {
        crate::paths::config_dir().join("config.ini")
    }

    pub fn load() -> Config {
        let mut c = Config::default();
        for (k, v) in DEFAULTS {
            c.v.insert(k.to_string(), v.to_string());
        }
        if let Ok(s) = std::fs::read_to_string(Self::path()) {
            c.merge_text(&s);
        }
        c
    }

    pub fn merge_text(&mut self, s: &str) {
        for l in s.lines() {
            let l = l.trim();
            if l.starts_with('#') || l.starts_with(';') {
                continue;
            }
            if let Some((k, v)) = l.split_once('=') {
                self.v.insert(k.trim().to_string(), v.trim().to_string());
            }
        }
    }

    pub fn text(&self) -> String {
        let mut s = String::from("# Sonela Lasso settings (Settings window writes this file)\n");
        for (k, v) in &self.v {
            s += &format!("{k}={v}\n");
        }
        s
    }

    pub fn save(&self) {
        let p = Self::path();
        let tmp = p.with_extension("ini.tmp");
        if std::fs::write(&tmp, self.text()).is_ok() {
            let _ = std::fs::rename(&tmp, &p);
        }
    }

    pub fn get(&self, k: &str) -> &str {
        self.v.get(k).map(|s| s.as_str()).unwrap_or("")
    }
    pub fn on(&self, k: &str) -> bool {
        matches!(self.get(k), "1" | "true" | "yes" | "on")
    }
    pub fn num(&self, k: &str) -> i64 {
        self.get(k).parse().unwrap_or(0)
    }
    pub fn set(&mut self, k: &str, v: impl Into<String>) {
        self.v.insert(k.to_string(), v.into());
    }

    pub fn save_dir(&self) -> std::path::PathBuf {
        let d = self.get("save_dir");
        if d.is_empty() {
            crate::paths::pictures_dir()
        } else {
            std::path::PathBuf::from(d)
        }
    }

    pub fn keymap(&self) -> crate::keys::Keymap {
        crate::keys::Keymap::with_overrides(self.v.iter().filter_map(|(k, v)| k.strip_prefix("key.").map(|id| (id, v.as_str()))))
    }

    /// The accent: (main, lighter, darker) as 0xRRGGBB
    pub fn accent(&self) -> (u32, u32, u32) {
        accent_of(self.get("accent"))
    }

    /// The tool strip's order: the user's list first, any tool missing from it after (in the default order)
    pub fn tool_order(&self) -> Vec<crate::shapes::Tool> {
        let mut v: Vec<crate::shapes::Tool> = vec![];
        for id in self.get("tool_order").split(',') {
            if let Some(t) = crate::shapes::Tool::from_id(id.trim()) {
                if !v.contains(&t) {
                    v.push(t);
                }
            }
        }
        for t in crate::shapes::Tool::ALL {
            if !v.contains(&t) {
                v.push(t);
            }
        }
        v
    }

    pub fn dark(&self) -> bool {
        match self.get("theme") {
            "light" => false,
            "dark" => true,
            _ => crate::platform_theme_dark(),
        }
    }
}

pub const ACCENTS: [(&str, u32, u32, u32); 6] = [
    ("teal", 0x14a38b, 0x36c9a8, 0x0d8085),
    ("blue", 0x3b82f6, 0x60a5fa, 0x2563eb),
    ("purple", 0x8b5cf6, 0xa78bfa, 0x6d28d9),
    ("coral", 0xef6a4c, 0xf49782, 0xe0485e),
    ("pink", 0xec4899, 0xf472b6, 0xbe185d),
    ("amber", 0xf59e0b, 0xfbbf24, 0xd97706),
];

pub fn accent_of(id: &str) -> (u32, u32, u32) {
    let a = ACCENTS.iter().find(|a| a.0 == id).unwrap_or(&ACCENTS[0]);
    (a.1, a.2, a.3)
}

pub fn color(h: u32) -> slint::Color {
    slint::Color::from_rgb_u8((h >> 16) as u8, (h >> 8) as u8, h as u8)
}
