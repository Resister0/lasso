//! Share links (Ctrl+L / Enter = link / the Link button) - `sonela.app/s/<id>` (notes `LASSO_SHARE_PLAN.md`).
//!
//! A SWITCH: config `share_on` (on by default since 21 Nov 2026, the server part is live). No account = a device key
//! (`token_or_device`, links last a year at most); signed in = the Echo Cloud account. Off = nothing
//! here talks to a server. On = before every upload or sign-in Lasso asks `GET /health` whether the server
//! knows Lasso (`"lasso": 1`); an older server = "needs the Sonela server update first", nothing uploaded.
//!
//! Flow (Echo's own upload flow, same account): the Echo Cloud account (email + 6-digit code, no password) ->
//! privacy check (OCR: emails, numbers, keys - warns once, the second press shares anyway) -> encode (Fits Discord)
//! -> `POST /upload` (share_only: the file goes when its link does) -> PUT to the presigned link -> `POST /share`
//! (kind = image) -> the link on the clipboard + "Link copied - expires in 30 days".
//! Sharing a capture again (after editing it) = the SAME link shows the new picture ("Update link", 6c #3).
//!
//! The UI (Settings sign-in, Library link icon / Stop sharing / expiry) calls the functions marked "UI:" - they
//! block, so call them from a worker thread.

mod api;
pub mod backup;
#[cfg(target_os = "linux")]
mod keyring_linux;
mod net;
mod secret;
mod store;
#[cfg(test)]
mod tests;

use crate::config::Config;
use crate::shapes::{Pic, R};
use api::{Api, Fail, Http, LinkReq};
pub use store::Shared;
use store::{Account, Store};
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// The Echo Cloud server (also the API for sonela.app links). `LASSO_SHARE_API` / config `share_api` override it.
pub const API_DEFAULT: &str = "https://echoclip.link";
/// Link expiry choices in days (0 = never); the default comes from config `link_expiry` (30)
pub const EXPIRY_CHOICES: [u32; 5] = [1, 7, 30, 365, 0];
/// A privacy warning is remembered this long: pressing Link again within it shares anyway
const WARN_FOR: Duration = Duration::from_secs(120);

/// Share links switched on (Settings / config.ini `share_on=1`)
pub fn enabled(cfg: &Config) -> bool {
    cfg.on("share_on")
}

/// A config value with a default for keys config.rs doesn't list (yet)
fn opt<'a>(cfg: &'a Config, key: &str, default: &'a str) -> &'a str {
    if cfg.v.contains_key(key) {
        cfg.get(key)
    } else {
        default
    }
}

fn api_base(cfg: &Config) -> String {
    let v = std::env::var("LASSO_SHARE_API").ok().filter(|v| !v.trim().is_empty()).unwrap_or_else(|| opt(cfg, "share_api", "").to_string());
    base_or_default(&v)
}

/// Only https (or a local test server) - the token never goes anywhere else
fn base_or_default(v: &str) -> String {
    let v = v.trim().trim_end_matches('/');
    if v.starts_with("https://") || v.starts_with("http://127.0.0.1:") {
        v.to_string()
    } else {
        API_DEFAULT.to_string()
    }
}

/// The expiry for new links: config `link_expiry` if it's one of the choices, else 30 days
pub fn default_days(cfg: &Config) -> u32 {
    cfg.get("link_expiry").trim().parse::<u32>().ok().filter(|d| EXPIRY_CHOICES.contains(d)).unwrap_or(30)
}

/// "expires in 30 days" / "expires in 24 hours" / "never expires"
pub fn expiry_label(days: u32) -> String {
    match days {
        0 => "never expires".into(),
        1 => "expires in 24 hours".into(),
        365 => "expires in 1 year".into(),
        d => format!("expires in {d} days"),
    }
}

// ---------- privacy check (6c #4) ----------

