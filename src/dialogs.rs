//! File dialogs: Linux = the xdg-desktop-portal FileChooser (KDE / GNOME show their own dialog);
//! Windows = the common dialog (win.rs). Both run off the UI thread; the answer comes back on the event loop.

use std::path::PathBuf;

#[cfg(target_os = "linux")]
pub fn pick(save: bool, title: &str, name: &str, folder: &std::path::Path) -> Option<PathBuf> {
    use dbus::arg::{PropMap, RefArg, Variant};
    use dbus::blocking::Connection;
    use dbus::message::MatchRule;
    use std::time::{Duration, Instant};
    let conn = Connection::new_session().ok()?;
    let token = format!("lassofc{}", std::process::id());
    let sender = conn.unique_name().trim_start_matches(':').replace('.', "_");
    let path = format!("/org/freedesktop/portal/desktop/request/{sender}/{token}");
    let got: std::sync::Arc<std::sync::Mutex<Option<(u32, PropMap)>>> = Default::default();
    let g2 = got.clone();
    conn.add_match(MatchRule::new_signal("org.freedesktop.portal.Request", "Response").with_path(path), move |(c, r): (u32, PropMap), _, _| {
        *g2.lock().unwrap() = Some((c, r));
        false
    })
    .ok()?;
    let mut o: PropMap = PropMap::new();
    o.insert("handle_token".into(), Variant(Box::new(token) as Box<dyn RefArg>));
    let mut f = folder.to_string_lossy().as_bytes().to_vec();
    f.push(0);
    o.insert("current_folder".into(), Variant(Box::new(f) as Box<dyn RefArg>));
    if save {
        o.insert("current_name".into(), Variant(Box::new(save_name(name, "png").0) as Box<dyn RefArg>));
    } else if name == "<folder>" {
        o.insert("directory".into(), Variant(Box::new(true) as Box<dyn RefArg>));
    } else {
        let filters: Vec<(String, Vec<(u32, String)>)> = vec![("Pictures".into(), vec![(1, "image/png".into()), (1, "image/jpeg".into()), (1, "image/webp".into()), (1, "image/bmp".into()), (1, "image/gif".into())])];
        o.insert("filters".into(), Variant(Box::new(filters) as Box<dyn RefArg>));
    }
    let p = conn.with_proxy("org.freedesktop.portal.Desktop", "/org/freedesktop/portal/desktop", Duration::from_secs(10));
    let _: (dbus::Path,) = p.method_call("org.freedesktop.portal.FileChooser", if save { "SaveFile" } else { "OpenFile" }, ("", title, o)).ok()?;
    let until = Instant::now() + Duration::from_secs(600);
    while got.lock().unwrap().is_none() && Instant::now() < until {
        let _ = conn.process(Duration::from_millis(300));
    }
    let (code, res) = got.lock().unwrap().take()?;
    if code != 0 {
        return None;
    }
    let uris: Vec<String> = res.get("uris").and_then(|v| v.0.as_iter()).map(|it| it.filter_map(|x| x.as_str().map(String::from)).collect()).unwrap_or_default();
    uris.first().map(|u| PathBuf::from(crate::util::url_decode(u.trim_start_matches("file://"))))
}

#[cfg(windows)]
pub fn pick(save: bool, title: &str, name: &str, folder: &std::path::Path) -> Option<PathBuf> {
    crate::win::file_dialog(save, title, name, folder)
}

/// A save dialog's file name: `name` may carry the picture's extension ("shot.jpg", the kind the bytes will be);
/// without a known one it gets `default_ext`. Returns (the name to show, its extension; Windows: `lpstrDefExt`).
pub fn save_name(name: &str, default_ext: &str) -> (String, String) {
    const KNOWN: [&str; 6] = ["png", "jpg", "jpeg", "webp", "bmp", "gif"];
    if let Some((_, e)) = name.rsplit_once('.') {
        let e = e.to_ascii_lowercase();
        if KNOWN.contains(&e.as_str()) {
            return (name.to_string(), e);
        }
    }
    (format!("{name}.{default_ext}"), default_ext.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn save_names() {
        assert_eq!(save_name("Lasso 13-11-2026", "png"), ("Lasso 13-11-2026.png".into(), "png".into()));
        assert_eq!(save_name("shot.JPG", "png"), ("shot.JPG".into(), "jpg".into()));
        assert_eq!(save_name("v1.2 notes", "webp"), ("v1.2 notes.webp".into(), "webp".into()));
        assert_eq!(save_name("pic.webp", "png"), ("pic.webp".into(), "webp".into()));
    }
}
