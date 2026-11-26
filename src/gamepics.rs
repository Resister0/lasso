//! Game screenshots in the Library (26 Nov 2026, the user's idea): Steam's own screenshots (found by itself, named after
//! their game) and any folder the user adds for other launchers (Xbox Game Bar, NVIDIA, Medal, ...). Lasso only SHOWS
//! them - they stay where they are, nothing is copied. Settings > Library > Game screenshots; off by default.
//! Config: `steam_shots` = 1 / 0, `game_dirs` = folders joined by "|".

use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// One picture found in a game folder
pub struct Found {
    pub path: PathBuf,
    /// a small copy when the launcher made one (Steam's thumbnails/), else the picture itself
    pub thumb: PathBuf,
    pub created: i64,
    /// the game (Steam) or the folder it's in
    pub app: String,
}

const EXTS: [&str; 4] = ["png", "jpg", "jpeg", "webp"];
/// a folder full of years of screenshots must not make the Library slow
const MAX: usize = 5000;

fn is_picture(p: &Path) -> bool {
    p.extension().map(|e| EXTS.contains(&e.to_string_lossy().to_lowercase().as_str())).unwrap_or(false)
}

fn mtime(p: &Path) -> i64 {
    std::fs::metadata(p).and_then(|m| m.modified()).ok().and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok()).map(|d| d.as_secs() as i64).unwrap_or(0)
}

/// Where Steam is installed on this PC (each folder once)
pub fn steam_roots() -> Vec<PathBuf> {
    let mut c: Vec<PathBuf> = vec![];
    #[cfg(target_os = "linux")]
    if let Some(h) = std::env::var_os("HOME").map(PathBuf::from) {
        c.push(h.join(".local/share/Steam"));
        c.push(h.join(".steam/steam"));
        c.push(h.join(".var/app/com.valvesoftware.Steam/.local/share/Steam"));
    }
    #[cfg(windows)]
    for v in ["ProgramFiles(x86)", "ProgramFiles"] {
        if let Some(p) = std::env::var_os(v) {
            c.push(PathBuf::from(p).join("Steam"));
        }
    }
    let mut out: Vec<PathBuf> = vec![];
    for p in c {
        if !p.join("userdata").is_dir() {
            continue;
        }
        let real = std::fs::canonicalize(&p).unwrap_or(p);
        if !out.contains(&real) {
            out.push(real);
        }
    }
    out
}

/// appid -> game name, from the appmanifest files of every Steam library on the PC
fn steam_names(root: &Path) -> HashMap<String, String> {
    let mut libs = vec![root.to_path_buf()];
    if let Ok(v) = std::fs::read_to_string(root.join("steamapps/libraryfolders.vdf")) {
        for l in v.lines() {
            if let Some(p) = vdf_value(l, "path") {
                libs.push(PathBuf::from(p.replace("\\\\", "\\")));
            }
        }
    }
    let mut names = HashMap::new();
    for lib in libs {
        let Ok(rd) = std::fs::read_dir(lib.join("steamapps")) else { continue };
        for e in rd.flatten() {
            let n = e.file_name().to_string_lossy().to_string();
            if !(n.starts_with("appmanifest_") && n.ends_with(".acf")) {
                continue;
            }
            let Ok(t) = std::fs::read_to_string(e.path()) else { continue };
            let id = t.lines().find_map(|l| vdf_value(l, "appid"));
            let name = t.lines().find_map(|l| vdf_value(l, "name"));
            if let (Some(id), Some(name)) = (id, name) {
                names.insert(id, name);
            }
        }
    }
    names
}

/// `"key"   "value"` (Steam's text format) -> value
fn vdf_value(line: &str, key: &str) -> Option<String> {
    let mut parts = line.split('"').map(str::trim).filter(|s| !s.is_empty());
    (parts.next()? == key).then(|| parts.next().map(str::to_string)).flatten()
}

/// Steam: <root>/userdata/<user>/760/remote/<appid>/screenshots/*.jpg (+ thumbnails/ with the same names)
pub fn steam_pictures(root: &Path) -> Vec<Found> {
    let names = steam_names(root);
    let mut out = vec![];
    let Ok(users) = std::fs::read_dir(root.join("userdata")) else { return out };
    for u in users.flatten() {
        let Ok(apps) = std::fs::read_dir(u.path().join("760/remote")) else { continue };
        for a in apps.flatten() {
            let appid = a.file_name().to_string_lossy().to_string();
            let dir = a.path().join("screenshots");
            let Ok(rd) = std::fs::read_dir(&dir) else { continue };
            let app = names.get(&appid).cloned().unwrap_or_else(|| "Steam".to_string());
            for e in rd.flatten() {
                let p = e.path();
                if !p.is_file() || !is_picture(&p) {
                    continue;
                }
                let t = dir.join("thumbnails").join(e.file_name());
                out.push(Found { thumb: if t.is_file() { t } else { p.clone() }, created: mtime(&p), path: p, app: app.clone() });
                if out.len() >= MAX {
                    return out;
                }
            }
        }
    }
    out
}