/// UI: OCR + `ocr::sensitive` - the boxes of emails, numbers, IP addresses and keys (picture pixels).
/// Err = couldn't read the text (no Tesseract) - then there is nothing to warn about.
#[allow(dead_code)] // UI: the overlay's own "Found ... - blur them?" prompt
pub fn privacy_check(pic: &Pic) -> Result<Vec<(R, &'static str)>, String> {
    Ok(crate::ocr::sensitive(&crate::ocr::read(pic)?))
}

/// "Found an email and a key" / "Found 2 emails, a number and an IP address" ("" = nothing)
pub fn privacy_message(hits: &[(R, &str)]) -> String {
    let mut order: Vec<&str> = vec![];
    let mut n: HashMap<&str, usize> = HashMap::new();
    for (_, k) in hits {
        if !order.contains(k) {
            order.push(k);
        }
        *n.entry(k).or_default() += 1;
    }
    let parts: Vec<String> = order
        .iter()
        .map(|k| {
            let c = n[k];
            if c == 1 {
                let a = if k.starts_with(['a', 'e', 'i', 'o', 'u', 'I']) { "an" } else { "a" };
                format!("{a} {k}")
            } else if k.ends_with("ss") {
                format!("{c} {k}es")
            } else {
                format!("{c} {k}s")
            }
        })
        .collect();
    match parts.len() {
        0 => String::new(),
        1 => format!("Found {}", parts[0]),
        l => format!("Found {} and {}", parts[..l - 1].join(", "), parts[l - 1]),
    }
}

/// Captures warned about lately (library id -> when): the next press within WARN_FOR shares anyway
static WARNED: Mutex<Option<HashMap<String, Instant>>> = Mutex::new(None);

/// true = warn now (first press); false = warned a moment ago, share anyway
fn first_warning(id: &str) -> bool {
    let mut g = WARNED.lock().unwrap_or_else(|e| e.into_inner());
    let m = g.get_or_insert_with(HashMap::new);
    m.retain(|_, t| t.elapsed() < WARN_FOR);
    if id.is_empty() || m.remove(id).is_some() {
        return false;
    }
    m.insert(id.to_string(), Instant::now());
    true
}

// ---------- making a link ----------

/// Captures being uploaded right now (one upload per capture at a time)
static BUSY: Mutex<Option<HashSet<String>>> = Mutex::new(None);

/// What one share does (everything the worker thread needs)
struct Job {
    capture: String,
    bytes: Vec<u8>,
    ext: &'static str,
    w: u32,
    h: u32,
    /// None = new link: the default; a link shared before keeps its own expiry
    days: Option<u32>,
    once: bool,
    now: i64,
    year: i32,
    month: u32,
}

