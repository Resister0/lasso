//! Auto-update (Aimdot's method, settings/src/update.rs + core/src/update.rs, for Lasso's one binary).
//!
//! - Ask `https://sonela.app/lasso/update/<windows|linux>/<x86_64|aarch64>/<version>` (User-Agent "Lasso/<version>"):
//!   204 = up to date, 200 = `{version, notes, pub_date, url, signature}` (Tauri's updater format). The answer goes into
//!   `update.ini` next to config.ini (the running Lasso and the Settings window both read it).
//! - The running Lasso checks in a background thread: a first look a minute after start, then every 4 h (it sleeps in
//!   between - nothing wakes up while idle except this thread every few hours); a failed check is tried again after
//!   15 min. A known update puts "Update to Lasso X" in the tray menu; a click opens Settings > About, which installs it.
//! - Install (Settings window): download `url`, check its minisign signature (`signature` = base64 of the .minisig
//!   text, Tauri signer format) against Lasso's own updater key built in at compile time. Nothing that doesn't verify
//!   is ever installed. Windows installed copy (uninstall.exe next to lasso.exe): `Lasso-Setup.exe /P /R /UPDATE`
//!   (Tauri's passive update: progress only, the installer stops Lasso and starts it again). Linux AppImage
//!   (`$APPIMAGE`): the new file next to it as `<AppImage>.new`, renamed over it, the new one started, the running
//!   Lasso told to quit. Anything else (tar.gz, own build, no key): the button opens sonela.app/lasso.
//! - Only published builds check (CI sets LASSO_RELEASE=1), never a test copy (LASSO_HOME) and never in Flatpak
//!   (Flathub updates it).

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

const UPDATE_BASE: &str = "https://sonela.app/lasso/update";
/// Where people download Lasso by hand (tar.gz, or when an update can't install itself)
const DOWNLOAD_PAGE: &str = "https://sonela.app/lasso";
/// How often the running Lasso checks
const CHECK_EVERY_SECS: u64 = 4 * 3600;
/// A failed check is tried again this soon
const RETRY_SECS: u64 = 15 * 60;

/// Lasso's updater public key = base64 of the minisign `.pub` file (Tauri signer format). Set at build time from the
/// repo variable LASSO_UPDATE_PUBKEY (CI). Public, not a secret. Empty = updates are shown, but the button opens the
/// download page instead of installing.
fn pubkey() -> &'static str {
    option_env!("LASSO_UPDATE_PUBKEY").map(str::trim).unwrap_or("")
}

/// The tray's "Update to Lasso X" started this Settings window: install once it is up
const INSTALL_ENV: &str = "LASSO_INSTALL_UPDATE";
/// The new AppImage started by an update: wait until the old Lasso has quit before starting
const REPLACE_ENV: &str = "LASSO_REPLACE";

const VERSION: &str = env!("CARGO_PKG_VERSION");

fn log(msg: &str) {
    crate::log::line("update", msg);
}

fn in_flatpak() -> bool {
    std::env::var_os("FLATPAK_ID").is_some() || Path::new("/.flatpak-info").exists()
}

/// This copy checks at all: published build, not a test copy, not Flatpak
fn checks() -> bool {
    crate::ping::release_build() && std::env::var_os("LASSO_HOME").is_none() && !in_flatpak()
}

fn os() -> &'static str {
    if cfg!(windows) {
        "windows"
    } else {
        "linux"
    }
}

fn arch() -> &'static str {
    if cfg!(target_arch = "aarch64") {
        "aarch64"
    } else {
        "x86_64"
    }
}

fn check_url_for(os: &str, arch: &str, version: &str) -> String {
    format!("{UPDATE_BASE}/{os}/{arch}/{version}")
}

/// + `?ch=beta|dev` on those update channels (stable = no parameter = the old request)
fn with_channel(url: String, channel: &str) -> String {
    if channel == "stable" {
        url
    } else {
        format!("{url}?ch={channel}")
    }
}

