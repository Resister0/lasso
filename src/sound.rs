//! Optional sounds (Settings > Start, off by default): a camera shutter when a picture is copied / saved; the screen
//! freezing is silent.
//! 25 Nov 2026: were sine chimes made in code - the done one was Echo's "clip saved" notes (880 + 1320 Hz), so with
//! both apps running the user couldn't tell which one made the sound. Sounds made in code didn't sound right either
//! (tried shutter clicks, pop, marimba, glass); the user picked the desktop's own "camera-shutter" (freedesktop sound
//! theme, the same one screenshot tools use). Linux: that file, played by the desktop's player; Windows: the system
//! "asterisk" sound.

#[derive(Clone, Copy)]
pub enum Snd {
    Freeze,
    Done,
}

pub fn play(s: Snd) {
    if matches!(s, Snd::Freeze) || !crate::config::Config::load().on("sounds") {
        return;
    }
    #[cfg(target_os = "linux")]
    std::thread::spawn(|| {
        let quiet = |c: &mut std::process::Command| c.stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null()).status().map(|st| st.success()).unwrap_or(false);
        if let Some(f) = theme_file("camera-shutter") {
            for player in ["pw-play", "paplay"] {
                if quiet(std::process::Command::new(player).arg(&f)) {
                    return;
                }
            }
        }
        // no sound file found (or no player that reads .oga): libcanberra finds the theme's sound by its name
        let _ = quiet(std::process::Command::new("canberra-gtk-play").args(["-i", "camera-shutter"]));
    });
    #[cfg(windows)]
    unsafe {
        use windows_sys::Win32::System::Diagnostics::Debug::MessageBeep;
        use windows_sys::Win32::UI::WindowsAndMessaging::MB_ICONASTERISK;
        MessageBeep(MB_ICONASTERISK);
    }
}

/// A sound from the installed sound themes (freedesktop naming spec): `<data dir>/sounds/<theme>/stereo/<name>.oga`.
/// The freedesktop theme first (it has camera-shutter; KDE's Ocean doesn't), then any other theme.
#[cfg(target_os = "linux")]
fn theme_file(name: &str) -> Option<std::path::PathBuf> {
    let mut dirs: Vec<std::path::PathBuf> = vec![];
    if let Some(h) = std::env::var_os("XDG_DATA_HOME").filter(|v| !v.is_empty()) {
        dirs.push(h.into());
    } else if let Some(h) = std::env::var_os("HOME") {
        dirs.push(std::path::PathBuf::from(h).join(".local/share"));
    }
    let sys = std::env::var("XDG_DATA_DIRS").unwrap_or_default();
    dirs.extend(sys.split(':').filter(|d| !d.is_empty()).map(std::path::PathBuf::from));
    // an AppImage can change XDG_DATA_DIRS: always look in the usual system folders too
    dirs.push("/usr/share".into());
    dirs.push("/usr/local/share".into());
    theme_file_in(&dirs, name)
}

#[cfg(target_os = "linux")]
fn theme_file_in(dirs: &[std::path::PathBuf], name: &str) -> Option<std::path::PathBuf> {
    let exts = ["oga", "ogg", "wav"];
    for d in dirs {
        for e in exts {
            let p = d.join("sounds/freedesktop/stereo").join(format!("{name}.{e}"));
            if p.is_file() {
                return Some(p);
            }
        }
    }
    for d in dirs {
        let Ok(themes) = std::fs::read_dir(d.join("sounds")) else { continue };
        for t in themes.flatten() {
            for e in exts {
                let p = t.path().join("stereo").join(format!("{name}.{e}"));
                if p.is_file() {
                    return Some(p);
                }
            }
        }
    }
    None
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    #[test]
    fn finds_freedesktop_first_then_other_themes() {
        let base = std::env::temp_dir().join(format!("lasso-sound-test-{}", std::process::id()));
        let a = base.join("a");
        let b = base.join("b");
        std::fs::create_dir_all(a.join("sounds/ocean/stereo")).unwrap();
        std::fs::create_dir_all(b.join("sounds/freedesktop/stereo")).unwrap();
        std::fs::write(a.join("sounds/ocean/stereo/camera-shutter.oga"), b"x").unwrap();
        std::fs::write(b.join("sounds/freedesktop/stereo/camera-shutter.oga"), b"x").unwrap();
        let dirs = [a.clone(), b.clone()];
        assert_eq!(super::theme_file_in(&dirs, "camera-shutter"), Some(b.join("sounds/freedesktop/stereo/camera-shutter.oga")));
        std::fs::remove_dir_all(b.join("sounds/freedesktop")).unwrap();
        assert_eq!(super::theme_file_in(&dirs, "camera-shutter"), Some(a.join("sounds/ocean/stereo/camera-shutter.oga")));
        assert_eq!(super::theme_file_in(&dirs, "nope"), None);
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn this_pc_has_the_shutter() {
        // informational: prints where the sound would come from on the machine running the tests
        eprintln!("camera-shutter: {:?}", super::theme_file("camera-shutter"));
    }
}
