//! The mini library (notes 6d): every capture is kept with its shapes still editable - the frozen picture
//! (`screen.png`), the box + shapes (`state.json`), pasted pictures, a small `thumb.png`. Esc-restore after a
//! restart and "Edit" from the Library window both come from here. Plus the pictures in the save folder (flat,
//! no shapes). Saving an edit keeps the untouched original here ("Back to original").
//! Default: the last 500 captures / 90 days (settings); the save folder is never cleaned. Deleting = to the trash.

use crate::editor::Mode;
use crate::shapes::{Pic, Shape, R};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

#[derive(Clone, Debug, Serialize, Deserialize, Default)]
pub struct State {
    pub created: i64,
    #[serde(default)]
    pub boxr: Option<R>,
    #[serde(default)]
    pub shapes: Vec<Shape>,
    #[serde(default)]
    pub mode: Option<Mode>,
    #[serde(default)]
    pub output: String,
    /// files this capture was saved as
    #[serde(default)]
    pub saved: Vec<String>,
    /// a picture from the save folder / opened file: where it came from
    #[serde(default)]
    pub source: String,
    #[serde(default)]
    pub link: String,
    #[serde(default)]
    pub link_expires: i64,
    /// pasted pictures: id -> file name in the folder
    #[serde(default)]
    pub images: HashMap<String, String>,
    /// the app / game the box was on ("Library: group by app")
    #[serde(default)]
    pub app: String,
    /// the text in the picture (OCR in the background, when OCR works) - the library's search
    #[serde(default)]
    pub text: String,
    /// the undo steps (box + shapes, oldest first): Ctrl+Z still works after the picture is opened again (22 Nov 2026)
    #[serde(default)]
    pub history: Vec<(Option<R>, Vec<Shape>)>,
}

pub struct Item {
    /// library id (folder name) or "" for a plain file
    pub id: String,
    pub path: PathBuf,
    pub thumb: PathBuf,
    pub created: i64,
    pub shared: bool,
    pub editable: bool,
    pub app: String,
    pub text: String,
}

pub fn dir(id: &str) -> PathBuf {
    crate::paths::library_dir().join(id)
}

pub fn new_id() -> String {
    let (y, mo, d, h, mi, s) = crate::util::local_now();
    let n = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.subsec_nanos()).unwrap_or(0) % 10000;
    format!("{y:04}{mo:02}{d:02}-{h:02}{mi:02}{s:02}-{n:04}")
}

/// Writes (or rewrites) a capture. Slow parts (PNG) - call from a worker thread.
/// thumb.png made by export::tile_thumb (22 Nov 2026: bigger tiles); without this file next to it it's an older,
/// smaller one and the library makes it again when it shows it
pub const THUMB_MARK: &str = "thumb.v2";

pub fn store(id: &str, base: &Pic, base_dirty: bool, st: &State, images: &HashMap<String, Arc<Pic>>, thumb: Option<&Pic>) -> Result<(), String> {
    let d = dir(id);
    std::fs::create_dir_all(&d).map_err(|e| e.to_string())?;
    if base_dirty || !d.join("screen.png").exists() {
        crate::export::write_new(&d.join("screen.png"), &crate::export::png(base, true))?;
    }
    let mut st = st.clone();
    for (k, p) in images {
        let f = format!("{k}.png");
        if !d.join(&f).exists() {
            crate::export::write_new(&d.join(&f), &crate::export::png(p, true))?;
        }
        st.images.insert(k.clone(), f);
    }
    if let Some(t) = thumb {
        crate::export::write_new(&d.join("thumb.png"), &crate::export::png(&crate::export::tile_thumb(t), true))?;
        let _ = std::fs::write(d.join(THUMB_MARK), b"");
    }
    let json = serde_json::to_string_pretty(&st).map_err(|e| e.to_string())?;
    crate::export::write_new(&d.join("state.json"), json.as_bytes())?;
    // Echo Cloud backup (when it's on): look again soon
    crate::share::backup::kick();
    Ok(())
}