// ----- update channels (21 Nov 2026, Echo's / Aimdot's method): Settings > About > Updates > Update channel -----
// stable = the normal releases; beta = new versions a little earlier (sonela.app offers the newest of stable + beta);
// dev = testers, needs the code from the developer (header x-sonela-code; a wrong code = stable). Kept in channel.ini
// next to config.ini: `channel=<stable|beta|dev>`, `code=<dev code>`. No file = stable.

fn channel_path() -> PathBuf {
    crate::paths::config_dir().join("channel.ini")
}

fn parse_channel(text: &str) -> (String, String) {
    let get = |k: &str| text.lines().find_map(|l| l.trim().strip_prefix(&format!("{k}="))).unwrap_or("").trim().to_string();
    let ch = match get("channel").as_str() {
        "beta" => "beta",
        "dev" => "dev",
        _ => "stable",
    };
    let code = if ch == "dev" { get("code") } else { String::new() };
    (ch.to_string(), code)
}

/// ("stable" | "beta" | "dev", the dev code - "" unless dev)
pub fn channel() -> (String, String) {
    parse_channel(&std::fs::read_to_string(channel_path()).unwrap_or_default())
}

/// Saves the channel. A change forgets the known update (it was the old channel's answer) so the next check decides.
fn set_channel(ch: &str, code: &str) -> std::io::Result<()> {
    let (ch, code) = parse_channel(&format!("channel={ch}\ncode={}", code.trim().replace(['\n', '\r'], "")));
    if channel() != (ch.clone(), code.clone()) {
        let _ = std::fs::remove_file(state_path());
    }
    std::fs::create_dir_all(crate::paths::config_dir())?;
    std::fs::write(channel_path(), format!("channel={ch}\ncode={code}\n"))
}

/// a > b for "x.y.z" versions (anything after - or + is ignored)
fn ver_gt(a: &str, b: &str) -> bool {
    let p = |s: &str| -> Vec<u64> { s.trim().trim_start_matches('v').split(['.', '-', '+']).take(3).map(|x| x.parse().unwrap_or(0)).collect() };
    let (x, y) = (p(a), p(b));
    for i in 0..3 {
        let (u, w) = (x.get(i).copied().unwrap_or(0), y.get(i).copied().unwrap_or(0));
        if u != w {
            return u > w;
        }
    }
    false
}

fn now() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

// ----- update.ini -----

/// What the last check found. `version` empty = up to date.
#[derive(Clone, Debug, Default, PartialEq)]
struct State {
    version: String,
    notes: String,
    url: String,
    signature: String,
    /// unix seconds of the last answer (or failure)
    checked: u64,
    /// last failure ("" = the last check worked)
    error: String,
}

impl State {
    /// A newer version than this copy is known
    fn newer(&self) -> bool {
        !self.version.is_empty() && ver_gt(&self.version, VERSION)
    }
}

fn state_path() -> PathBuf {
    crate::paths::config_dir().join("update.ini")
}

/// Values on one line: \ and newlines escaped
fn esc(s: &str) -> String {
    s.replace('\\', "\\\\").replace('\n', "\\n").replace('\r', "")
}

fn unesc(s: &str) -> String {
    let mut o = String::new();
    let mut it = s.chars();
    while let Some(c) = it.next() {
        if c == '\\' {
            match it.next() {
                Some('n') => o.push('\n'),
                Some(x) => o.push(x),
                None => {}
            }
        } else {
            o.push(c);
        }
    }
    o
}

fn read() -> State {
    let mut s = State::default();
    let Ok(t) = std::fs::read_to_string(state_path()) else { return s };
    for l in t.lines() {
        let Some((k, v)) = l.split_once('=') else { continue };
        let v = unesc(v);
        match k.trim() {
            "version" => s.version = v,
            "notes" => s.notes = v,
            "url" => s.url = v,
            "signature" => s.signature = v,
            "checked" => s.checked = v.trim().parse().unwrap_or(0),
            "error" => s.error = v,
            _ => {}
        }
    }
    s
}

