//! "Also show in Echo" (the user's idea, 21 Nov 2026): every picture Lasso saves also gets a copy in Echo's
//! Screenshots folder (<Echo's clips folder>/Screenshots), so it shows on Echo's Screenshots page. Echo needs no
//! change: that page lists every picture in that folder. Echo's clips folder is read fresh from Echo's settings.json
//! each time (the user can move it). Lasso's own save folder stays as it is.
//! A copy only replaces a file in Echo's folder that Lasso put there itself (a library edit re-saving the same
//! picture); any other name clash gets a free new name, so an Echo screenshot is never overwritten.

use std::path::{Path, PathBuf};

fn home() -> PathBuf {
    std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE")).map(PathBuf::from).unwrap_or_else(|| PathBuf::from("."))
}

/// Where Echo's settings.json can be: Windows %APPDATA%\EchoClip, Linux ~/.config/EchoClip (Echo points APPDATA
/// there), the Flatpak's own config folder. Echo's folder was called "Clipper" until 24 Nov 2026: Echo renames it once
/// and marks the new one (.echo-folder) - same rule as Echo's own data_dir: the marked new folder, else the old one.
/// Test copies (LASSO_HOME) only ever look inside their own folder.
fn settings_files() -> Vec<PathBuf> {
    let pair = |base: PathBuf, v: &mut Vec<PathBuf>| v.extend(settings_in(&base));
    let mut v = vec![];
    if let Some(t) = std::env::var_os("LASSO_HOME") {
        pair(PathBuf::from(t).join("echo-config"), &mut v);
        return v;
    }
    if cfg!(windows) {
        if let Some(a) = std::env::var_os("APPDATA") {
            pair(PathBuf::from(a), &mut v);
        }
    } else {
        let cfg = std::env::var_os("XDG_CONFIG_HOME").map(PathBuf::from).unwrap_or_else(|| home().join(".config"));
        pair(cfg, &mut v);
        pair(home().join(".var/app/link.echoclip.Echo/config"), &mut v);
    }
    v
}

/// Echo's clips folder from its settings text ("clips_dir"; Echo's own default when the key is missing or empty)
fn clips_dir_from(json: &str) -> PathBuf {
    serde_json::from_str::<serde_json::Value>(json)
        .ok()
        .and_then(|v| v.get("clips_dir").and_then(|d| d.as_str()).map(str::to_string))
        .filter(|d| !d.trim().is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| home().join("Videos").join("EchoClips"))
}

/// Echo's settings.json under one config folder, the one Echo uses first
fn settings_in(base: &Path) -> Vec<PathBuf> {
    let mut v = vec![];
    if base.join("EchoClip").join(".echo-folder").is_file() {
        v.push(base.join("EchoClip").join("settings.json"));
    }
    v.push(base.join("Clipper").join("settings.json"));
    v
}

/// Echo's Screenshots folder, or None when Echo isn't set up on this PC (no settings.json found)
pub fn shots_dir() -> Option<PathBuf> {
    let f = settings_files().into_iter().find(|f| f.is_file())?;
    let s = std::fs::read_to_string(&f).ok()?;
    Some(clips_dir_from(&s).join("Screenshots"))
}

/// The folder to copy into when the switch is on and Echo is there
pub fn target(cfg: &crate::config::Config) -> Option<PathBuf> {
    cfg.on("echo_shots").then(shots_dir).flatten()
}

/// What Lasso put into Echo's folder: one "capture id<TAB>file name" line per capture (newest last), so a later copy
/// or save of the SAME capture replaces its file (Ctrl+C then Ctrl+S, or a library edit saved again = still one
/// picture in Echo), and only files Lasso made itself are ever replaced.
fn record() -> PathBuf {
    crate::paths::config_dir().join("echo-copies.txt")
}

fn read_rec(rec: &Path) -> Vec<(String, String)> {
    std::fs::read_to_string(rec)
        .unwrap_or_default()
        .lines()
        .filter_map(|l| l.split_once('\t').map(|(a, b)| (a.to_string(), b.to_string())))
        .collect()
}

fn write_rec(rec: &Path, mut v: Vec<(String, String)>) {
    if v.len() > 3000 {
        v.drain(..v.len() - 3000);
    }
    let s: String = v.iter().map(|(a, b)| format!("{a}\t{b}\n")).collect();
    let tmp = rec.with_extension("txt.tmp");
    if std::fs::write(&tmp, s).is_ok() {
        let _ = std::fs::rename(&tmp, rec);
    }
}

/// one writer at a time (a copy and a save of the same capture finish close together)
static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Puts one capture's picture into `dir` as `<stem>.<ext>`: replaces this capture's earlier file, never anyone
/// else's (a clash gets a free name). `write` fills the given temp path.
fn place(dir: &Path, rec: &Path, id: &str, stem: &str, ext: &str, write: impl FnOnce(&Path) -> std::io::Result<()>) -> Result<PathBuf, String> {
    let _g = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    std::fs::create_dir_all(dir).map_err(|e| format!("Echo's folder: {e}"))?;
    let mut v = read_rec(rec);
    let prev = (!id.is_empty()).then(|| v.iter().rev().find(|(a, _)| a == id).map(|(_, b)| b.clone())).flatten();
    let want = format!("{stem}.{ext}");
    let dst = match &prev {
        // same kind of file: write over this capture's own copy (it keeps its name)
        Some(p) if Path::new(p).extension().map(|e| e.to_string_lossy() == ext).unwrap_or(false) => dir.join(p),
        _ => {
            let d = dir.join(&want);
            if d.exists() {
                crate::export::free_name(dir, stem, ext)
            } else {
                d
            }
        }
    };
    let tmp = dst.with_extension("lasso-part");
    write(&tmp).map_err(|e| {
        let _ = std::fs::remove_file(&tmp);
        format!("copy to Echo: {e}")
    })?;
    std::fs::rename(&tmp, &dst).map_err(|e| {
        let _ = std::fs::remove_file(&tmp);
        format!("copy to Echo: {e}")
    })?;
    let name = dst.file_name().unwrap_or_default().to_string_lossy().to_string();
    // another kind of file for the same capture (PNG copy, then saved as JPG): the old copy goes
    if let Some(p) = prev.filter(|p| *p != name) {
        let _ = std::fs::remove_file(dir.join(p));
    }
    if !id.is_empty() {
        v.retain(|(a, _)| a != id);
        v.push((id.to_string(), name));
        write_rec(rec, v);
    }
    Ok(dst)
}