/// Any folder (3 levels deep - NVIDIA keeps one folder per game): pictures, named after the folder they're in
pub fn folder_pictures(dir: &Path) -> Vec<Found> {
    let mut out = vec![];
    let top = dir.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
    walk(dir, 0, &top, &mut out);
    out
}

fn walk(dir: &Path, depth: u32, top: &str, out: &mut Vec<Found>) {
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    let here = if depth == 0 { top.to_string() } else { dir.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default() };
    for e in rd.flatten() {
        if out.len() >= MAX {
            return;
        }
        let p = e.path();
        let name = e.file_name().to_string_lossy().to_string();
        if name.starts_with('.') {
            continue;
        }
        let Ok(ft) = e.file_type() else { continue };
        if ft.is_dir() {
            if depth < 2 && !name.eq_ignore_ascii_case("thumbnails") {
                walk(&p, depth + 1, top, out);
            }
        } else if is_picture(&p) {
            out.push(Found { thumb: p.clone(), created: mtime(&p), path: p, app: here.clone() });
        }
    }
}

/// The added folders (config `game_dirs`)
pub fn dirs(cfg: &crate::config::Config) -> Vec<PathBuf> {
    cfg.get("game_dirs").split('|').filter(|s| !s.trim().is_empty()).map(PathBuf::from).collect()
}

/// Everything to show: Steam's (when switched on) + the added folders, each picture once
pub fn all(cfg: &crate::config::Config) -> Vec<Found> {
    let mut out = vec![];
    if cfg.on("steam_shots") {
        for r in steam_roots() {
            out.extend(steam_pictures(&r));
        }
    }
    for d in dirs(cfg) {
        out.extend(folder_pictures(&d));
    }
    let mut seen = std::collections::HashSet::new();
    out.retain(|f| seen.insert(f.path.clone()));
    out
}

/// A small copy for the tray flyout of a picture that has none (other launchers): made once, kept in the Library's
/// cache folder, named after the file + its size + time (a changed file gets a new one)
pub fn small_thumb(f: &Found) -> Option<PathBuf> {
    if f.thumb != f.path {
        return Some(f.thumb.clone());
    }
    let m = std::fs::metadata(&f.path).ok()?;
    let mut h: u64 = 0xcbf29ce484222325;
    for b in format!("{}|{}|{}", f.path.to_string_lossy(), m.len(), f.created).bytes() {
        h ^= b as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    let dir = crate::paths::library_dir().join(".flythumbs");
    let out = dir.join(format!("{h:x}.png"));
    if out.is_file() {
        return Some(out);
    }
    let _ = std::fs::create_dir_all(&dir);
    let img = image::open(&f.path).ok()?;
    img.thumbnail(480, 240).save(&out).ok()?;
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn steam_layout_and_names() {
        let base = std::env::temp_dir().join(format!("lasso-steam-test-{}", std::process::id()));
        let shots = base.join("userdata/123/760/remote/730/screenshots");
        std::fs::create_dir_all(shots.join("thumbnails")).unwrap();
        std::fs::create_dir_all(base.join("steamapps")).unwrap();
        std::fs::write(base.join("steamapps/appmanifest_730.acf"), "\"AppState\"\n{\n\t\"appid\"\t\t\"730\"\n\t\"name\"\t\t\"Counter-Strike 2\"\n}\n").unwrap();
        std::fs::write(shots.join("20261126_1.jpg"), b"x").unwrap();
        std::fs::write(shots.join("thumbnails/20261126_1.jpg"), b"x").unwrap();
        std::fs::write(shots.join("notes.txt"), b"x").unwrap();
        let f = steam_pictures(&base);
        assert_eq!(f.len(), 1);
        assert_eq!(f[0].app, "Counter-Strike 2");
        assert_eq!(f[0].thumb, shots.join("thumbnails/20261126_1.jpg"));
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn folders_name_by_subfolder_and_skip_thumbnails() {
        let base = std::env::temp_dir().join(format!("lasso-folder-test-{}", std::process::id())).join("Captures");
        std::fs::create_dir_all(base.join("Elden Ring/thumbnails")).unwrap();
        std::fs::write(base.join("a.png"), b"x").unwrap();
        std::fs::write(base.join("Elden Ring/b.jpg"), b"x").unwrap();
        std::fs::write(base.join("Elden Ring/thumbnails/b.jpg"), b"x").unwrap();
        let mut f = folder_pictures(&base);
        f.sort_by(|a, b| a.app.cmp(&b.app));
        assert_eq!(f.len(), 2);
        assert_eq!(f[0].app, "Captures");
        assert_eq!(f[1].app, "Elden Ring");
        let _ = std::fs::remove_dir_all(base.parent().unwrap());
    }

    #[test]
    fn vdf_lines() {
        assert_eq!(vdf_value("\t\t\"path\"\t\t\"/games/SteamLibrary\"", "path").as_deref(), Some("/games/SteamLibrary"));
        assert_eq!(vdf_value("\t\"name\"\t\t\"Portal 2\"", "path"), None);
    }
}