/// The whole share, blocking: ready? -> upload link -> PUT -> /share -> remembered. The same capture again
/// reuses its path, so its link shows the new picture. Ok((link, true)) = an existing link was updated.
fn run(http: &dyn Http, base: &str, st: &Store, job: Job, default_days: u32) -> Result<(Shared, bool), Fail> {
    let token = token_or_device(http, base, st)?;
    let api = Api { base: base.to_string(), token, http };
    let res = (|| {
        api.ready()?;
        let mut prev = st.get(&job.capture, job.now);
        let days = job.days.or(prev.as_ref().map(|p| p.days)).unwrap_or(default_days);
        let once = job.once || prev.as_ref().is_some_and(|p| p.once);
        // a view-once link is never filled with a new picture (it may be opened already - the PUT would come before
        // the server could say so; review D2): a new link, and the old one is stopped (one live link per capture)
        let old_once = prev.take_if(|p| p.once);
        if let Some(p) = &old_once {
            if api.remove(&p.path).is_ok() {
                let _ = st.remove(&p.id, job.now);
            }
        }
        let path = prev.as_ref().map(|p| p.path.clone()).unwrap_or_else(|| {
            let name = match (job.capture.is_empty(), old_once.is_some()) {
                (true, _) => format!("shot-{}", job.now),
                (false, false) => job.capture.clone(),
                (false, true) => format!("{}-{}", job.capture, job.now),
            };
            api::key_for(&name, job.ext, job.year, job.month)
        });
        let up = api.upload_url(&path, job.bytes.len())?;
        let bytes = job.bytes;
        api.put(&up, bytes.clone())?;
        let mut path = path;
        let mut link = api.link(&path, &LinkReq { days, once, w: job.w, h: job.h, title: String::new() })?;
        if link.viewed {
            // the view-once link was already opened: that link stays spent, this picture gets a NEW link
            crate::log::line("share", "the view-once link was already opened - making a new one");
            let name = format!("{}-{}", if job.capture.is_empty() { "shot".to_string() } else { job.capture.clone() }, job.now);
            path = api::key_for(&name, job.ext, job.year, job.month);
            let up = api.upload_url(&path, bytes.len())?;
            api.put(&up, bytes)?;
            link = api.link(&path, &LinkReq { days, once, w: job.w, h: job.h, title: String::new() })?;
            prev = None;
        }
        let s = Shared {
            capture: job.capture.clone(),
            url: link.url,
            id: link.id,
            path,
            days,
            once,
            expires: link.expires,
            created: prev.as_ref().map(|p| p.created).unwrap_or(job.now),
            updated: job.now,
            w: job.w,
            h: job.h,
        };
        if let Err(e) = st.put(s.clone(), job.now) {
            crate::log::line("share", &format!("couldn't remember the link: {e}"));
        }
        Ok((s, prev.is_some()))
    })();
    signed_out_check(st, &res);
    res
}

/// The token, or SignedOut when there is no account / it can't be read
fn token_of(st: &Store) -> Result<String, Fail> {
    let a = st.account().ok_or(Fail::SignedOut)?;
    let t = secret::unprotect(&a.token);
    if t.is_empty() {
        return Err(Fail::SignedOut);
    }
    Ok(t)
}

/// The account's token, or (no account - 21 Nov 2026: links without signing in) this PC's device session: the device
/// key is made once (32 random bytes), the server turns it into a session; the session is kept and asked again when
/// the server says it ended. The same key always gets the same hidden device account, so its links stay its own.
fn token_or_device(http: &dyn Http, base: &str, st: &Store) -> Result<String, Fail> {
    if let Ok(t) = token_of(st) {
        return Ok(t);
    }
    let (mut key, token) = st.device();
    if !token.is_empty() {
        return Ok(token);
    }
    if key.len() != 64 {
        key = secret::random_hex32().ok_or_else(|| Fail::Msg("Couldn't make a device key on this PC".into()))?;
        st.set_device(&key, "").map_err(|e| Fail::Msg(format!("Couldn't save the device key: {e}")))?;
    }
    let api = Api { base: base.to_string(), token: String::new(), http };
    api.ready()?;
    let t = api.auth_device(&key)?;
    let _ = st.set_device(&key, &t);
    Ok(t)
}

/// The server said the sign-in ended: forget it here too (signed in: the next press asks to sign in; device: the next
/// press gets a new device session with the same key)
fn signed_out_check<T>(st: &Store, r: &Result<T, Fail>) {
    if matches!(r, Err(Fail::SignedOut)) {
        if let Some(a) = st.account() {
            secret::forget(&a.token);
            st.clear_account();
        } else {
            let (key, _) = st.device();
            let _ = st.set_device(&key, "");
        }
    }
}

/// Ctrl+L / Enter = link / the Link button. Off -> nothing (app.rs shows "aren't switched on yet").
/// On -> privacy check, then the upload, all on a worker thread; the link lands on the clipboard + a notification.
pub fn start(pic: Arc<Pic>, id: String, cfg: &Config) {
    share(pic, id, cfg, true);
}

/// UI: the same without the privacy check (the overlay already asked "Found ... - blur them?" and the user said Share)
#[allow(dead_code)]
pub fn start_checked(pic: Arc<Pic>, id: String, cfg: &Config) {
    share(pic, id, cfg, false);
}