/// Temp file + rename (the other process may read it at the same moment)
fn write(s: &State) {
    let p = state_path();
    let text = format!(
        "# Lasso update check (safe to delete)\nversion={}\nnotes={}\nurl={}\nsignature={}\nchecked={}\nerror={}\n",
        esc(&s.version),
        esc(&s.notes),
        esc(&s.url),
        esc(&s.signature),
        s.checked,
        esc(&s.error)
    );
    let tmp = p.with_extension(format!("tmp{}", std::process::id()));
    if std::fs::write(&tmp, text).is_err() || std::fs::rename(&tmp, &p).is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
}

/// Seconds until the next check is due (0 = now): `every` after a good check, 15 min after a failed one
fn wait_secs(s: &State, every: u64, now: u64) -> u64 {
    let wait = if s.error.is_empty() { every } else { every.min(RETRY_SECS) };
    wait.saturating_sub(now.saturating_sub(s.checked))
}

/// The tray menu item, e.g. "Update to Lasso 0.2.0" - None when nothing newer is known
pub fn tray_label() -> Option<String> {
    if !checks() {
        return None;
    }
    let s = read();
    s.newer().then(|| format!("Update to Lasso {}", s.version))
}

// ----- the network -----

/// GET -> (status, body). reqwest blocking + rustls/ring like ping.rs (rustls without a default crypto provider: ring
/// is installed once). Redirects are followed (GitHub downloads redirect).
fn http_get(url: &str) -> Result<(u16, Vec<u8>), String> {
    http(url, &[], None)
}

/// GET (or POST with a JSON body) with extra headers
fn http(url: &str, headers: &[(&str, String)], post_json: Option<String>) -> Result<(u16, Vec<u8>), String> {
    if rustls::crypto::CryptoProvider::get_default().is_none() {
        let _ = rustls::crypto::ring::default_provider().install_default();
    }
    let c = reqwest::blocking::Client::builder()
        .user_agent(concat!("Lasso/", env!("CARGO_PKG_VERSION")))
        .connect_timeout(Duration::from_secs(15))
        .timeout(Duration::from_secs(300))
        .build()
        .map_err(|e| e.to_string())?;
    let mut req = match post_json {
        Some(b) => c.post(url).header("content-type", "application/json").body(b),
        None => c.get(url),
    };
    for (k, v) in headers {
        req = req.header(*k, v.as_str());
    }
    let mut r = req.send().map_err(|e| format!("no connection ({e})"))?;
    let code = r.status().as_u16();
    let mut body = Vec::new();
    std::io::copy(&mut r, &mut body).map_err(|e| format!("download failed ({e})"))?;
    Ok((code, body))
}

/// The server's answer as a State (version "" = up to date). Pure, for the tests.
fn parse_answer(code: u16, body: &str, current: &str, now: u64) -> Result<State, String> {
    let mut s = State { checked: now, ..Default::default() };
    if code == 204 || body.trim().is_empty() {
        return Ok(s);
    }
    if code != 200 {
        return Err(format!("the update server answered {code}"));
    }
    let j: serde_json::Value = serde_json::from_str(body).map_err(|_| "the update server's answer didn't make sense".to_string())?;
    let f = |k: &str| j.get(k).and_then(|v| v.as_str()).unwrap_or("").to_string();
    let v = f("version");
    if !ver_gt(&v, current) {
        return Ok(s);
    }
    s.version = v;
    s.notes = f("notes");
    s.url = f("url");
    s.signature = f("signature");
    Ok(s)
}

