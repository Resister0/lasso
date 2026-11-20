//! Where Lasso keeps things. Linux: ~/.config/lasso (settings, log), ~/.local/share/lasso/library (captures).
//! Windows: %APPDATA%\Lasso, %LOCALAPPDATA%\Lasso\library. Test copies: LASSO_HOME=<dir> puts everything there.

use std::path::PathBuf;

fn home() -> PathBuf {
    std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE")).map(PathBuf::from).unwrap_or_else(|| PathBuf::from("."))
}

fn test_home() -> Option<PathBuf> {
    std::env::var_os("LASSO_HOME").map(PathBuf::from)
}

pub fn config_dir() -> PathBuf {
    let d = if let Some(t) = test_home() {
        t.join("config")
    } else if cfg!(windows) {
        std::env::var_os("APPDATA").map(PathBuf::from).unwrap_or_else(home).join("Lasso")
    } else {
        std::env::var_os("XDG_CONFIG_HOME").map(PathBuf::from).unwrap_or_else(|| home().join(".config")).join("lasso")
    };
    let _ = std::fs::create_dir_all(&d);
    d
}

pub fn library_dir() -> PathBuf {
    let d = if let Some(t) = test_home() {
        t.join("library")
    } else if cfg!(windows) {
        std::env::var_os("LOCALAPPDATA").map(PathBuf::from).unwrap_or_else(home).join("Lasso").join("library")
    } else {
        std::env::var_os("XDG_DATA_HOME").map(PathBuf::from).unwrap_or_else(|| home().join(".local/share")).join("lasso").join("library")
    };
    let _ = std::fs::create_dir_all(&d);
    d
}

/// The default save folder: Pictures/Lasso (XDG pictures folder on Linux)
pub fn pictures_dir() -> PathBuf {
    if let Some(t) = test_home() {
        return t.join("Pictures").join("Lasso");
    }
    #[cfg(target_os = "linux")]
    {
        let cfg = std::env::var_os("XDG_CONFIG_HOME").map(PathBuf::from).unwrap_or_else(|| home().join(".config"));
        if let Ok(s) = std::fs::read_to_string(cfg.join("user-dirs.dirs")) {
            for l in s.lines() {
                if let Some(v) = l.strip_prefix("XDG_PICTURES_DIR=") {
                    let v = v.trim_matches('"').replace("$HOME", &home().to_string_lossy());
                    return PathBuf::from(v).join("Lasso");
                }
            }
        }
    }
    home().join("Pictures").join("Lasso")
}

/// Pictures handed out by dragging the corner thumbnail (when the capture wasn't saved): a cache, files older
/// than a day go on the next drag. Linux ~/.cache/lasso/drag, Windows %LOCALAPPDATA%\Lasso\drag.
pub fn drag_dir() -> PathBuf {
    let d = if let Some(t) = test_home() {
        t.join("drag")
    } else if cfg!(windows) {
        std::env::var_os("LOCALAPPDATA").map(PathBuf::from).unwrap_or_else(home).join("Lasso").join("drag")
    } else {
        std::env::var_os("XDG_CACHE_HOME").map(PathBuf::from).unwrap_or_else(|| home().join(".cache")).join("lasso").join("drag")
    };
    let _ = std::fs::create_dir_all(&d);
    d
}

/// Unix socket / scratch files (Linux: $XDG_RUNTIME_DIR, only this user can open it)
pub fn runtime_dir() -> PathBuf {
    if let Some(t) = test_home() {
        let d = t.join("run");
        let _ = std::fs::create_dir_all(&d);
        return d;
    }
    std::env::var_os("XDG_RUNTIME_DIR").map(PathBuf::from).unwrap_or_else(std::env::temp_dir)
}

#[cfg_attr(windows, allow(dead_code))]
pub fn socket_path() -> PathBuf {
    runtime_dir().join("lasso.sock")
}