fn share(pic: Arc<Pic>, id: String, cfg: &Config, check: bool) {
    if !enabled(cfg) {
        crate::log::line("share", "share links are off (share_on=0) - nothing uploaded");
        return;
    }
    {
        let mut b = BUSY.lock().unwrap_or_else(|e| e.into_inner());
        if !b.get_or_insert_with(HashSet::new).insert(id.clone()) {
            return;
        }
    }
    let base = api_base(cfg);
    let fmt = match cfg.get("format") {
        "webp" => "webp",
        "jpg" | "jpeg" => "jpg",
        _ => "png",
    };
    let check = check && opt(cfg, "share_check", "1") == "1";
    let both = opt(cfg, "share_copy", "link") == "both";
    let days = default_days(cfg);
    let once = opt(cfg, "share_once", "0") == "1";
    std::thread::spawn(move || {
        let busy_id = id.clone();
        let out = (|| -> Result<(String, Vec<u8>, String), Fail> {
            if check {
                match crate::ocr::read(&pic) {
                    Ok(words) => {
                        let hits = crate::ocr::sensitive(&words);
                        if !hits.is_empty() && first_warning(&id) {
                            let m = privacy_message(&hits);
                            crate::log::line("share", &format!("privacy check: {m} - not shared yet"));
                            return Ok((String::new(), vec![], format!("{m} - not shared yet. Blur it (Ctrl+Shift+R), or press Link again to share anyway.")));
                        }
                    }
                    // no Tesseract: no check (a warning that can never come helps nobody)
                    Err(e) => crate::log::line("share", &format!("privacy check skipped: {e}")),
                }
            }
            let enc = crate::export::encode(&pic, fmt, true, false);
            let size = enc.bytes.len();
            let keep = if both { enc.bytes.clone() } else { vec![] };
            let (y, mo, ..) = crate::util::local_now();
            let job = Job { capture: id.clone(), bytes: enc.bytes, ext: enc.ext, w: pic.w, h: pic.h, days: None, once, now: crate::util::unix_now(), year: y, month: mo };
            let t0 = Instant::now();
            let (s, again) = run(&net::Net, &base, &Store::app(), job, days)?;
            crate::log::line("share", &format!("link {} ({} KB, {} ms){}", short(&s.id), size / 1024, t0.elapsed().as_millis(), if again { ", same link updated" } else { "" }));
            // the server's end, not the wish: "never" without an account is a year (22 Nov 2026)
            let left = if s.expires > 0 { expiry_left(s.expires, crate::util::unix_now()) } else { expiry_label(0) };
            let mut note = if again { format!("Link updated - {left}") } else { format!("Link copied - {left}") };
            if !enc.note.is_empty() {
                note = format!("{note} · {}", enc.note);
            }
            Ok((s.url, keep, note))
        })();
        BUSY.lock().unwrap_or_else(|e| e.into_inner()).get_or_insert_with(HashSet::new).remove(&busy_id);
        // the corner thumbnail said "Uploading for a link..." - it says how it went too (24 Nov 2026)
        let thumb = move |note: String| {
            let _ = slint::invoke_from_event_loop(move || crate::with_app(|a| a.thumb_note_for(&busy_id, &note)));
        };
        match out {
            Ok((url, img, note)) if !url.is_empty() => {
                let n2 = note.clone();
                let _ = slint::invoke_from_event_loop(move || {
                    copy_link(&url, img);
                    crate::notify("Lasso", &note);
                });
                thumb(n2);
            }
            Ok((_, _, warning)) => {
                crate::notify("Lasso", &warning);
                thumb(warning);
            }
            Err(e) => {
                match &e {
                    Fail::Offline(why) => crate::log::line("share", &format!("no answer: {why}")),
                    _ => crate::log::line("share", &format!("not shared: {}", e.text())),
                }
                let m = format!("No link made: {}", e.text());
                crate::notify("Lasso", &m);
                thumb(m);
            }
        }
    });
}