/// One check, written to update.ini (a failure keeps the last known update and notes the error)
fn check_and_save() -> Result<State, String> {
    // dev sends the tester code (sonela.app answers stable for a wrong one)
    let (ch, dev_code) = channel();
    let headers = if ch == "dev" { vec![("x-sonela-code", dev_code)] } else { vec![] };
    let r = http(&with_channel(check_url_for(os(), arch(), VERSION), &ch), &headers, None).and_then(|(code, body)| parse_answer(code, &String::from_utf8_lossy(&body), VERSION, now()));
    let s = match &r {
        Ok(s) => s.clone(),
        Err(e) => State { error: e.clone(), checked: now(), ..read() },
    };
    write(&s);
    match &r {
        Ok(s) if s.version.is_empty() => log(&format!("up to date ({VERSION})")),
        Ok(s) => log(&format!("{} available (this is {VERSION})", s.version)),
        Err(e) => log(&format!("check failed: {e}")),
    }
    r
}

// ----- the running Lasso (tray) -----

/// Starts the check thread of the running Lasso. `changed` gets the tray label (on this thread at once, then on the
/// check thread after every check). Does nothing for own builds, test copies and Flatpak.
pub fn start(changed: impl Fn(Option<String>) + Send + 'static) {
    if !checks() {
        return;
    }
    changed(tray_label());
    let _ = std::thread::Builder::new().name("update".into()).stack_size(256 * 1024).spawn(move || {
        std::thread::sleep(Duration::from_secs(60));
        loop {
            if wait_secs(&read(), CHECK_EVERY_SECS, now()) == 0 {
                let _ = check_and_save();
            }
            changed(tray_label());
            // asleep until the next one is due (the Settings window may have checked meanwhile: read again)
            let wait = wait_secs(&read(), CHECK_EVERY_SECS, now()).clamp(60, CHECK_EVERY_SECS);
            std::thread::sleep(Duration::from_secs(wait));
        }
    });
}

/// Settings > About opens with "install now" (the tray's "Update to Lasso X"); `open` starts the Settings window
pub fn open_settings_and_install(open: impl FnOnce()) {
    std::env::set_var(INSTALL_ENV, "1");
    open();
    std::env::remove_var(INSTALL_ENV);
}

/// First thing in main() of a new AppImage started by an update: wait (up to 20 s) until the old Lasso has quit, so
/// this start becomes the running one instead of just telling the old one
pub fn wait_for_old() {
    if std::env::var_os(REPLACE_ENV).is_none() {
        return;
    }
    std::env::remove_var(REPLACE_ENV);
    for _ in 0..100 {
        if crate::ipc::send("ping").is_err() {
            // its socket is gone; the process ends right after (its lock goes with it)
            std::thread::sleep(Duration::from_millis(1000));
            return;
        }
        std::thread::sleep(Duration::from_millis(200));
    }
}

// ----- installing (Settings window) -----

/// How this copy can update itself
#[derive(Debug, PartialEq)]
enum Kind {
    /// Windows, installed by Lasso-Setup.exe
    Installer,
    /// Linux AppImage (the file to replace)
    AppImage(PathBuf),
    /// tar.gz, own folder: the download page
    Manual,
}

fn kind() -> Kind {
    if cfg!(windows) {
        let installed = std::env::current_exe().ok().and_then(|p| p.parent().map(|d| d.join("uninstall.exe").exists())).unwrap_or(false);
        return if installed { Kind::Installer } else { Kind::Manual };
    }
    match std::env::var_os("APPIMAGE").filter(|v| !v.is_empty()) {
        Some(a) => Kind::AppImage(PathBuf::from(a)),
        None => Kind::Manual,
    }
}

/// It can install itself: a copy that can replace itself, a key built in, and a signed download
fn can_install_with(kind: &Kind, key: &str, s: &State) -> bool {
    *kind != Kind::Manual && !key.is_empty() && !s.url.is_empty() && !s.signature.is_empty()
}

fn can_install(s: &State) -> bool {
    can_install_with(&kind(), pubkey(), s)
}

