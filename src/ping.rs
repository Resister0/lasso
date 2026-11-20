//! The daily count (20 Nov 2026: "very basic tracking - daily downloads, installs and users per day"). At most ONE
//! request a day: `GET https://sonela.app/lasso/ping?v=<version>&os=<windows|linux>&new=<0|1>` (User-Agent
//! "Lasso/<version>"). sonela.app adds one to that day's users (and installs when new=1) - no ID, nothing else is sent
//! or kept. Only published builds (CI sets LASSO_RELEASE=1) send it, never a test copy (LASSO_HOME), and never when
//! Settings > About > "Count me in the daily numbers" is off (`usage_ping=0`).
//! The day it last went out lives in `usage.ini` next to config.ini (`last=YYYY-MM-DD`, `installed=1`).

use std::time::Duration;

const URL: &str = "https://sonela.app/lasso/ping";

/// A published build (CI)? Own builds never send anything.
pub fn release_build() -> bool {
    option_env!("LASSO_RELEASE") == Some("1")
}

fn state_path() -> std::path::PathBuf {
    crate::paths::config_dir().join("usage.ini")
}

fn today() -> String {
    let (y, m, d, ..) = crate::util::local_now();
    format!("{y:04}-{m:02}-{d:02}")
}

/// (last day sent, a ping already went out once = not a new install)
fn read_state() -> (String, bool) {
    let s = std::fs::read_to_string(state_path()).unwrap_or_default();
    let get = |k: &str| s.lines().find_map(|l| l.strip_prefix(&format!("{k}="))).unwrap_or("").trim().to_string();
    (get("last"), get("installed") == "1")
}

/// What the request would be today, or None (sent today already). Pure, for the tests.
pub fn due(last: &str, installed: bool, today: &str, version: &str, os: &str) -> Option<String> {
    if last == today {
        return None;
    }
    Some(format!("{URL}?v={version}&os={os}&new={}", if installed { 0 } else { 1 }))
}

fn os() -> &'static str {
    if cfg!(windows) {
        "windows"
    } else {
        "linux"
    }
}

/// Starts the background thread: a first look a minute after start, then once an hour (it sleeps in between - no
/// timers on the UI thread, nothing while idle). Does nothing at all for own builds and test copies.
pub fn start() {
    if !release_build() || std::env::var_os("LASSO_HOME").is_some() {
        return;
    }
    let _ = std::thread::Builder::new().name("ping".into()).spawn(|| {
        std::thread::sleep(Duration::from_secs(60));
        loop {
            check();
            std::thread::sleep(Duration::from_secs(3600));
        }
    });
}

fn check() {
    if !crate::config::Config::load().on("usage_ping") {
        return;
    }
    let (last, installed) = read_state();
    let day = today();
    let Some(url) = due(&last, installed, &day, env!("CARGO_PKG_VERSION"), os()) else { return };
    if rustls::crypto::CryptoProvider::get_default().is_none() {
        let _ = rustls::crypto::ring::default_provider().install_default();
    }
    let ok = reqwest::blocking::Client::builder()
        .user_agent(concat!("Lasso/", env!("CARGO_PKG_VERSION")))
        .connect_timeout(Duration::from_secs(15))
        .timeout(Duration::from_secs(20))
        .build()
        .ok()
        .and_then(|c| c.get(&url).send().ok())
        .is_some_and(|r| r.status().is_success());
    if ok {
        // written only after it went out: offline today = tried again in an hour
        let _ = std::fs::write(state_path(), format!("last={day}\ninstalled=1\n"));
        crate::log::line("ping", "daily count sent");
    }
}

#[cfg(test)]
mod tests {
    use super::due;

    #[test]
    fn once_a_day_and_new_only_the_first_time() {
        let first = due("", false, "2026-11-20", "0.2.0", "linux").unwrap();
        assert!(first.ends_with("?v=0.2.0&os=linux&new=1"), "{first}");
        assert_eq!(due("2026-11-20", true, "2026-11-20", "0.2.0", "linux"), None);
        let next = due("2026-11-20", true, "2026-11-21", "0.2.0", "windows").unwrap();
        assert!(next.ends_with("&os=windows&new=0"), "{next}");
    }
}
