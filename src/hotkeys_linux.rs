//! Global hotkeys through the XDG GlobalShortcuts portal (Wayland gives apps no key hooks). Copied from Aimdot's
//! overlay/src/linux/hotkeys.rs (itself Echo's hotkey_linux.rs approach), changed for Lasso's 5 keys.
//!
//! the user, 13 Nov 2026: "prevent KDE keys until the user requires it, same rules as aimdot and echo" - this
//! thread is only started once the user turns global hotkeys ON in Settings (config `hotkeys=1`). Before that:
//! the tray icon and `lasso --region` (bound in the desktop's own shortcut settings) work.
//!
//! KDE owns the key after the first confirmation (System Settings > Shortcuts > Lasso); the keys KDE really uses
//! (`trigger_description`) are written to `kde_keys.txt` for the Settings window to show.

use dbus::arg::{PropMap, RefArg, Variant};
use dbus::blocking::Connection;
use dbus::message::MatchRule;
use std::io::{Read, Write};
use std::os::fd::AsRawFd;
use std::os::unix::net::UnixStream;
use std::sync::{Arc, Mutex};
use std::time::Duration;

const PORTAL: &str = "org.freedesktop.portal.Desktop";
const PORTAL_PATH: &str = "/org/freedesktop/portal/desktop";
const IFACE: &str = "org.freedesktop.portal.GlobalShortcuts";
const APP_ID: &str = "link.sonela.Lasso";
/// Never rename: KDE remembers the user's keys by id. Same order as config hk.<id>.
pub const IDS: [&str; N] = ["region", "full", "window", "repeat", "restore", "delete"];
pub const WHAT: [&str; N] = ["Capture an area", "Full screenshot", "Capture a window", "Repeat the last box", "Restore the last capture", "Delete the last capture"];
pub const N: usize = 6;

struct Shared {
    keys: [String; N],
    dirty: bool,
    /// switched off in Settings: close the portal session, end the thread
    stop: bool,
}

pub struct Hotkeys {
    shared: Arc<Mutex<Shared>>,
    wake: UnixStream,
}

impl Hotkeys {
    /// `fire` gets the id ("region", ...) of a pressed key.
    pub fn start(fire: impl Fn(&str) + Send + Sync + 'static, keys: [String; N]) -> Hotkeys {
        let shared = Arc::new(Mutex::new(Shared { keys, dirty: true, stop: false }));
        let (wake_r, wake) = UnixStream::pair().expect("socketpair");
        let _ = wake_r.set_nonblocking(true);
        let _ = wake.set_nonblocking(true);
        let sh = shared.clone();
        let fire: Arc<dyn Fn(&str) + Send + Sync> = Arc::new(fire);
        let _ = std::thread::Builder::new().name("hotkeys".into()).stack_size(256 * 1024).spawn(move || loop {
            // switched off (also while waiting after an error): never a NEW portal session / KDE dialog (review D11)
            if sh.lock().unwrap().stop {
                crate::log::line("hotkeys", "switched off - the desktop's shortcut session is closed");
                break;
            }
            let r = run(&fire, &sh, &wake_r);
            if let Err(e) = r {
                if sh.lock().unwrap().stop {
                    continue;
                }
                crate::log::line("hotkeys", &e);
                let _ = std::fs::write(crate::paths::config_dir().join("kde_keys.txt"), format!("error={e}\n"));
                loop {
                    let _ = wait(&[wake_r.as_raw_fd()]);
                    drain(&wake_r);
                    let s = sh.lock().unwrap();
                    if s.dirty || s.stop {
                        break;
                    }
                }
            }
        });
        Hotkeys { shared, wake }
    }

    /// Global hotkeys switched off: the portal session is closed (KDE drops Lasso's keys), the thread ends
    pub fn stop(&self) {
        self.shared.lock().unwrap().stop = true;
        let _ = (&self.wake).write(&[1]);
    }

    pub fn set(&self, keys: [String; N]) {
        let mut s = self.shared.lock().unwrap();
        if s.keys != keys {
            s.keys = keys;
            s.dirty = true;
            let _ = (&self.wake).write(&[1]);
        }
    }
}

fn wait(fds: &[i32]) -> bool {
    let mut p: Vec<libc::pollfd> = fds.iter().map(|&fd| libc::pollfd { fd, events: libc::POLLIN, revents: 0 }).collect();
    unsafe { libc::poll(p.as_mut_ptr(), p.len() as libc::nfds_t, -1) };
    p[0].revents & (libc::POLLHUP | libc::POLLERR | libc::POLLNVAL) == 0
}

fn drain(r: &UnixStream) {
    let mut b = [0u8; 64];
    while (&*r).read(&mut b).map_or(false, |n| n > 0) {}
}