/// minisign check of the downloaded file against an updater key (both base64 of the minisign text, Tauri format)
fn verify_with(key_b64: &str, data: &[u8], sig_b64: &str) -> Result<(), String> {
    use base64::Engine;
    let b64 = base64::engine::general_purpose::STANDARD;
    let text = |b: &str| -> Result<String, String> { String::from_utf8(b64.decode(b.trim()).map_err(|e| e.to_string())?).map_err(|e| e.to_string()) };
    if key_b64.trim().is_empty() {
        return Err("no updater key in this build".into());
    }
    let pk = minisign_verify::PublicKey::decode(&text(key_b64)?).map_err(|e| e.to_string())?;
    let sig = minisign_verify::Signature::decode(&text(sig_b64)?).map_err(|e| e.to_string())?;
    pk.verify(data, &sig, false).map_err(|e| e.to_string())
}

fn open_url(url: &str) {
    let opener = if cfg!(windows) { "explorer" } else { "xdg-open" };
    let _ = std::process::Command::new(opener).arg(url).spawn();
}

/// What Settings > About > Updates shows
#[derive(Clone, Debug, Default)]
pub struct View {
    /// a newer version is known
    pub available: bool,
    pub version: String,
    pub notes: String,
    pub status: String,
    /// "Update now" (installs itself) / "Download" (opens sonela.app/lasso) / "Updating…"
    pub button: String,
    pub busy: bool,
    /// "Check for updates" makes sense (published build, not Flatpak, not a test copy)
    pub can_check: bool,
    /// update channel: 0 stable, 1 beta, 2 dev
    pub channel: i32,
}

pub type Sink = Arc<dyn Fn(View) + Send + Sync>;

pub struct Updater {
    sink: Sink,
    busy: AtomicBool,
    status: Mutex<String>,
}

impl Updater {
    fn view(&self) -> View {
        let s = read();
        let busy = self.busy.load(Ordering::Relaxed);
        let available = checks() && s.newer();
        let mut status = self.status.lock().map(|g| g.clone()).unwrap_or_default();
        if status.is_empty() {
            status = if std::env::var_os("LASSO_HOME").is_some() {
                "Test copies don't check for updates.".into()
            } else if !crate::ping::release_build() {
                "This copy of Lasso was built by hand and doesn't update itself. Get Lasso from sonela.app/lasso for updates.".into()
            } else if in_flatpak() {
                "Flatpak keeps Lasso up to date.".into()
            } else if available {
                format!("Lasso {} is out (you have {VERSION}).", s.version)
            } else if !s.error.is_empty() {
                format!("Couldn't check for updates: {}", s.error)
            } else if s.checked > 0 {
                format!("You have the newest version ({VERSION}).")
            } else {
                String::new()
            };
        }
        let button = if busy {
            "Updating…"
        } else if can_install(&s) {
            "Update now"
        } else {
            "Download"
        };
        let channel = match channel().0.as_str() {
            "beta" => 1,
            "dev" => 2,
            _ => 0,
        };
        View { available, version: s.version.clone(), notes: s.notes.clone(), status, button: button.into(), busy, can_check: checks(), channel }
    }

    fn show(&self) {
        (self.sink)(self.view());
    }

    fn set_status(&self, s: impl Into<String>) {
        if let Ok(mut g) = self.status.lock() {
            *g = s.into();
        }
        self.show();
    }

    /// Check now (by hand: shows "Checking…" and the result). `then` runs on the check thread afterwards.
    pub fn check(self: &Arc<Self>, by_hand: bool, then: impl FnOnce(&Arc<Updater>) + Send + 'static) {
        if !checks() || self.busy.load(Ordering::Relaxed) {
            self.show();
            return;
        }
        if by_hand {
            self.set_status("Checking…");
        }
        let u = self.clone();
        std::thread::spawn(move || {
            let r = check_and_save();
            // the running Lasso's tray shows / drops "Update to Lasso X" now, not at its next check
            let _ = crate::ipc::send("update-known");
            match r {
                Ok(s) if s.version.is_empty() && by_hand => u.set_status(format!("You have the newest version ({VERSION}).")),
                Err(e) if by_hand => u.set_status(format!("Couldn't check for updates: {e}")),
                _ => u.set_status(""),
            }
            then(&u);
        });
    }