fn logged(r: Result<PathBuf, String>) {
    match r {
        Ok(p) => crate::log::line("echo", &format!("in Echo's Screenshots: {}", p.file_name().unwrap_or_default().to_string_lossy())),
        Err(e) => crate::log::line("echo", &e),
    }
}

fn copy_file(dir: &Path, rec: &Path, id: &str, file: &Path) -> Result<PathBuf, String> {
    let stem = file.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_default();
    let ext = file.extension().map(|s| s.to_string_lossy().to_lowercase()).unwrap_or_default();
    place(dir, rec, id, &stem, &ext, |t| std::fs::copy(file, t).map(|_| ()))
}

/// After a save: that file also goes to Echo, in the background (never slows the save; failures only go to the log)
pub fn mirror(dir: Option<PathBuf>, id: &str, file: &Path) {
    let Some(dir) = dir else { return };
    let (id, file) = (id.to_string(), file.to_path_buf());
    std::thread::spawn(move || logged(copy_file(&dir, &record(), &id, &file)));
}

/// After a copy (Ctrl+C / Enter, nothing saved): the picture goes to Echo as a file named like a save would be
/// (format + "Fits Discord" from the settings), encoded in the background
pub fn put_pic(dir: Option<PathBuf>, id: &str, pic: std::sync::Arc<crate::shapes::Pic>, stem: String, format: String, fits: bool) {
    let Some(dir) = dir else { return };
    let id = id.to_string();
    std::thread::spawn(move || {
        let enc = crate::export::encode(&pic, &format, fits, false);
        logged(place(&dir, &record(), &id, &stem, enc.ext, |t| std::fs::write(t, &enc.bytes)));
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clips_folder_from_echo_settings() {
        assert_eq!(clips_dir_from(r#"{"clips_dir": "/x/My Clips", "other": 1}"#), PathBuf::from("/x/My Clips"));
        assert_eq!(clips_dir_from(r#"{"clips_dir": ""}"#), home().join("Videos").join("EchoClips"));
        assert_eq!(clips_dir_from("not json"), home().join("Videos").join("EchoClips"));
    }

    /// 24 Nov 2026: Echo's folder "Clipper" became "EchoClip" (renamed once by Echo, marked with .echo-folder) - Lasso
    /// finds Echo's settings in both, the marked new folder first; an unmarked "EchoClip" isn't Echo's
    #[test]
    fn finds_echo_in_the_old_and_the_new_folder() {
        let b = std::env::temp_dir().join(format!("lasso-echo-dir-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&b);
        std::fs::create_dir_all(b.join("EchoClip")).unwrap();
        assert_eq!(settings_in(&b), vec![b.join("Clipper").join("settings.json")]);
        std::fs::write(b.join("EchoClip").join(".echo-folder"), "").unwrap();
        assert_eq!(settings_in(&b), vec![b.join("EchoClip").join("settings.json"), b.join("Clipper").join("settings.json")]);
        let _ = std::fs::remove_dir_all(&b);
    }

    #[test]
    fn one_file_per_capture_and_echo_files_stay() {
        let t = std::env::temp_dir().join(format!("lasso-echo-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&t);
        std::fs::create_dir_all(&t).unwrap();
        let rec = t.join("echo-copies.txt");
        let dir = t.join("Screenshots");
        std::fs::create_dir_all(&dir).unwrap();
        // an Echo screenshot with the same name stays as it is; Lasso's copy gets another name
        std::fs::write(dir.join("Lasso 1.png"), b"echo's own").unwrap();
        let src = t.join("Lasso 1.png");
        std::fs::write(&src, b"lasso-1").unwrap();
        let a = copy_file(&dir, &rec, "cap1", &src).unwrap();
        assert_ne!(a, dir.join("Lasso 1.png"));
        assert_eq!(std::fs::read(dir.join("Lasso 1.png")).unwrap(), b"echo's own");
        // Ctrl+C (copy) then Ctrl+S (save) of capture 2 = one file in Echo, the saved one
        let b = place(&dir, &rec, "cap2", "Lasso 2", "png", |p| std::fs::write(p, b"copied")).unwrap();
        let src2 = t.join("Lasso 2 saved.png");
        std::fs::write(&src2, b"saved").unwrap();
        let c = copy_file(&dir, &rec, "cap2", &src2).unwrap();
        assert_eq!(b, c);
        assert_eq!(std::fs::read(&c).unwrap(), b"saved");
        // saved again as JPG: the PNG copy goes, the JPG stays (still one file for that capture)
        let src3 = t.join("Lasso 2.jpg");
        std::fs::write(&src3, b"jpeg").unwrap();
        let d = copy_file(&dir, &rec, "cap2", &src3).unwrap();
        assert!(!c.exists() && d.exists());
        let n = std::fs::read_dir(&dir).unwrap().count();
        assert_eq!(n, 3, "Echo's own + capture 1 + capture 2");
        let _ = std::fs::remove_dir_all(&t);
    }
}