// --- one store worker: writes of all captures in order, never two at once; a cancelled capture is never written again

type Job = Box<dyn FnOnce() + Send>;

fn worker() -> &'static std::sync::Mutex<std::sync::mpsc::Sender<Job>> {
    static W: std::sync::OnceLock<std::sync::Mutex<std::sync::mpsc::Sender<Job>>> = std::sync::OnceLock::new();
    W.get_or_init(|| {
        let (tx, rx) = std::sync::mpsc::channel::<Job>();
        let _ = std::thread::Builder::new().name("library".into()).spawn(move || {
            for job in rx {
                // one bad job (a panic) must not stop every later write of every capture
                if std::panic::catch_unwind(std::panic::AssertUnwindSafe(job)).is_err() {
                    crate::log::line("library", "a write failed (panic) - the next ones go on");
                }
            }
        });
        std::sync::Mutex::new(tx)
    })
}

fn cancelled() -> &'static std::sync::Mutex<std::collections::HashSet<String>> {
    static C: std::sync::OnceLock<std::sync::Mutex<std::collections::HashSet<String>>> = std::sync::OnceLock::new();
    C.get_or_init(Default::default)
}

/// Runs `f(id)` on the library worker, after every earlier job - unless that capture was cancelled by then
pub fn queue(id: &str, f: impl FnOnce(&str) + Send + 'static) {
    let id = id.to_string();
    let job: Job = Box::new(move || {
        if cancelled().lock().map(|c| c.contains(&id)).unwrap_or(false) {
            return;
        }
        f(&id);
    });
    if let Ok(tx) = worker().lock() {
        let _ = tx.send(job);
    }
}

/// Esc with nothing picked: this capture's folder goes (after its pending writes), and nothing writes it again
pub fn cancel(id: &str) {
    let d = dir(id);
    cancel_with(id, move || {
        let _ = std::fs::remove_dir_all(d);
    });
}

/// Quit: waits (up to `max`) until every queued write is done - a capture closed just before Quit is kept.
/// true = all written.
pub fn drain(max: std::time::Duration) -> bool {
    let (tx, rx) = std::sync::mpsc::channel();
    if let Ok(w) = worker().lock() {
        let _ = w.send(Box::new(move || {
            let _ = tx.send(());
        }));
    }
    rx.recv_timeout(max).is_ok()
}

/// The text in a picture for the search (OCR, up to seconds): on its own thread, so it never holds up the writes.
/// Only the newest picture of a capture is read; the text is written by the store worker (in order with the rest).
pub fn read_text_later(id: &str, pic: Pic) {
    type Pending = std::sync::Mutex<HashMap<String, Pic>>;
    static PENDING: std::sync::OnceLock<Pending> = std::sync::OnceLock::new();
    static OCR: std::sync::OnceLock<std::sync::Mutex<std::sync::mpsc::Sender<String>>> = std::sync::OnceLock::new();
    let pending = PENDING.get_or_init(Default::default);
    let tx = OCR.get_or_init(|| {
        let (tx, rx) = std::sync::mpsc::channel::<String>();
        let _ = std::thread::Builder::new().name("ocr".into()).spawn(move || {
            for id in rx {
                let Some(pic) = PENDING.get().and_then(|p| p.lock().ok()?.remove(&id)) else { continue };
                let words = std::panic::catch_unwind(|| crate::ocr::read(&pic));
                if let Ok(Ok(words)) = words {
                    let text = crate::ocr::text(&words);
                    queue(&id, move |id| set_text(id, &text));
                }
            }
        });
        std::sync::Mutex::new(tx)
    });
    let fresh = pending.lock().map(|mut p| p.insert(id.to_string(), pic).is_none()).unwrap_or(false);
    if fresh {
        if let Ok(tx) = tx.lock() {
            let _ = tx.send(id.to_string());
        }
    }
}