    /// Update channel picked: 0 stable, 1 beta (saved at once, then a check); dev goes through `dev_code`
    pub fn set_channel(self: &Arc<Self>, pick: i32) {
        let ch = match pick {
            1 => "beta",
            0 => "stable",
            _ => return self.show(),
        };
        if let Err(e) = set_channel(ch, "") {
            return self.set_status(format!("Couldn't save the update channel: {e}"));
        }
        log(&format!("update channel: {ch}"));
        self.check(true, |_| {});
    }

    /// The dev code typed in: sonela.app checks it, then dev is saved (a wrong code changes nothing). `done` gets
    /// "" = saved, else what to show under the code box (on the check thread).
    pub fn dev_code(self: &Arc<Self>, code: String, done: impl FnOnce(String) + Send + 'static) {
        let code = code.trim().to_string();
        if code.is_empty() {
            return done(String::new());
        }
        let u = self.clone();
        std::thread::spawn(move || {
            let body = serde_json::json!({ "code": code }).to_string();
            let r = http("https://sonela.app/lasso/channel/check", &[], Some(body)).and_then(|(c, b)| match c {
                200 => Ok(serde_json::from_slice::<serde_json::Value>(&b).ok().and_then(|j| j["ok"].as_bool()) == Some(true)),
                429 => Err("too many tries - wait an hour".to_string()),
                c => Err(format!("the server answered {c}")),
            });
            let msg = match r {
                Ok(true) => match set_channel("dev", &code) {
                    Ok(()) => {
                        log("update channel: dev");
                        u.check(true, |_| {});
                        String::new()
                    }
                    Err(e) => format!("Couldn't save the update channel: {e}"),
                },
                Ok(false) => "That code isn't right - ask the developer for it again.".into(),
                Err(e) => format!("Couldn't check the code: {e}"),
            };
            done(msg);
        });
    }

    /// The "Update now" / "Download" button
    pub fn install(self: &Arc<Self>) {
        let s = read();
        if !s.newer() {
            self.check(true, |_| {});
            return;
        }
        if !can_install(&s) {
            open_url(DOWNLOAD_PAGE);
            self.set_status(format!("Opened sonela.app/lasso - download Lasso {} there.", s.version));
            return;
        }
        if self.busy.swap(true, Ordering::Relaxed) {
            return;
        }
        self.set_status(format!("Downloading Lasso {}…", s.version));
        let u = self.clone();
        std::thread::spawn(move || {
            if let Err(e) = download_and_install(&s, &u) {
                log(&format!("install failed: {e}"));
                u.busy.store(false, Ordering::Relaxed);
                u.set_status(format!("The update couldn't be installed: {e}. You can download Lasso from sonela.app/lasso."));
            }
        });
    }
}

/// Download, verify, install, restart. Returns only on failure (success ends this process).
fn download_and_install(s: &State, u: &Updater) -> Result<(), String> {
    let (code, data) = http_get(&s.url)?;
    if code != 200 {
        return Err(format!("download failed (the server answered {code})"));
    }
    if let Err(e) = verify_with(pubkey(), &data, &s.signature) {
        log(&format!("signature check failed: {e}"));
        return Err("its signature didn't match, so it was not installed".into());
    }
    log(&format!("{} downloaded + verified ({} bytes)", s.version, data.len()));
    u.set_status(format!("Installing Lasso {}…", s.version));
    match kind() {
        Kind::Installer => {
            let f = std::env::temp_dir().join(format!("Lasso-Setup-{}.exe", s.version));
            std::fs::write(&f, &data).map_err(|e| e.to_string())?;
            // Tauri's passive update: progress only, /R starts lasso.exe again when done; the installer quits the
            // running Lasso and this window first (packaging/installer/hooks.nsh)
            std::process::Command::new(&f).args(["/P", "/R", "/UPDATE"]).spawn().map_err(|e| format!("the installer couldn't start ({e})"))?;
            log("installer started - exiting");
            std::process::exit(0);
        }
        Kind::AppImage(target) => {
            let mut tmp = target.clone().into_os_string();
            tmp.push(".new");
            let tmp = PathBuf::from(tmp);
            std::fs::write(&tmp, &data).map_err(|e| format!("couldn't write next to {} ({e})", target.display()))?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let _ = std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o755));
            }
            std::fs::rename(&tmp, &target).map_err(|e| {
                let _ = std::fs::remove_file(&tmp);
                format!("couldn't replace {} ({e})", target.display())
            })?;
            // start the new AppImage first (it waits until the old Lasso is gone, see wait_for_old), then tell the
            // running Lasso to quit - quitting also closes this window. LASSO_COPY etc. come from the old copy: the
            // new one must make its own (desktop.rs appimage_reexec).
            std::process::Command::new(&target)
                .env(REPLACE_ENV, "1")
                .env_remove("LASSO_COPY")
                .env_remove("LASSO_PAGE")
                .env_remove(INSTALL_ENV)
                .spawn()
                .map_err(|e| format!("couldn't start the new version ({e})"))?;
            let _ = crate::ipc::send("quit");
            log("new AppImage started - exiting");
            std::process::exit(0);
        }
        Kind::Manual => Err("this copy can't replace itself".into()),
    }
}

