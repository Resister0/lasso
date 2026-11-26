//! The backup (26 Nov 2026, the user: "echo cloud, what it covers selectable by the user, off by default", then "look
//! what Echo does"): WHAT - two switches in Settings > Library > Backup, both off: `backup_captures` (Lasso's captures,
//! as the finished picture) and `backup_games` (the game screenshots / added folders Lasso shows - gamepics.rs). WHERE -
//! as many places as you like: Echo Cloud (`backup_ec`, off by default; needs the Echo Cloud account and a server with
//! `"lasso": 2`) and your own clouds (clouds.rs: Google Drive, pCloud, MEGA, WebDAV, SFTP, S3, a folder / NAS).
//!
//! A worker thread in the always-running Lasso: a round 2 minutes after start, then every 10 minutes, and soon after a
//! new capture or a settings change (`kick`). Newest first, at most 300 pictures a round; a picture is made once and
//! sent to every place that doesn't have it yet. Per place a list of what's there (`backup.json` = Echo Cloud,
//! `backup-place_<id>.json`) - path -> stamp (the capture's state.json time / the game file's size + time) - so each
//! picture goes up once, and again when it changed (a capture edited later). A place that fails (full, offline, signed
//! out) pauses until the next round; the others go on. Deleting a picture on the PC keeps its backups. Settings' "Delete"
//! on the Echo Cloud row removes everything from Echo Cloud; an own place's pictures stay when it's removed.
//! Where: Echo Cloud u/<id>/Lasso/Backup/Captures/<yyyy>/<mm>/<id>.png (+ .../Games/<game>/<file>) - Echo never shows
//! them (it only reads u/<id>/Echo/), they count against the plan; own places <root>/Lasso/Captures/... and Lasso/Games/...
//! Status lines for Settings: `backup-status.txt` (Echo Cloud), `backup-status-place_<id>.txt`.

use super::api::{Api, Fail};
use super::net::Net;
use super::store::Store;
use crate::clouds::{Place, Rc};
use crate::config::Config;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::mpsc::{channel, Sender};
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

static KICK: OnceLock<Mutex<Sender<()>>> = OnceLock::new();
const PER_ROUND: usize = 300;

/// "ec" or "place_<id>"
fn list_path(d: &str) -> PathBuf {
    crate::paths::config_dir().join(if d == "ec" { "backup.json".to_string() } else { format!("backup-{d}.json") })
}

fn status_path(d: &str) -> PathBuf {
    crate::paths::config_dir().join(if d == "ec" { "backup-status.txt".to_string() } else { format!("backup-status-{d}.txt") })
}

/// Bumped by "Delete" on the Echo Cloud row (Settings is its own process): a round that started before stops without
/// saving Echo Cloud's list
fn reset_path() -> PathBuf {
    crate::paths::config_dir().join("backup-reset.txt")
}

fn reset_mark() -> String {
    std::fs::read_to_string(reset_path()).unwrap_or_default()
}

/// Echo Cloud's status line ("" = nothing to say)
pub fn status() -> String {
    std::fs::read_to_string(status_path("ec")).unwrap_or_default().trim().to_string()
}

/// An own place's status line
pub fn place_status(id: u32) -> String {
    std::fs::read_to_string(status_path(&format!("place_{id}"))).unwrap_or_default().trim().to_string()
}

fn set_status(d: &str, s: &str) {
    let _ = std::fs::write(status_path(d), s);
}

fn load(d: &str) -> HashMap<String, String> {
    std::fs::read_to_string(list_path(d)).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default()
}

fn save(d: &str, m: &HashMap<String, String>) {
    let p = list_path(d);
    let tmp = p.with_extension("json.tmp");
    if std::fs::write(&tmp, serde_json::to_string(m).unwrap_or_default()).is_ok() {
        let _ = std::fs::rename(&tmp, &p);
    }
}

/// How many pictures are in Echo Cloud's backup now
pub fn count() -> usize {
    load("ec").len()
}

pub fn place_count(id: u32) -> usize {
    load(&format!("place_{id}")).len()
}

/// A removed place: its lists go (the pictures stay in that cloud)
pub fn forget_place(id: u32) {
    let d = format!("place_{id}");
    let _ = std::fs::remove_file(list_path(&d));
    let _ = std::fs::remove_file(status_path(&d));
}

/// A path part every cloud accepts: letters, digits, space, - _ . ( ) only, no leading dot, max 80
fn part(s: &str) -> String {
    let c: String = s.chars().map(|c| if c.is_alphanumeric() || " -_.()".contains(c) { c } else { '-' }).take(80).collect();
    let c = c.trim().trim_start_matches('.').to_string();
    if c.is_empty() {
        "picture".into()
    } else {
        c
    }
}

fn mtime(p: &std::path::Path) -> i64 {
    std::fs::metadata(p).and_then(|m| m.modified()).ok().and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok()).map(|d| d.as_secs() as i64).unwrap_or(0)
}

