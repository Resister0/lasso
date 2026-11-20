//! The clipboard. KDE / wlroots: Lasso's own Wayland data-control source (src/wl.rs) - no window focus needed,
//! and it stays valid as long as Lasso runs (it lives in the tray). Elsewhere on Linux: `wl-copy` / `xclip`.
//! Windows: arboard.

use crate::shapes::Pic;
#[cfg(target_os = "linux")]
use std::sync::Arc;

#[cfg(target_os = "linux")]
const TEXT_MIMES: [&str; 5] = ["text/plain;charset=utf-8", "text/plain", "UTF8_STRING", "STRING", "TEXT"];

pub fn set_image(bytes: Vec<u8>, mime: &str) {
    #[cfg(target_os = "linux")]
    {
        if crate::wl::set_clipboard(vec![(mime.to_string(), Arc::new(bytes.clone()))]) {
            return;
        }
        pipe_to(&["wl-copy", "--type", mime], &bytes).or_else(|_| pipe_to(&["xclip", "-selection", "clipboard", "-t", mime], &bytes)).unwrap_or_else(|e| crate::log::line("clipboard", &e));
    }
    #[cfg(windows)]
    {
        let _ = mime;
        crate::win::clipboard_set_image(bytes);
    }
}

pub fn set_text(t: &str) {
    #[cfg(target_os = "linux")]
    {
        let data = Arc::new(t.as_bytes().to_vec());
        if crate::wl::set_clipboard(TEXT_MIMES.iter().map(|m| (m.to_string(), data.clone())).collect()) {
            return;
        }
        let _ = pipe_to(&["wl-copy"], t.as_bytes()).or_else(|_| pipe_to(&["xclip", "-selection", "clipboard"], t.as_bytes()));
    }
    #[cfg(windows)]
    {
        if let Ok(mut c) = arboard::Clipboard::new() {
            let _ = c.set_text(t.to_string());
        }
    }
}

#[cfg(target_os = "linux")]
fn pipe_to(cmd: &[&str], data: &[u8]) -> Result<(), String> {
    use std::io::Write;
    let mut ch = std::process::Command::new(cmd[0]).args(&cmd[1..]).stdin(std::process::Stdio::piped()).spawn().map_err(|e| format!("{}: {e}", cmd[0]))?;
    ch.stdin.take().ok_or("stdin")?.write_all(data).map_err(|e| e.to_string())?;
    // wait for it in the background (no zombie per copy)
    std::thread::spawn(move || {
        let _ = ch.wait();
    });
    Ok(())
}

/// A picture from the clipboard (PNG / JPEG / WebP, or a copied picture file). `done` runs later on the event loop.
pub fn read_image(done: impl FnOnce(Result<Pic, String>) + Send + 'static) {
    #[cfg(target_os = "linux")]
    {
        let types = crate::wl::clipboard_types();
        if let Some(m) = ["image/png", "image/jpeg", "image/webp", "image/bmp"].iter().find(|m| types.iter().any(|t| t == *m)) {
            crate::wl::read_clipboard(m, move |b| done(crate::export::decode(&b)));
            return;
        }
        if types.iter().any(|t| t == "text/uri-list") {
            crate::wl::read_clipboard("text/uri-list", move |b| {
                let s = String::from_utf8_lossy(&b).to_string();
                let first = s.lines().find(|l| l.starts_with("file://")).map(|l| crate::util::url_decode(l.trim_start_matches("file://").trim()));
                done(match first {
                    Some(p) => std::fs::read(&p).map_err(|e| e.to_string()).and_then(|b| crate::export::decode(&b)),
                    None => Err("No picture on the clipboard".into()),
                })
            });
            return;
        }
        let _ = slint::invoke_from_event_loop(move || done(Err("No picture on the clipboard".into())));
    }
    #[cfg(windows)]
    {
        let r = crate::win::clipboard_read_image();
        let _ = slint::invoke_from_event_loop(move || done(r));
    }
}

/// Several picture files at once (the library's multi-select): chat apps and file managers paste the files.
pub fn set_files(paths: &[&str]) {
    #[cfg(target_os = "linux")]
    {
        let uris: String = paths.iter().map(|p| format!("file://{}\r\n", crate::util::url_encode_path(p))).collect();
        let gnome = format!("copy\n{}", uris.replace("\r\n", "\n").trim_end());
        let items = vec![
            ("text/uri-list".to_string(), Arc::new(uris.clone().into_bytes())),
            ("x-special/gnome-copied-files".to_string(), Arc::new(gnome.into_bytes())),
            ("text/plain;charset=utf-8".to_string(), Arc::new(paths.join("\n").into_bytes())),
        ];
        if !crate::wl::set_clipboard(items) {
            let _ = pipe_to(&["wl-copy", "--type", "text/uri-list"], uris.as_bytes());
        }
    }
    #[cfg(windows)]
    crate::win::clipboard_files(paths);
}