/// An updated link keeps its old end: "expires in N days" counted from now
fn expiry_left(expires: i64, now: i64) -> String {
    if expires == 0 {
        return "never expires".into();
    }
    let left = (expires - now).max(0);
    if left < 86400 {
        "expires within 24 hours".into()
    } else if left > 360 * 86400 {
        "expires in 1 year".into()
    } else {
        format!("expires in {} days", (left + 43200) / 86400)
    }
}

/// The first 3 characters of a link id (the log may go out with a bug report - not the whole link)
fn short(id: &str) -> String {
    format!("{}…", id.chars().take(3).collect::<String>())
}

/// The link on the clipboard; config `share_copy=both` (6c #10, KDE / wlroots): the picture too - chat apps paste
/// the picture, text boxes the link. Main thread.
fn copy_link(url: &str, img: Vec<u8>) {
    #[cfg(target_os = "linux")]
    if !img.is_empty() {
        let mime = if img.starts_with(b"\x89PNG") {
            "image/png"
        } else if img.starts_with(b"RIFF") {
            "image/webp"
        } else {
            "image/jpeg"
        };
        let t = Arc::new(url.as_bytes().to_vec());
        let mut items = vec![(mime.to_string(), Arc::new(img))];
        for m in ["text/plain;charset=utf-8", "text/plain", "UTF8_STRING", "STRING", "TEXT"] {
            items.push((m.to_string(), t.clone()));
        }
        if crate::wl::set_clipboard(items) {
            return;
        }
        crate::clipboard::set_text(url);
        return;
    }
    let _ = img;
    crate::clipboard::set_text(url);
}

// ---------- account (UI: Settings > Share links) ----------

/// UI: the signed-in email (None = signed out). Reads only the file - no keyring, no network.
#[allow(dead_code)]
pub fn signed_in() -> Option<String> {
    Store::app().account().map(|a| a.email)
}

fn sign_in_start_with(http: &dyn Http, base: &str, email: &str) -> Result<String, Fail> {
    let email = api::clean_email(email);
    if email.is_empty() {
        return Err(Fail::Msg("Enter your email".into()));
    }
    let api = Api { base: base.to_string(), token: String::new(), http };
    api.ready()?;
    api.auth_start(&email)?;
    Ok(email)
}

fn sign_in_finish_with(http: &dyn Http, base: &str, st: &Store, email: &str, code: &str) -> Result<(), Fail> {
    let email = api::clean_email(email);
    let code = api::clean_code(code).ok_or_else(|| Fail::Msg("Enter the 6-digit code from the email".into()))?;
    let api = Api { base: base.to_string(), token: String::new(), http };
    let token = api.auth_verify(&email, &code)?;
    st.set_account(&Account { email, token: secret::protect(&token) }).map_err(|e| Fail::Msg(format!("Couldn't save the sign-in: {e}")))
}

fn sign_out_with(http: &dyn Http, base: &str, st: &Store) {
    if let Ok(t) = token_of(st) {
        let _ = Api { base: base.to_string(), token: t, http }.logout();
    }
    if let Some(a) = st.account() {
        secret::forget(&a.token);
    }
    st.clear_account();
    // the links stay listed: they are public until they expire, so the Library must still offer "Stop sharing"
    // (after signing in again) instead of forgetting them
}

/// UI: sign-in step 1 - emails a 6-digit code (the Echo Cloud account: one sign-in for Echo and Lasso).
/// Ok = the cleaned email to show in step 2. Blocking.
#[allow(dead_code)]
pub fn sign_in_start(cfg: &Config, email: &str) -> Result<String, String> {
    if !enabled(cfg) {
        return Err(Fail::NotReady.text());
    }
    sign_in_start_with(&net::Net, &api_base(cfg), email).map_err(|e| e.text())
}