/// `cancel` with its own clean-up (tests: no real library folder)
fn cancel_with(id: &str, remove: impl FnOnce() + Send + 'static) {
    if let Ok(mut c) = cancelled().lock() {
        c.insert(id.to_string());
    }
    if let Ok(tx) = worker().lock() {
        let _ = tx.send(Box::new(remove));
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn a_cancelled_capture_is_never_written_again() {
        // B4: store -> Esc (cancel) -> a late store of the same capture must not bring it back
        use std::sync::{Arc, Mutex};
        let log: Arc<Mutex<Vec<String>>> = Default::default();
        let id = "test-cancel-0001";
        let l = log.clone();
        super::queue(id, move |_| l.lock().unwrap().push("first store".into()));
        let l = log.clone();
        super::cancel_with(id, move || l.lock().unwrap().push("removed".into()));
        let l = log.clone();
        super::queue(id, move |_| l.lock().unwrap().push("late store".into()));
        // wait for the worker: a job of another capture after them
        let (tx, rx) = std::sync::mpsc::channel();
        super::queue("test-cancel-sync", move |_| {
            let _ = tx.send(());
        });
        rx.recv_timeout(std::time::Duration::from_secs(5)).unwrap();
        let log = log.lock().unwrap();
        // the folder is removed, and no store of this capture runs after the cancel (not even one queued before it)
        assert!(log.contains(&"removed".to_string()));
        assert!(!log.contains(&"late store".to_string()));
        assert_eq!(log.last().map(|s| s.as_str()), Some("removed"));
    }

    #[test]
    fn a_panicking_write_does_not_stop_later_ones_and_drain_waits() {
        // D4: one bad job must not kill the worker; Quit waits for the queued writes
        use std::sync::{Arc, Mutex};
        let log: Arc<Mutex<Vec<&str>>> = Default::default();
        super::queue("test-panic-0001", |_| panic!("bad job"));
        let l = log.clone();
        super::queue("test-panic-0002", move |_| {
            std::thread::sleep(std::time::Duration::from_millis(200));
            l.lock().unwrap().push("later write");
        });
        assert!(super::drain(std::time::Duration::from_secs(5)));
        assert_eq!(*log.lock().unwrap(), vec!["later write"]);
    }

    #[test]
    fn only_a_folder_without_state_is_an_orphan() {
        // D7: a state.json a newer Lasso wrote (unreadable here) must never make prune delete the capture
        let d = std::env::temp_dir().join(format!("lasso-orphan-test-{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        assert!(super::orphan(&d));
        std::fs::write(d.join("state.json"), "").unwrap();
        assert!(super::orphan(&d));
        std::fs::write(d.join("state.json"), r#"{"created": 1, "shapes": [{"kind": "hologram"}]}"#).unwrap();
        assert!(!super::orphan(&d));
        let _ = std::fs::remove_dir_all(&d);
    }
}

/// The opened file's own bytes, for "Back to original"
pub fn keep_original(id: &str, src: &Path) {
    let ext = src.extension().map(|e| e.to_string_lossy().to_lowercase()).unwrap_or_else(|| "png".into());
    if let Ok(b) = std::fs::read(src) {
        let _ = crate::export::write_new(&dir(id).join(format!("original.{ext}")), &b);
    }
}

/// The capture was opened from the user's own file (Ctrl+O, drag & drop): its bytes are kept as original.<ext>
pub fn has_original(id: &str) -> bool {
    std::fs::read_dir(dir(id)).map(|rd| rd.flatten().any(|e| e.file_name().to_string_lossy().starts_with("original."))).unwrap_or(false)
}

/// The Library deleted this capture: nothing queued writes it again, a folder a write in flight made goes too
pub fn forget(id: &str) {
    cancel(id);
}

/// Fills in the text of a capture (OCR, in the background): keeps everything else of state.json as it is
pub fn set_text(id: &str, text: &str) {
    if let Some(mut st) = load_state(id) {
        st.text = text.to_string();
        if let Ok(json) = serde_json::to_string_pretty(&st) {
            let _ = crate::export::write_new(&dir(id).join("state.json"), json.as_bytes());
        }
    }
}

pub fn load_state(id: &str) -> Option<State> {
    serde_json::from_str(&std::fs::read_to_string(dir(id).join("state.json")).ok()?).ok()
}

pub fn load(id: &str) -> Result<(Pic, State, HashMap<String, Arc<Pic>>), String> {
    let d = dir(id);
    let st = load_state(id).ok_or("this capture is gone")?;
    let base = crate::export::decode(&std::fs::read(d.join("screen.png")).map_err(|e| e.to_string())?)?;
    let mut imgs = HashMap::new();
    for (k, f) in &st.images {
        if let Ok(b) = std::fs::read(d.join(f)) {
            if let Ok(p) = crate::export::decode(&b) {
                imgs.insert(k.clone(), Arc::new(p));
            }
        }
    }
    Ok((base, st, imgs))
}

/// The newest `n` captures that have a thumbnail, newest first (the tray flyout). Only those folders are read - folder
/// names start with the date and time, so no full pass over the Library is needed.
pub fn recent(n: usize) -> Vec<Item> {
    let Ok(rd) = std::fs::read_dir(crate::paths::library_dir()) else { return vec![] };
    let mut names: Vec<String> = rd.flatten().map(|e| e.file_name().to_string_lossy().to_string()).collect();
    names.sort_unstable_by(|a, b| b.cmp(a));
    let mut out = vec![];
    for id in names {
        if out.len() >= n {
            break;
        }
        let d = dir(&id);
        let thumb = d.join("thumb.png");
        if !thumb.is_file() {
            continue;
        }
        let Some(st) = load_state(&id) else { continue };
        out.push(Item { id, path: d.join("screen.png"), thumb, created: st.created, shared: !st.link.is_empty(), editable: true, app: st.app, text: st.text });
    }
    out.sort_by(|a, b| b.created.cmp(&a.created));
    out
}

/// Newest first: library captures + pictures in the save folder that aren't from a capture.
pub fn list(save_dir: &Path) -> Vec<Item> {
    let mut out = vec![];
    let mut saved: std::collections::HashSet<PathBuf> = Default::default();
    if let Ok(rd) = std::fs::read_dir(crate::paths::library_dir()) {
        for e in rd.flatten() {
            let id = e.file_name().to_string_lossy().to_string();
            if let Some(st) = load_state(&id) {
                for s in &st.saved {
                    saved.insert(PathBuf::from(s));
                }
                let d = e.path();
                out.push(Item {
                    id: id.clone(),
                    path: d.join("screen.png"),
                    thumb: d.join("thumb.png"),
                    created: st.created,
                    shared: !st.link.is_empty(),
                    editable: true,
                    app: st.app.clone(),
                    text: st.text.clone(),
                });
            }
        }
    }
    if let Ok(rd) = std::fs::read_dir(save_dir) {
        for e in rd.flatten() {
            let p = e.path();
            let ext = p.extension().map(|x| x.to_string_lossy().to_lowercase()).unwrap_or_default();
            if !["png", "jpg", "jpeg", "webp"].contains(&ext.as_str()) || saved.contains(&p) {
                continue;
            }
            let created = e.metadata().and_then(|m| m.modified()).ok().and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok()).map(|d| d.as_secs() as i64).unwrap_or(0);
            out.push(Item { id: String::new(), thumb: p.clone(), path: p, created, shared: false, editable: false, app: String::new(), text: String::new() });
        }
    }
    // game screenshots (Settings > Library > Game screenshots): shown, never copied
    let have: std::collections::HashSet<PathBuf> = out.iter().filter(|i| i.id.is_empty()).map(|i| i.path.clone()).collect();
    for f in crate::gamepics::all(&crate::config::Config::load()) {
        if !have.contains(&f.path) && !saved.contains(&f.path) {
            out.push(Item { id: String::new(), path: f.path, thumb: f.thumb, created: f.created, shared: false, editable: false, app: f.app, text: String::new() });
        }
    }
    out.sort_by(|a, b| b.created.cmp(&a.created));
    out
}

static LAST_PRUNE: std::sync::atomic::AtomicI64 = std::sync::atomic::AtomicI64::new(0);

/// After each new capture (Lasso runs for weeks in the tray - the limit must not wait for the next login): a full
/// pass only when there are more folders than `keep`, or the last pass is a day old (for `days`). Store worker only.
pub fn prune_after_new(keep: usize, days: i64) {
    let n = std::fs::read_dir(crate::paths::library_dir()).map(|rd| rd.flatten().filter(|e| e.path().is_dir()).count()).unwrap_or(0);
    let day_old = crate::util::unix_now() - LAST_PRUNE.load(std::sync::atomic::Ordering::Relaxed) > 86400;
    if n > keep.max(1) || (days > 0 && day_old) {
        prune(keep, days);
    }
}

/// Keep the newest `keep` captures and none older than `days` (the save folder is never touched).
pub fn prune(keep: usize, days: i64) {
    let now = crate::util::unix_now();
    LAST_PRUNE.store(now, std::sync::atomic::Ordering::Relaxed);
    let mut caps: Vec<(i64, String)> = vec![];
    if let Ok(rd) = std::fs::read_dir(crate::paths::library_dir()) {
        for e in rd.flatten() {
            let id = e.file_name().to_string_lossy().to_string();
            if let Some(st) = load_state(&id) {
                caps.push((st.created, id));
            }
        }
    }
    // folders with NO state.json (a crash before the first write) and older than a day go too. One that has a
    // state.json this Lasso can't read is kept: a newer Lasso wrote it (review D7 - an older build at login must not
    // delete newer captures).
    if let Ok(rd) = std::fs::read_dir(crate::paths::library_dir()) {
        for e in rd.flatten() {
            let id = e.file_name().to_string_lossy().to_string();
            let old = e.metadata().and_then(|m| m.modified()).ok().and_then(|t| t.elapsed().ok()).map(|d| d.as_secs() > 86400).unwrap_or(false);
            if !id.starts_with('.') && e.path().is_dir() && orphan(&e.path()) && old {
                let _ = std::fs::remove_dir_all(e.path());
            }
        }
    }
    caps.sort_by(|a, b| b.0.cmp(&a.0));
    for (i, (t, id)) in caps.iter().enumerate() {
        if i >= keep.max(1) || (days > 0 && now - t > days * 86400) {
            let _ = std::fs::remove_dir_all(dir(id));
        }
    }
}

/// A capture folder without a usable state.json at all (missing or empty) - never one that's only unreadable
fn orphan(d: &Path) -> bool {
    std::fs::metadata(d.join("state.json")).map(|m| m.len() == 0).unwrap_or(true)
}

/// Delete a capture: its library folder + the pictures it saved (both to the trash; a picture the user opened
/// themselves stays). `extra_saved` = saved paths the caller knows that may not be in state.json yet (a write still
/// queued). The caller tells the running Lasso to forget it. Ok(n) = how many things went to the trash.
pub fn delete_capture(id: &str, extra_saved: &[String]) -> Result<usize, String> {
    let st = load_state(id);
    let mut saved: Vec<String> = st.as_ref().map(|s| s.saved.clone()).unwrap_or_default();
    for s in extra_saved {
        if !saved.contains(s) {
            saved.push(s.clone());
        }
    }
    let own = st.as_ref().map(|s| s.source.clone()).filter(|s| !s.is_empty() && has_original(id));
    let mut n = 0;
    let d = dir(id);
    if d.exists() {
        trash(&d)?;
        n += 1;
    }
    for s in saved {
        if own.as_deref() == Some(s.as_str()) {
            continue;
        }
        let p = Path::new(&s);
        if p.exists() && trash(p).is_ok() {
            n += 1;
        }
    }
    // after the trash (cancel removes the folder): a write still queued for it never brings it back
    cancel(id);
    Ok(n)
}

/// To the trash / recycle bin - never a hard delete for the user's own pictures.
pub fn trash(p: &Path) -> Result<(), String> {
    #[cfg(target_os = "linux")]
    {
        // freedesktop trash spec: files/ + info/<name>.trashinfo
        let base = std::env::var_os("XDG_DATA_HOME").map(PathBuf::from).unwrap_or_else(|| PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(".local/share"));
        let base = if let Some(t) = std::env::var_os("LASSO_HOME") { PathBuf::from(t).join("Trash") } else { base.join("Trash") };
        let files = base.join("files");
        let info = base.join("info");
        std::fs::create_dir_all(&files).map_err(|e| e.to_string())?;
        std::fs::create_dir_all(&info).map_err(|e| e.to_string())?;
        let name = p.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_else(|| "lasso".into());
        let mut dest = files.join(&name);
        let mut n = 2;
        while dest.exists() {
            dest = files.join(format!("{n} {name}"));
            n += 1;
        }
        let dn = dest.file_name().unwrap().to_string_lossy().to_string();
        let (y, mo, d, h, mi, s) = crate::util::local_now();
        let abs = std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf());
        let text = format!("[Trash Info]\nPath={}\nDeletionDate={y:04}-{mo:02}-{d:02}T{h:02}:{mi:02}:{s:02}\n", abs.to_string_lossy());
        std::fs::write(info.join(format!("{dn}.trashinfo")), text).map_err(|e| e.to_string())?;
        if std::fs::rename(p, &dest).is_err() {
            // another disk: copy + remove
            if p.is_dir() {
                return Err("can't move to the trash".into());
            }
            std::fs::copy(p, &dest).map_err(|e| e.to_string())?;
            std::fs::remove_file(p).map_err(|e| e.to_string())?;
        }
        Ok(())
    }
    #[cfg(windows)]
    {
        crate::win::recycle(p)
    }
}

/// A capture as the finished picture (the box + every shape), like Copy on the frozen screen
pub fn finished(id: &str) -> Result<Pic, String> {
    let (base, st, images) = load(id)?;
    let (w, h) = (base.w as i32, base.h as i32);
    let b = st.boxr.map(|r| r.ir().clamp(w, h)).filter(|r| !r.is_empty()).unwrap_or(crate::shapes::IR { x: 0, y: 0, w, h });
    let doc = crate::shapes::Doc { base: Arc::new(base), shapes: st.shapes, images };
    Ok(crate::shapes::flatten(&doc, b))
}

/// "Back to original": the saved file gets the untouched picture again (the box without any shape)
pub fn back_to_original(id: &str) -> Result<(), String> {
    let (base, st, _) = load(id)?;
    let target = st.saved.last().cloned().filter(|s| !s.is_empty()).or_else(|| Some(st.source.clone()).filter(|s| !s.is_empty())).ok_or("This capture was never saved to a file")?;
    let (w, h) = (base.w as i32, base.h as i32);
    let b = st.boxr.map(|r| r.ir().clamp(w, h)).filter(|r| !r.is_empty()).unwrap_or(crate::shapes::IR { x: 0, y: 0, w, h });
    let target = Path::new(&target);
    let ext = target.extension().map(|e| e.to_string_lossy().to_lowercase()).unwrap_or_default();
    // an opened file: its own bytes, untouched
    let orig = dir(id).join(format!("original.{ext}"));
    if let Ok(b) = std::fs::read(&orig) {
        return crate::export::write_new(target, &b);
    }
    let pic = base.crop(b);
    let fmt = if ext == "jpeg" { "jpg".to_string() } else { ext };
    crate::export::write_new(target, &crate::export::encode(&pic, &fmt, false, false).bytes)
}