/// One picture the switches cover: where it goes under Lasso/ ("Captures/2026/11/<id>.png"), its stamp, how to get it
struct Todo {
    rel: String,
    stamp: String,
    created: i64,
    src: Src,
}

enum Src {
    Capture(String),
    File(PathBuf),
}

/// Everything the switches cover, newest first
fn todo(cfg: &Config) -> Vec<Todo> {
    let mut out = vec![];
    if cfg.on("backup_captures") {
        if let Ok(rd) = std::fs::read_dir(crate::paths::library_dir()) {
            for e in rd.flatten() {
                let id = e.file_name().to_string_lossy().to_string();
                let state = e.path().join("state.json");
                if id.starts_with('.') || !state.is_file() {
                    continue;
                }
                let Some(st) = crate::library::load_state(&id) else { continue };
                let (y, mo, ..) = crate::util::local_from_unix(st.created);
                out.push(Todo { rel: format!("Captures/{y:04}/{mo:02}/{}.png", part(&id)), stamp: format!("c{}", mtime(&state)), created: st.created, src: Src::Capture(id) });
            }
        }
    }
    if cfg.on("backup_games") {
        for f in crate::gamepics::all(cfg) {
            let Some(name) = f.path.file_name().map(|n| n.to_string_lossy().to_string()) else { continue };
            let size = std::fs::metadata(&f.path).map(|m| m.len()).unwrap_or(0);
            out.push(Todo { rel: format!("Games/{}/{}", part(&f.app), part(&name)), stamp: format!("g{size}-{}", f.created), created: f.created, src: Src::File(f.path) });
        }
    }
    out.sort_by(|a, b| b.created.cmp(&a.created));
    out
}

/// The signed-in account's token - backups never use the device session
fn account_api<'a>(http: &'a Net, cfg: &Config) -> Result<Api<'a>, Fail> {
    let token = super::token_of(&Store::app())?;
    Ok(Api { base: super::api_base(cfg), token, http })
}

/// One place this round
enum To<'a> {
    Ec(Api<'a>),
    Own(Rc, Place),
}

struct Dest<'a> {
    to: To<'a>,
    key: String,
    done: HashMap<String, String>,
    alive: bool,
    sent: usize,
}

impl Dest<'_> {
    /// the picture's path in this place
    fn path(&self, rel: &str) -> String {
        match self.to {
            To::Ec(_) => format!("Lasso/Backup/{rel}"),
            To::Own(..) => format!("Lasso/{rel}"),
        }
    }
}

/// "1 picture" / "12 pictures"
pub fn pics(n: usize) -> String {
    if n == 1 { "1 picture".into() } else { format!("{n} pictures") }
}

fn hhmm() -> String {
    let (_, _, _, h, m, _) = crate::util::local_from_unix(crate::util::unix_now());
    format!("{h:02}:{m:02}")
}

fn any_what(c: &Config) -> bool {
    c.on("backup_captures") || c.on("backup_games")
}

