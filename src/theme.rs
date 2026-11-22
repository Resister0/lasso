//! The themes (21 Nov 2026: "why are there only some very default themes unlike the other apps"): Echo's / Aimdot's
//! list and their themeVars() maths (copied from Aimdot/settings/src/theme.rs), Lasso teal first (id "").
//! config.ini: `theme` = the id ("" teal, "light", "system", "custom"...), custom colours `ui_accent` / `ui_bg`.
//! Old values from before (theme=dark/light/system + accent=blue/purple/...) still work: see `current`.

/// (id, name, accent, accent2, gradient, background)
const THEMES: [(&str, &str, &str, &str, [&str; 3], &str); 24] = [
    ("", "Teal", "#14a38b", "#36c9a8", ["#36c9a8", "#14a38b", "#0d8085"], "#0d0d10"),
    ("violet", "Violet", "#7c5cff", "#9d85ff", ["#8b6cff", "#6a4cf5", "#4f7bff"], "#0d0d10"),
    ("coral", "Coral", "#ef6a4c", "#f49782", ["#ff8f6b", "#ef6a4c", "#e0485e"], "#0d0d10"),
    ("ocean", "Ocean", "#2fa8e0", "#7cc9f0", ["#4cc3f0", "#2f8fe0", "#3b5cf0"], "#0b0e12"),
    ("ember", "Ember", "#f07a1a", "#ffa154", ["#ffa154", "#f07a1a", "#e0484a"], "#100d0b"),
    ("emerald", "Emerald", "#2fbf86", "#6fd9ab", ["#6fd9ab", "#2fbf86", "#1d9a9a"], "#0b100e"),
    ("crimson", "Crimson", "#e03232", "#e97070", ["#e55151", "#e03232", "#a82626"], "#0d0d10"),
    ("rose", "Rosé", "#e0609e", "#f29ac4", ["#f29ac4", "#e0609e", "#9b5be0"], "#100c0f"),
    ("slate", "Slate", "#8a96a8", "#c3ccd8", ["#c3ccd8", "#8a96a8", "#5d6a7c"], "#0c0d0f"),
    ("frost", "Frost", "#4fb8f0", "#9ad8f8", ["#bfe8ff", "#4fb8f0", "#5a7cf0"], "#0a0e12"),
    ("lime", "Lime", "#8fcf3c", "#b8e57e", ["#c5ef8a", "#8fcf3c", "#2fae6f"], "#0c0f0a"),
    ("gold", "Gold", "#e0a93a", "#f0cb7a", ["#f5d68a", "#e0a93a", "#c06a2a"], "#100e0a"),
    ("midnight", "Midnight", "#6a6ff0", "#a3a6fa", ["#a3a6fa", "#6a6ff0", "#3a3fb8"], "#08080f"),
    ("plum", "Plum", "#c13fd6", "#dc8ae9", ["#d77ae6", "#c13fd6", "#7f2fc0"], "#140d16"),
    ("mono", "Mono", "#e4e4e7", "#f4f4f5", ["#ffffff", "#e4e4e7", "#a1a1aa"], "#0b0b0c"),
    ("sand", "Sand", "#d2b48c", "#e6d2b4", ["#ecdcc0", "#d2b48c", "#a0805a"], "#14120e"),
    ("christmas", "Christmas", "#e0484a", "#f08a8a", ["#f08a8a", "#e0484a", "#2f9e5a"], "#0a100c"),
    ("autumn", "Autumn", "#e07a2a", "#f0a868", ["#f5c27a", "#e07a2a", "#a8402a"], "#110d09"),
    ("halloween", "Halloween", "#f08a1a", "#ffb15a", ["#ffb15a", "#f08a1a", "#8a3ae0"], "#0e0a12"),
    ("winter", "Winter", "#7cc4ec", "#b8e2f8", ["#e2f4ff", "#7cc4ec", "#5a86d8"], "#0a0d12"),
    ("rain", "Rain", "#6f9bd1", "#a3c2e8", ["#c4d8f0", "#6f9bd1", "#4a6a9a"], "#0d1015"),
    ("light", "Light", "#0f8f7a", "#14a38b", ["#36c9a8", "#0f8f7a", "#0d7085"], "#f4f4f7"),
    ("paper", "Paper", "#c0602a", "#d9844a", ["#e8a06a", "#c0602a", "#9a3f2a"], "#f6f2ea"),
    ("system", "Match system", "", "", ["", "", ""], ""),
];


/// Every colour of one theme, as 0xRRGGBB
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Palette {
    pub bg: u32,
    pub rail: u32,
    pub panel: u32,
    pub panel2: u32,
    pub line: u32,
    pub hover: u32,
    pub text: u32,
    pub muted: u32,
    pub dim: u32,
    pub accent: u32,
    pub accent2: u32,
    pub on_accent: u32,
    pub grad: [u32; 3],
    pub light: bool,
}