/// "Ctrl+Shift+Print" -> "CTRL+SHIFT+Print" (the shortcuts spec's format). None = no key.
pub fn trigger(k: &str) -> Option<String> {
    let k = k.trim();
    if k.is_empty() {
        return None;
    }
    let mut out = String::new();
    let mut key = None;
    for p in k.split('+') {
        match p.trim().to_ascii_lowercase().as_str() {
            "ctrl" | "control" => out += "CTRL+",
            "alt" => out += "ALT+",
            "shift" => out += "SHIFT+",
            "meta" | "super" | "win" | "logo" => out += "LOGO+",
            _ => key = Some(p.trim().to_string()),
        }
    }
    let key = key?;
    let key = if key.len() == 1 { key.to_lowercase() } else { key };
    Some(out + &key)
}

fn triggers_of(v: &dyn RefArg, into: &mut [String; N]) -> bool {
    let mut any = false;
    let Some(items) = v.as_iter() else { return false };
    for item in items {
        let Some(mut f) = item.as_iter() else { continue };
        let (Some(id), Some(dict)) = (f.next().and_then(|x| x.as_str().map(String::from)), f.next()) else { continue };
        let Some(n) = IDS.iter().position(|&i| i == id) else { continue };
        let mut trig = String::new();
        if let Some(mut d) = dict.as_iter() {
            while let (Some(k), Some(val)) = (d.next(), d.next()) {
                if k.as_str() == Some("trigger_description") {
                    trig = val.as_str().map(String::from).or_else(|| val.as_iter().and_then(|mut i| i.next().and_then(|x| x.as_str().map(String::from)))).unwrap_or_default();
                }
            }
        }
        into[n] = trig;
        any = true;
    }
    any
}

fn write_keys(k: &[String; N]) {
    let s: String = IDS.iter().zip(k.iter()).map(|(i, t)| format!("{i}={t}\n")).collect();
    let _ = std::fs::write(crate::paths::config_dir().join("kde_keys.txt"), s);
}

fn var(v: impl RefArg + 'static) -> Variant<Box<dyn RefArg>> {
    Variant(Box::new(v))
}

