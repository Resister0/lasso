//! Linux: Lasso's own .desktop file + icon in ~/.local/share (written on start when missing or pointing at
//! another copy). KWin only answers ScreenShot2 for apps whose installed .desktop file lists it in
//! X-KDE-DBUS-Restricted-Interfaces - this is what makes the silent, fast capture work. The AppImage path
//! ($APPIMAGE) is used when Lasso runs as an AppImage.

use std::path::PathBuf;

pub const APP_ID: &str = "link.sonela.Lasso";

fn test_mode() -> bool {
    std::env::var_os("LASSO_HOME").is_some()
}

/// ~/.local/share (a test copy: inside LASSO_HOME - never the real menu / autostart)
fn data_home() -> PathBuf {
    if let Some(t) = std::env::var_os("LASSO_HOME") {
        return PathBuf::from(t).join("share");
    }
    std::env::var_os("XDG_DATA_HOME").map(PathBuf::from).unwrap_or_else(|| PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(".local/share"))
}

/// "Start with your PC": ~/.config/autostart/link.sonela.Lasso.desktop (removed when off)
pub fn set_autostart(on: bool) {
    let cfg = match std::env::var_os("LASSO_HOME") {
        Some(t) => PathBuf::from(t).join("xdg-config"),
        None => std::env::var_os("XDG_CONFIG_HOME").map(PathBuf::from).unwrap_or_else(|| PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(".config")),
    };
    let file = cfg.join("autostart").join(format!("{APP_ID}.desktop"));
    if !on {
        let _ = std::fs::remove_file(&file);
        return;
    }
    let exe = std::env::var_os("APPIMAGE").map(PathBuf::from).or_else(|| std::env::current_exe().ok()).unwrap_or_default();
    let _ = std::fs::create_dir_all(file.parent().unwrap());
    let _ = std::fs::write(&file, format!("[Desktop Entry]\nType=Application\nName=Lasso\nExec=\"{}\"\nIcon={APP_ID}\nX-GNOME-Autostart-enabled=true\nNoDisplay=true\n", exe.to_string_lossy()));
}

/// AppImage: KWin allows ScreenShot2 only when the RUNNING exe matches an installed .desktop file's Exec, but an
/// AppImage runs from a temporary mount. So the daemon copies itself once to ~/.local/share/lasso/bin/lasso and goes on
/// from there (exec: same process, so the AppImage's mount + libraries stay). A hidden .desktop file points KWin at the
/// copy; the menu entry keeps starting the AppImage (updates replace the AppImage, the copy follows on its next start).
/// ⚠ untested (no AppImage built yet)
pub fn appimage_reexec() {
    use std::os::unix::process::CommandExt;
    let Some(_) = std::env::var_os("APPIMAGE") else { return };
    if std::env::var_os("LASSO_COPY").is_some() {
        return;
    }
    let Ok(me) = std::env::current_exe() else { return };
    let target = data_home().join("lasso/bin/lasso");
    let same = std::fs::metadata(&me).ok().zip(std::fs::metadata(&target).ok()).map(|(a, b)| a.len() == b.len()).unwrap_or(false)
        && std::fs::read(&me).ok() == std::fs::read(&target).ok();
    if !same {
        let _ = std::fs::create_dir_all(target.parent().unwrap());
        let tmp = target.with_extension("new");
        if std::fs::copy(&me, &tmp).is_err() || std::fs::rename(&tmp, &target).is_err() {
            return;
        }
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o755));
    }
    let err = std::process::Command::new(&target).args(std::env::args_os().skip(1)).env("LASSO_COPY", "1").exec();
    crate::log::line("desktop", &format!("AppImage copy didn't start ({err}) - going on from the AppImage"));
}

pub fn install() {
    let exe = std::env::var_os("APPIMAGE").map(PathBuf::from).or_else(|| std::env::current_exe().ok()).unwrap_or_default();
    // the AppImage copy (see appimage_reexec): a hidden entry with KWin's permission that matches the running exe
    if std::env::var_os("LASSO_COPY").is_some() {
        if let Ok(me) = std::env::current_exe() {
            let f = data_home().join("applications").join(format!("{APP_ID}-capture.desktop"));
            let t = format!("[Desktop Entry]\nType=Application\nName=Lasso (screen capture)\nExec=\"{}\"\nIcon={APP_ID}\nNoDisplay=true\nX-KDE-DBUS-Restricted-Interfaces=org.kde.KWin.ScreenShot2,org.kde.KWin.CaptureScreen\n", me.to_string_lossy());
            if std::fs::read_to_string(&f).map(|s| s != t).unwrap_or(true) && std::fs::write(&f, t).is_ok() {
                if !test_mode() { let _ = std::process::Command::new("kbuildsycoca6").stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null()).spawn(); }
            }
        }
    }
    let apps = data_home().join("applications");
    let file = apps.join(format!("{APP_ID}.desktop"));
    let text = format!(
        "[Desktop Entry]\nType=Application\nName=Lasso\nGenericName=Screenshot tool\nComment=Screenshots with every editing tool right on the frozen screen\nExec=\"{}\" %U\nIcon={APP_ID}\nTerminal=false\nCategories=Graphics;Utility;\nKeywords=screenshot;snip;capture;annotate;\nStartupNotify=false\nX-KDE-DBUS-Restricted-Interfaces=org.kde.KWin.ScreenShot2,org.kde.KWin.CaptureScreen\nActions=Region;Full;Window;Library;\n\n[Desktop Action Region]\nName=Capture an area\nExec=\"{0}\" --region\n\n[Desktop Action Full]\nName=Full screenshot\nExec=\"{0}\" --full\n\n[Desktop Action Window]\nName=Capture a window\nExec=\"{0}\" --window\n\n[Desktop Action Library]\nName=Library\nExec=\"{0}\" --library\n",
        exe.to_string_lossy()
    );
    if std::fs::read_to_string(&file).map(|s| s == text).unwrap_or(false) {
        return;
    }
    let _ = std::fs::create_dir_all(&apps);
    if std::fs::write(&file, text).is_ok() {
        crate::log::line("desktop", "wrote the .desktop file (KWin screenshot permission + menu entry)");
        // KWin finds the permission through KDE's app cache (ksycoca): refresh it now rather than on its own time
        if !test_mode() { let _ = std::process::Command::new("kbuildsycoca6").stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null()).spawn(); }
    }
    for (s, png) in [
        (48, &include_bytes!("../assets/icons/lasso-48.png")[..]),
        (128, &include_bytes!("../assets/icons/lasso-128.png")[..]),
        (256, &include_bytes!("../assets/icons/lasso-256.png")[..]),
    ] {
        let d = data_home().join(format!("icons/hicolor/{s}x{s}/apps"));
        let _ = std::fs::create_dir_all(&d);
        let _ = std::fs::write(d.join(format!("{APP_ID}.png")), png);
    }
    let d = data_home().join("icons/hicolor/scalable/apps");
    let _ = std::fs::create_dir_all(&d);
    let _ = std::fs::write(d.join(format!("{APP_ID}.svg")), include_bytes!("../assets/icons/lasso.svg"));
}