/// Settings window: shows what update.ini knows, checks when the last check is over 10 minutes old, and installs at
/// once when the tray's "Update to Lasso X" opened it
pub fn start_window(sink: Sink) -> Arc<Updater> {
    let u = Arc::new(Updater { sink, busy: AtomicBool::new(false), status: Mutex::new(String::new()) });
    let install = std::env::var_os(INSTALL_ENV).is_some();
    std::env::remove_var(INSTALL_ENV);
    u.show();
    if !checks() {
        return u;
    }
    if wait_secs(&read(), 10 * 60, now()) == 0 {
        u.check(false, move |u| {
            if install {
                u.install();
            }
        });
    } else if install {
        u.install();
    }
    u
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions() {
        assert!(ver_gt("0.1.10", "0.1.9"));
        assert!(ver_gt("1.0.0", "0.9.99"));
        assert!(!ver_gt("0.1.3", "0.1.3"));
        assert!(!ver_gt("0.1.2", "0.1.3"));
        assert!(ver_gt("v0.2.0", "0.1.3-dev"));
        assert!(!ver_gt("", "0.1.0"));
    }

    #[test]
    fn channels() {
        assert_eq!(parse_channel(""), ("stable".into(), "".into()));
        assert_eq!(parse_channel("channel=beta\ncode=abc\n"), ("beta".into(), "".into()));
        assert_eq!(parse_channel("channel=dev\ncode= abc123 \n"), ("dev".into(), "abc123".into()));
        assert_eq!(parse_channel("channel=nightly\n"), ("stable".into(), "".into()));
        let u = check_url_for("linux", "x86_64", "0.1.5");
        assert_eq!(with_channel(u.clone(), "stable"), "https://sonela.app/lasso/update/linux/x86_64/0.1.5");
        assert_eq!(with_channel(u.clone(), "beta"), "https://sonela.app/lasso/update/linux/x86_64/0.1.5?ch=beta");
        assert_eq!(with_channel(u, "dev"), "https://sonela.app/lasso/update/linux/x86_64/0.1.5?ch=dev");
    }

    #[test]
    fn check_url() {
        assert_eq!(check_url_for("linux", "x86_64", "0.1.0"), "https://sonela.app/lasso/update/linux/x86_64/0.1.0");
        assert_eq!(check_url_for(os(), arch(), VERSION), format!("https://sonela.app/lasso/update/{}/{}/{VERSION}", os(), arch()));
    }

    #[test]
    fn server_answers() {
        // 204 / empty = up to date
        assert_eq!(parse_answer(204, "", "0.1.0", 5).unwrap(), State { checked: 5, ..Default::default() });
        // a newer version: every field taken
        let b = r#"{"version":"0.2.0","notes":"Line one\nsaid \"hi\"","pub_date":"2026-11-20T20:00:00Z","url":"https://example.org/Lasso-x86_64.AppImage","signature":"c2ln"}"#;
        let s = parse_answer(200, b, "0.1.0", 5).unwrap();
        assert_eq!((s.version.as_str(), s.notes.as_str(), s.signature.as_str()), ("0.2.0", "Line one\nsaid \"hi\"", "c2ln"));
        assert!(s.url.ends_with("Lasso-x86_64.AppImage"));
        // the same or an older version = nothing to do
        assert!(parse_answer(200, b, "0.2.0", 5).unwrap().version.is_empty());
        // errors
        assert!(parse_answer(500, "oops", "0.1.0", 5).is_err());
        assert!(parse_answer(200, "not json", "0.1.0", 5).is_err());
    }

    #[test]
    fn check_timing() {
        let ok = State { checked: 1000, ..Default::default() };
        assert_eq!(wait_secs(&ok, CHECK_EVERY_SECS, 1000), CHECK_EVERY_SECS);
        assert_eq!(wait_secs(&ok, CHECK_EVERY_SECS, 1000 + CHECK_EVERY_SECS + 1), 0);
        let failed = State { checked: 1000, error: "no connection".into(), ..Default::default() };
        assert_eq!(wait_secs(&failed, CHECK_EVERY_SECS, 1000 + 60), RETRY_SECS - 60);
        // never checked = now
        assert_eq!(wait_secs(&State::default(), CHECK_EVERY_SECS, 1_800_000_000), 0);
    }

    #[test]
    fn state_values_stay_on_one_line() {
        let notes = "a\\b\nc=d";
        let line = esc(notes);
        assert!(!line.contains('\n'));
        assert_eq!(unesc(&line), notes);
    }

    #[test]
    fn no_key_means_no_install() {
        let s = State { version: "99.0.0".into(), url: "https://example.org/x".into(), signature: "abc".into(), ..Default::default() };
        let appimage = Kind::AppImage(PathBuf::from("Lasso-x86_64.AppImage"));
        assert!(!can_install_with(&appimage, "", &s));
        assert!(!can_install_with(&Kind::Installer, "", &s));
        // a key, but a copy that can't replace itself (tar.gz / own build) or an unsigned answer
        assert!(!can_install_with(&Kind::Manual, "a2V5", &s));
        assert!(!can_install_with(&appimage, "a2V5", &State { signature: String::new(), ..s.clone() }));
        assert!(can_install_with(&appimage, "a2V5", &s));
        // own builds have no key compiled in: never installs, and verifying fails
        if pubkey().is_empty() {
            assert!(!can_install(&s));
            assert!(verify_with(pubkey(), b"data", "c2ln").is_err());
        }
    }

    #[test]
    fn bad_signature_is_rejected() {
        use base64::Engine;
        let b64 = base64::engine::general_purpose::STANDARD;
        // a real-looking minisign key (Ed25519, all zeros) + a signature that isn't for this data
        let key = b64.encode(format!("untrusted comment: minisign public key\n{}\n", b64.encode([b"Ed".as_slice(), &[0u8; 8], &[0u8; 32]].concat())));
        let sig = b64.encode(format!(
            "untrusted comment: signature\n{}\ntrusted comment: timestamp:0\n{}\n",
            b64.encode([b"ED".as_slice(), &[0u8; 8], &[1u8; 64]].concat()),
            b64.encode([2u8; 64])
        ));
        // both parse - it is the check itself that says no
        let text = |b: &str| String::from_utf8(b64.decode(b).unwrap()).unwrap();
        assert!(minisign_verify::PublicKey::decode(&text(&key)).is_ok());
        assert!(minisign_verify::Signature::decode(&text(&sig)).is_ok());
        assert!(verify_with(&key, b"data", &sig).is_err());
        assert!(verify_with(&key, b"data", "not base64 !!").is_err());
        assert!(verify_with("not base64 !!", b"data", &sig).is_err());
        assert!(verify_with(&key, b"data", "").is_err());
    }
}