fn run(fire: &Arc<dyn Fn(&str) + Send + Sync>, shared: &Arc<Mutex<Shared>>, wake_r: &UnixStream) -> Result<(), String> {
    let mut ch = dbus::channel::Channel::get_private(dbus::channel::BusType::Session).map_err(|e| format!("no D-Bus session: {e}"))?;
    ch.set_watch_enabled(true);
    let conn = Connection::from(ch);
    let me = conn.unique_name().trim_start_matches(':').replace('.', "_");
    let proxy = conn.with_proxy(PORTAL, PORTAL_PATH, Duration::from_secs(30));
    let reg: Result<(), _> = proxy.method_call("org.freedesktop.host.portal.Registry", "Register", (APP_ID, PropMap::new()));
    if let Err(e) = reg {
        crate::log::line("hotkeys", &format!("portal app id not registered ({e})"));
    }
    let session: Arc<Mutex<String>> = Arc::default();
    let keys: Arc<Mutex<[String; N]>> = Arc::default();
    // CreateSession's answer (Response on its request): (code, session_handle). The session only exists after it -
    // binding right away gave "BindShortcuts: Invalid session" on Plasma (20 Nov 2026)
    let created: Arc<Mutex<Option<(u32, Option<String>)>>> = Arc::default();
    {
        let (created, me) = (created.clone(), me.clone());
        conn.add_match(MatchRule::new_signal("org.freedesktop.portal.Request", "Response"), move |_: (), _, msg| {
            let ours = msg.path().is_some_and(|p| p.contains(&format!("/request/{me}/lasso")) && p.ends_with('r'));
            if ours {
                if let (Some(code), Some(res)) = msg.get2::<u32, PropMap>() {
                    let h = res.get("session_handle").and_then(|v| v.0.as_str().map(|s| s.to_string()));
                    *created.lock().unwrap() = Some((code, h));
                }
            }
            true
        })
        .map_err(|e| e.to_string())?;
    }
    {
        let (fire, session) = (fire.clone(), session.clone());
        conn.add_match(MatchRule::new_signal(IFACE, "Activated"), move |_: (), _, msg| {
            let (s, id, _, _) = msg.get4::<dbus::Path, String, u64, PropMap>();
            if s.as_ref().map(|p| p.to_string()) == Some(session.lock().unwrap().clone()) {
                if let Some(id) = id {
                    fire(&id);
                }
            }
            true
        })
        .map_err(|e| e.to_string())?;
    }
    {
        let (keys, me) = (keys.clone(), me.clone());
        conn.add_match(MatchRule::new_signal("org.freedesktop.portal.Request", "Response"), move |_: (), _, msg| {
            let ours = msg.path().is_some_and(|p| p.contains(&format!("/request/{me}/lasso")) && p.ends_with('b'));
            if ours {
                if let (Some(code), Some(res)) = msg.get2::<u32, PropMap>() {
                    let mut k = keys.lock().unwrap();
                    if code == 0 {
                        if let Some(v) = res.get("shortcuts") {
                            triggers_of(&v.0, &mut k);
                        }
                    }
                    write_keys(&k);
                }
            }
            true
        })
        .map_err(|e| e.to_string())?;
    }
    {
        let (keys, session) = (keys.clone(), session.clone());
        conn.add_match(MatchRule::new_signal(IFACE, "ShortcutsChanged"), move |_: (), _, msg| {
            let mut it = msg.iter_init();
            let s: Option<dbus::Path> = it.read().ok();
            if s.map(|p| p.to_string()) == Some(session.lock().unwrap().clone()) {
                if let Some(v) = it.get_refarg() {
                    let mut k = keys.lock().unwrap();
                    if triggers_of(&*v, &mut k) {
                        write_keys(&k);
                    }
                }
            }
            true
        })
        .map_err(|e| e.to_string())?;
    }
    let mut n = 0u32;
    loop {
        let bind = {
            let mut s = shared.lock().unwrap();
            std::mem::take(&mut s.dirty).then(|| s.keys.clone())
        };
        if let Some(want) = bind {
            let old = std::mem::take(&mut *session.lock().unwrap());
            if !old.is_empty() {
                let sp = conn.with_proxy(PORTAL, old, Duration::from_secs(5));
                let _: Result<(), _> = sp.method_call("org.freedesktop.portal.Session", "Close", ());
            }
            n += 1;
            let tok = format!("lasso{}_{n}", std::process::id());
            let mut opt = PropMap::new();
            opt.insert("handle_token".into(), var(format!("{tok}r")));
            opt.insert("session_handle_token".into(), var(tok.clone()));
            *created.lock().unwrap() = None;
            let _: (dbus::Path,) = proxy.method_call(IFACE, "CreateSession", (opt,)).map_err(|e| format!("no shortcuts portal ({e})"))?;
            // wait for the portal's answer (normally a few ms; no dialog for this step)
            let t0 = std::time::Instant::now();
            let answer = loop {
                if let Some(a) = created.lock().unwrap().take() {
                    break Some(a);
                }
                if t0.elapsed() > Duration::from_secs(10) {
                    break None;
                }
                conn.process(Duration::from_millis(50)).map_err(|e| e.to_string())?;
            };
            let sp = match answer {
                Some((0, h)) => h.unwrap_or_else(|| format!("{PORTAL_PATH}/session/{me}/{tok}")),
                Some((code, _)) => return Err(format!("the desktop refused a shortcut session (code {code})")),
                None => return Err("the desktop's shortcuts portal didn't answer".into()),
            };
            *session.lock().unwrap() = sp.clone();
            let list: Vec<(String, PropMap)> = (0..N)
                .map(|i| {
                    let mut m = PropMap::new();
                    m.insert("description".into(), var(format!("Lasso: {}", WHAT[i])));
                    if let Some(t) = trigger(&want[i]) {
                        m.insert("preferred_trigger".into(), var(t));
                    }
                    (IDS[i].to_string(), m)
                })
                .collect();
            let mut opt = PropMap::new();
            opt.insert("handle_token".into(), var(format!("{tok}b")));
            let _: (dbus::Path,) = proxy.method_call(IFACE, "BindShortcuts", (dbus::Path::from(sp), list, "", opt)).map_err(|e| format!("BindShortcuts: {e}"))?;
            crate::log::line("hotkeys", "offered to the desktop's shortcuts portal");
        }
        while conn.process(Duration::ZERO).map_err(|e| e.to_string())? {}
        if !wait(&[conn.channel().watch().fd, wake_r.as_raw_fd()]) {
            return Err("D-Bus connection lost".into());
        }
        drain(wake_r);
        if shared.lock().unwrap().stop {
            let old = std::mem::take(&mut *session.lock().unwrap());
            if !old.is_empty() {
                let sp = conn.with_proxy(PORTAL, old, Duration::from_secs(5));
                let _: Result<(), _> = sp.method_call("org.freedesktop.portal.Session", "Close", ());
            }
            return Ok(());
        }
        conn.process(Duration::ZERO).map_err(|e| format!("D-Bus connection lost: {e}"))?;
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn triggers() {
        assert_eq!(super::trigger("Ctrl+Shift+Print").as_deref(), Some("CTRL+SHIFT+Print"));
        assert_eq!(super::trigger("Alt+X").as_deref(), Some("ALT+x"));
        assert_eq!(super::trigger(""), None);
    }
}