fn parse_hex(h: &str) -> Option<(f32, f32, f32)> {
    let h = h.trim().trim_start_matches('#');
    if h.len() != 6 || !h.chars().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    let n = u32::from_str_radix(h, 16).ok()?;
    Some((((n >> 16) & 255) as f32, ((n >> 8) & 255) as f32, (n & 255) as f32))
}
fn hex((r, g, b): (f32, f32, f32)) -> String {
    format!("#{:02x}{:02x}{:02x}", r.round().clamp(0.0, 255.0) as u8, g.round().clamp(0.0, 255.0) as u8, b.round().clamp(0.0, 255.0) as u8)
}
fn mix(a: &str, b: &str, t: f32) -> String {
    let (x, y) = (parse_hex(a).unwrap_or((0.0, 0.0, 0.0)), parse_hex(b).unwrap_or((0.0, 0.0, 0.0)));
    hex((x.0 + (y.0 - x.0) * t, x.1 + (y.1 - x.1) * t, x.2 + (y.2 - x.2) * t))
}
fn lum(h: &str) -> f32 {
    let (r, g, b) = parse_hex(h).unwrap_or((0.0, 0.0, 0.0));
    (0.2126 * r + 0.7152 * g + 0.0722 * b) / 255.0
}
fn rel_lum(h: &str) -> f32 {
    let (r, g, b) = parse_hex(h).unwrap_or((0.0, 0.0, 0.0));
    let lin = |c: f32| {
        let c = c / 255.0;
        if c <= 0.04045 {
            c / 12.92
        } else {
            ((c + 0.055) / 1.055).powf(2.4)
        }
    };
    0.2126 * lin(r) + 0.7152 * lin(g) + 0.0722 * lin(b)
}
/// Text on the accent: white, unless white reads below 2.3:1 there (Mono, Sand, Lime...), then near-black
fn on_colour(fill: &str) -> &'static str {
    if 1.05 / (rel_lum(fill) + 0.05) < 2.3 {
        "#0d0d10"
    } else {
        "#ffffff"
    }
}
fn n(h: &str) -> u32 {
    let (r, g, b) = parse_hex(h).unwrap_or((0.0, 0.0, 0.0));
    ((r as u32) << 16) | ((g as u32) << 8) | b as u32
}

/// The theme id in use, with the old settings mapped (theme=dark + accent=blue -> "ocean", theme=light -> "light")
pub fn current(theme: &str, old_accent: &str) -> String {
    match theme {
        "dark" | "" => match old_accent {
            "blue" => "ocean",
            "purple" => "violet",
            "coral" => "coral",
            "pink" => "rose",
            "amber" => "gold",
            _ => "",
        }
        .into(),
        t if known(t) => t.into(),
        _ => "".into(),
    }
}

pub fn known(id: &str) -> bool {
    id == "custom" || THEMES.iter().any(|t| t.0 == id)
}

/// Echo's themeVars(): the whole palette from a theme id (+ the custom colours). `system_light` = the desktop is light
/// (only asked for "Match system").
pub fn palette(id: &str, accent: &str, bg: &str, system_light: impl Fn() -> bool) -> Palette {
    let id = if id == "system" { if system_light() { "light" } else { "" } } else { id };
    let t = THEMES.iter().find(|t| t.0 == id && id != "system");
    let (a, a2, g, b) = match t {
        Some(t) if id != "custom" => (t.2.to_string(), t.3.to_string(), [t.4[0].to_string(), t.4[1].to_string(), t.4[2].to_string()], t.5.to_string()),
        _ => {
            let a = if parse_hex(accent).is_some() { accent.to_string() } else { THEMES[0].2.into() };
            let mut b = if parse_hex(bg).is_some() { bg.to_string() } else { "#0d0d10".into() };
            // a custom background is dark or light, nothing in between (text must stay readable)
            if lum(&b) <= 0.5 {
                for _ in 0..12 {
                    if lum(&b) <= 0.16 {
                        break;
                    }
                    b = mix(&b, "#000000", 0.2);
                }
            }
            (a.clone(), mix(&a, "#ffffff", 0.3), [mix(&a, "#ffffff", 0.15), a.clone(), mix(&a, "#000000", 0.25)], b)
        }
    };
    let light = lum(&b) > 0.5;
    let (rail, panel, panel2, line, hover, text, muted, dim) = if light {
        (mix(&b, "#000000", 0.035), "#ffffff".to_string(), mix(&b, "#000000", 0.045), mix(&b, "#000000", 0.11), mix(&b, "#000000", 0.18), "#16161c", "#55555f", "#86868f")
    } else {
        (mix(&b, "#000000", 0.22), mix(&b, "#ffffff", 0.05), mix(&b, "#ffffff", 0.085), mix(&b, "#ffffff", 0.13), mix(&b, "#ffffff", 0.2), "#f5f5f7", "#a0a0ab", "#6b6b76")
    };
    Palette {
        bg: n(&b),
        rail: n(&rail),
        panel: n(&panel),
        panel2: n(&panel2),
        line: n(&line),
        hover: n(&hover),
        text: n(text),
        muted: n(muted),
        dim: n(dim),
        accent: n(&a),
        accent2: n(&a2),
        on_accent: n(on_colour(&a)),
        grad: [n(&g[0]), n(&g[1]), n(&g[2])],
        light,
    }
}

/// Every theme for the cards: (id, name), Custom last
pub fn list() -> Vec<(&'static str, &'static str)> {
    let mut v: Vec<(&str, &str)> = THEMES.iter().map(|t| (t.0, t.1)).collect();
    v.push(("custom", "Custom"));
    v
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn old_settings_and_palettes() {
        assert_eq!(current("dark", "purple"), "violet");
        assert_eq!(current("system", "teal"), "system");
        assert_eq!(current("light", ""), "light");
        assert_eq!(current("nope", ""), "");
        let teal = palette("", "", "", || false);
        assert_eq!(teal.accent, 0x14a38b);
        assert!(!teal.light);
        assert!(palette("system", "", "", || true).light);
        assert!(palette("paper", "", "", || false).light);
        // a mid-grey custom background is made dark enough to read
        let c = palette("custom", "#ff0000", "#707070", || false);
        assert!(!c.light && c.accent == 0xff0000);
        // Mono: dark text on the light accent
        assert_eq!(palette("mono", "", "", || false).on_accent, 0x0d0d10);
    }
}