fn round() {
    let cfg = Config::load();
    if !any_what(&cfg) {
        return;
    }
    let http = Net;
    let mut dests: Vec<Dest> = vec![];
    if cfg.on("backup_ec") {
        match account_api(&http, &cfg) {
            Err(_) => set_status("ec", "Paused: sign in to Echo Cloud first (Share links > Sign in)"),
            Ok(api) => match api.backup_ready() {
                Ok(()) => dests.push(Dest { to: To::Ec(api), key: "ec".into(), done: load("ec"), alive: true, sent: 0 }),
                Err(Fail::NotReady) => set_status("ec", "Waiting for the Sonela server update (it doesn't take backups yet)"),
                Err(e) => set_status("ec", &format!("Paused: {}", e.text())),
            },
        }
    }
    let own: Vec<Place> = Rc::list_only().places().into_iter().filter(|p| p.on).collect();
    if !own.is_empty() {
        match Rc::app() {
            Ok(rc) => {
                for p in own {
                    let key = format!("place_{}", p.id);
                    dests.push(Dest { done: load(&key), to: To::Own(rc.clone(), p), key, alive: true, sent: 0 });
                }
            }
            Err(e) => {
                for p in own {
                    set_status(&format!("place_{}", p.id), &format!("Paused: {e}"));
                }
            }
        }
    }
    if dests.is_empty() {
        return;
    }
    let mark = reset_mark();
    let tmp = crate::paths::config_dir().join("backup-picture.png");
    let mut made = 0;
    for t in todo(&cfg) {
        let need: Vec<usize> = (0..dests.len()).filter(|&i| dests[i].alive && dests[i].done.get(&dests[i].path(&t.rel)) != Some(&t.stamp)).collect();
        if need.is_empty() {
            continue;
        }
        if made >= PER_ROUND {
            break;
        }
        // switched off meanwhile: stop at once
        let now = Config::load();
        if !any_what(&now) {
            break;
        }
        let bytes = match &t.src {
            Src::Capture(id) => match crate::library::finished(id) {
                Ok(p) => crate::export::png(&p, false),
                Err(e) => {
                    crate::log::line("backup", &format!("skipped {}: {e}", t.rel));
                    continue;
                }
            },
            Src::File(p) => match std::fs::read(p) {
                Ok(b) => b,
                Err(e) => {
                    crate::log::line("backup", &format!("skipped {}: {e}", t.rel));
                    continue;
                }
            },
        };
        made += 1;
        // own places copy a file: a capture's picture is written once, a game picture goes as it is
        let file = match &t.src {
            Src::File(p) => Some(p.clone()),
            Src::Capture(_) if need.iter().any(|&i| matches!(dests[i].to, To::Own(..))) => std::fs::write(&tmp, &bytes).ok().map(|_| tmp.clone()),
            Src::Capture(_) => None,
        };
        for i in need {
            let path = dests[i].path(&t.rel);
            let d = &mut dests[i];
            let r = match &d.to {
                To::Ec(api) => {
                    if !now.on("backup_ec") || reset_mark() != mark {
                        d.alive = false;
                        continue;
                    }
                    if bytes.len() > super::api::MAX_BYTES {
                        Ok(()) // over 25 MB: Echo Cloud refuses it - remembered, so it isn't tried every round
                    } else {
                        set_status("ec", &format!("Backing up... {} so far", pics(d.done.len())));
                        api.backup_url(&path, bytes.len()).and_then(|u| api.put(&u, bytes.clone())).map_err(|e| e.text())
                    }
                }
                To::Own(rc, p) => match &file {
                    Some(f) => {
                        set_status(&d.key, &format!("Backing up... {} so far", pics(d.done.len())));
                        rc.put(p, f, &path)
                    }
                    None => Err("couldn't write the picture for the upload".into()),
                },
            };
            match r {
                Ok(()) => {
                    d.done.insert(path, t.stamp.clone());
                    d.sent += 1;
                    save(&d.key, &d.done);
                }
                Err(e) => {
                    crate::log::line("backup", &format!("{} stopped: {e}", d.key));
                    d.alive = false;
                    set_status(&d.key, &format!("Paused: {e}"));
                }
            }
        }
        std::thread::sleep(Duration::from_millis(200));
    }
    let _ = std::fs::remove_file(&tmp);
    for d in dests.iter().filter(|d| d.alive) {
        crate::log::line("backup", &format!("{}: {} pictures backed up this round", d.key, d.sent));
        if d.sent == 0 {
            set_status(&d.key, &format!("Everything is backed up ({})", pics(d.done.len())));
        } else {
            set_status(&d.key, &format!("{} backed up · last {}", pics(d.done.len()), hhmm()));
        }
    }
}

/// Start the worker (the always-running Lasso, once)
pub fn start() {
    if KICK.get().is_some() {
        return;
    }
    let (tx, rx) = channel::<()>();
    let _ = KICK.set(Mutex::new(tx));
    std::thread::Builder::new()
        .name("lasso-backup".into())
        .spawn(move || {
            let mut wait = Duration::from_secs(120);
            loop {
                // a kick (new capture, settings changed) comes in -> a short pause so several kicks make one round
                if rx.recv_timeout(wait).is_ok() {
                    std::thread::sleep(Duration::from_secs(20));
                    while rx.try_recv().is_ok() {}
                }
                round();
                wait = Duration::from_secs(600);
            }
        })
        .ok();
}

/// Look again soon (a new capture was stored, or the switches changed)
pub fn kick() {
    if let Some(k) = KICK.get() {
        if let Ok(tx) = k.lock() {
            let _ = tx.send(());
        }
    }
}

/// UI (blocking - a worker thread): delete everything Lasso backed up to Echo Cloud and forget the list. The caller
/// switches `backup_ec` off first (else the next round would put it all back).
pub fn delete_all(cfg: &Config) -> Result<usize, String> {
    let http = Net;
    let api = account_api(&http, cfg).map_err(|e| e.text())?;
    let _ = std::fs::write(reset_path(), crate::util::unix_now().to_string() + &format!("-{}", std::process::id()));
    // a running round may still have saved one more: wait for its upload to end, then read the list
    std::thread::sleep(Duration::from_secs(1));
    let done = load("ec");
    let paths: Vec<String> = done.keys().cloned().collect();
    for chunk in paths.chunks(500) {
        api.delete(chunk).map_err(|e| e.text())?;
    }
    save("ec", &HashMap::new());
    set_status("ec", "");
    Ok(paths.len())
}

#[cfg(test)]
mod tests {
    #[test]
    fn parts_are_safe() {
        assert_eq!(super::part("Counter-Strike 2"), "Counter-Strike 2");
        assert_eq!(super::part("a/b\\c:d"), "a-b-c-d");
        assert_eq!(super::part("..hidden"), "hidden");
        assert_eq!(super::part(""), "picture");
    }
}