/// UI: sign-in step 2 - the code from the email. Blocking.
#[allow(dead_code)]
pub fn sign_in_finish(cfg: &Config, email: &str, code: &str) -> Result<(), String> {
    if !enabled(cfg) {
        return Err(Fail::NotReady.text());
    }
    sign_in_finish_with(&net::Net, &api_base(cfg), &Store::app(), email, code).map_err(|e| e.text())
}

/// UI: sign out (the links keep working until they expire - they belong to the account). Blocking.
#[allow(dead_code)]
pub fn sign_out(cfg: &Config) {
    sign_out_with(&net::Net, &api_base(cfg), &Store::app());
}

// ---------- links (UI: Library) ----------

/// The share key of a picture from the save folder (not a capture). Never its path: the key becomes part of the
/// picture's name on the server, and a path has the user's folder names in it (24 Nov 2026). A stable hash of the
/// path instead (FNV-1a - std's hasher may change between Rust versions), so the same file keeps its one link.
pub fn file_key(path: &std::path::Path) -> String {
    let s = path.to_string_lossy();
    // Windows paths ignore case: C:\Pics\A.png and c:\pics\a.png are one file
    let s = if cfg!(windows) { s.to_lowercase() } else { s.to_string() };
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in s.bytes() {
        h ^= b as u64;
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    format!("file-{h:016x}")
}

/// UI: the working link of a capture (Library link icon, "Copy link")
#[allow(dead_code)]
pub fn link_of(capture: &str) -> Option<Shared> {
    Store::app().get(capture, crate::util::unix_now())
}

/// UI: every capture with a working link (Library: link icon on these tiles)
#[allow(dead_code)]
pub fn shared_captures() -> HashSet<String> {
    Store::app().links(crate::util::unix_now()).into_iter().map(|l| l.capture).filter(|c| !c.is_empty()).collect()
}

fn stop_with(http: &dyn Http, base: &str, st: &Store, capture: &str, now: i64) -> Result<(), Fail> {
    let Some(s) = st.get(capture, now) else { return Ok(()) };
    let api = Api { base: base.to_string(), token: token_or_device(http, base, st)?, http };
    let r = api.remove(&s.path);
    signed_out_check(st, &r);
    r?;
    st.remove(&s.id, now).map_err(Fail::Msg)
}

fn expiry_with(http: &dyn Http, base: &str, st: &Store, capture: &str, days: u32, now: i64) -> Result<Shared, Fail> {
    let s = st.get(capture, now).ok_or_else(|| Fail::Msg("This capture has no link".into()))?;
    let api = Api { base: base.to_string(), token: token_or_device(http, base, st)?, http };
    let r = api.link(&s.path, &LinkReq { days, once: s.once, w: s.w, h: s.h, title: String::new() });
    signed_out_check(st, &r);
    let l = r?;
    let n = Shared { url: l.url, id: l.id, expires: l.expires, days, updated: now, ..s };
    st.put(n.clone(), now).map_err(Fail::Msg)?;
    Ok(n)
}

/// UI: "Stop sharing" - the link dies now and the picture is deleted from the account. Blocking.
#[allow(dead_code)]
pub fn stop(cfg: &Config, capture: &str) -> Result<(), String> {
    let r = stop_with(&net::Net, &api_base(cfg), &Store::app(), capture, crate::util::unix_now()).map_err(|e| e.text());
    crate::log::line("share", &format!("stop sharing: {}", if r.is_ok() { "ok" } else { "failed" }));
    r
}

/// UI: a new expiry for a capture's link (counted from now; one of EXPIRY_CHOICES). Blocking.
#[allow(dead_code)]
pub fn set_expiry(cfg: &Config, capture: &str, days: u32) -> Result<Shared, String> {
    if !EXPIRY_CHOICES.contains(&days) {
        return Err("Pick 1, 7 or 30 days, or never".into());
    }
    expiry_with(&net::Net, &api_base(cfg), &Store::app(), capture, days, crate::util::unix_now()).map_err(|e| e.text())
}
