//! Linux tray icon: a StatusNotifierItem + com.canonical.dbusmenu menu, hand-written on the light blocking `dbus`
//! crate (copied from Aimdot's overlay/src/linux/tray.rs and changed for Lasso's menu). One thread, asleep in
//! poll() on the D-Bus socket + a wake pipe.
//!
//! Left click = capture an area. Menu: "Restore last capture · 23:41" on top (greyed when there is none -
//! the user, 13 Nov 2026: the Esc restore lives here after the first-time popup), then the capture kinds,
//! Library, Settings, Quit. While an update is known (src/update.rs): "Update to Lasso X" + a separator above all.

use dbus::arg::{PropMap, RefArg, Variant};
use dbus::blocking::Connection;
use dbus::channel::{MatchingReceiver, Sender};
use dbus::message::{MatchRule, MessageType};
use dbus::Message;
use std::io::{Read, Write};
use std::os::fd::AsRawFd;
use std::os::unix::net::UnixStream;
use std::sync::{Arc, Mutex};
use std::time::Duration;

const WATCHER: &str = "org.kde.StatusNotifierWatcher";
const SNI: &str = "org.kde.StatusNotifierItem";
const SNI_PATH: &str = "/StatusNotifierItem";
const MENU: &str = "com.canonical.dbusmenu";
const MENU_PATH: &str = "/MenuBar";
const PROPS: &str = "org.freedesktop.DBus.Properties";
const ICON_NAME: &str = "link.sonela.Lasso";

const ID_RESTORE: i32 = 1;
const ID_SEP1: i32 = 2;
const ID_REGION: i32 = 3;
const ID_FULL: i32 = 4;
const ID_WINDOW: i32 = 5;
const ID_SEP2: i32 = 6;
const ID_LIBRARY: i32 = 7;
const ID_SETTINGS: i32 = 8;
const ID_SEP3: i32 = 9;
const ID_QUIT: i32 = 10;
const ID_PINS: i32 = 11;
const ID_UPDATE: i32 = 12;
const ID_SEP_UPDATE: i32 = 13;
const IDS: [i32; 11] = [ID_RESTORE, ID_SEP1, ID_REGION, ID_FULL, ID_WINDOW, ID_SEP2, ID_LIBRARY, ID_PINS, ID_SETTINGS, ID_SEP3, ID_QUIT];

/// What the menu shows that can change: the restore label (None = nothing to restore) and "Update to Lasso X"
/// (None = no update known)
#[derive(Clone, Default, PartialEq)]
struct Labels {
    restore: Option<String>,
    update: Option<String>,
}

type Shared = Arc<Mutex<Labels>>;

/// The menu's items, top to bottom
fn ids(l: &Labels) -> Vec<i32> {
    let mut v = if l.update.is_some() { vec![ID_UPDATE, ID_SEP_UPDATE] } else { Vec::new() };
    v.extend(IDS);
    v
}

pub struct Tray {
    restore: Shared,
    wake: UnixStream,
}

impl Tray {
    /// `send` gets the command ("region", "full", "window", "restore", "library", "settings", "update", "quit")
    pub fn start(send: impl Fn(&str) + Send + Sync + 'static) -> Tray {
        let restore: Shared = Arc::new(Mutex::new(Labels::default()));
        let (wake_r, wake) = UnixStream::pair().expect("socketpair");
        let _ = wake_r.set_nonblocking(true);
        let _ = wake.set_nonblocking(true);
        let r2 = restore.clone();
        let send: Arc<dyn Fn(&str) + Send + Sync> = Arc::new(send);
        let _ = std::thread::Builder::new().name("tray".into()).stack_size(256 * 1024).spawn(move || {
            if let Err(e) = run(send, r2, wake_r) {
                crate::log::line("tray", &e);
            }
        });
        Tray { restore, wake }
    }

    /// "Restore last capture · 23:41" (Some) or greyed (None)
    pub fn set_restore(&self, label: Option<String>) {
        self.change(|l| l.restore = label);
    }

    /// "Update to Lasso X" on top of the menu (Some) or not there (None)
    pub fn set_update(&self, label: Option<String>) {
        self.change(|l| l.update = label);
    }

    fn change(&self, f: impl FnOnce(&mut Labels)) {
        let changed = self
            .restore
            .lock()
            .map(|mut r| {
                let old = r.clone();
                f(&mut r);
                *r != old
            })
            .unwrap_or(false);
        if changed {
            let _ = (&self.wake).write(&[1]);
        }
    }
}

type Var = Variant<Box<dyn RefArg>>;
type Pixmaps = Vec<(i32, i32, Vec<u8>)>;

fn var(v: impl RefArg + 'static) -> Var {
    Variant(Box::new(v))
}

fn pixmaps() -> Pixmaps {
    let pngs: [&[u8]; 3] = [
        include_bytes!("../assets/icons/lasso-24.png"),
        include_bytes!("../assets/icons/lasso-32.png"),
        include_bytes!("../assets/icons/lasso-48.png"),
    ];
    pngs.iter()
        .filter_map(|d| image::load_from_memory(d).ok())
        .map(|im| {
            let im = im.to_rgba8();
            let (w, h) = (im.width() as i32, im.height() as i32);
            let argb = im.into_raw().chunks_exact(4).flat_map(|p| [p[3], p[0], p[1], p[2]]).collect();
            (w, h, argb)
        })
        .collect()
}

fn sni_props(icons: &Pixmaps) -> PropMap {
    let mut m = PropMap::new();
    m.insert("Category".into(), var("ApplicationStatus".to_string()));
    m.insert("Id".into(), var("lasso".to_string()));
    m.insert("Title".into(), var("Lasso".to_string()));
    m.insert("Status".into(), var("Active".to_string()));
    m.insert("WindowId".into(), var(0i32));
    m.insert("IconName".into(), var(ICON_NAME.to_string()));
    m.insert("IconPixmap".into(), var(icons.clone()));
    m.insert("IconThemePath".into(), var(String::new()));
    m.insert("OverlayIconName".into(), var(String::new()));
    m.insert("AttentionIconName".into(), var(String::new()));
    let tip: (String, Pixmaps, String, String) = (String::new(), Vec::new(), "Lasso".to_string(), "Click: capture an area".to_string());
    m.insert("ToolTip".into(), var(tip));
    m.insert("ItemIsMenu".into(), var(false));
    m.insert("Menu".into(), var(dbus::Path::from(MENU_PATH)));
    m
}

fn menu_props() -> PropMap {
    let mut m = PropMap::new();
    m.insert("Version".into(), var(3u32));
    m.insert("TextDirection".into(), var("ltr".to_string()));
    m.insert("Status".into(), var("normal".to_string()));
    m.insert("IconThemePath".into(), var(Vec::<String>::new()));
    m
}

fn item(id: i32, l: &Labels) -> PropMap {
    let restore = &l.restore;
    let mut m = PropMap::new();
    let label = |m: &mut PropMap, s: &str| {
        m.insert("label".into(), var(s.to_string()));
    };
    match id {
        0 => {
            m.insert("children-display".into(), var("submenu".to_string()));
        }
        ID_RESTORE => {
            label(&mut m, &restore.clone().unwrap_or_else(|| "Restore last capture".into()));
            m.insert("enabled".into(), var(restore.is_some()));
            m.insert("icon-name".into(), var("edit-undo".to_string()));
        }
        ID_REGION => {
            label(&mut m, "Capture an area");
            m.insert("icon-name".into(), var("transform-crop".to_string()));
        }
        ID_FULL => {
            label(&mut m, "Full screenshot");
            m.insert("icon-name".into(), var("view-fullscreen".to_string()));
        }
        ID_WINDOW => {
            label(&mut m, "Capture a window");
            m.insert("icon-name".into(), var("window".to_string()));
        }
        ID_LIBRARY => label(&mut m, "Library"),
        ID_PINS => label(&mut m, "Make pins clickable again"),
        ID_SETTINGS => {
            label(&mut m, "Settings");
            m.insert("icon-name".into(), var("configure".to_string()));
        }
        ID_QUIT => label(&mut m, "Quit Lasso"),
        ID_UPDATE => {
            label(&mut m, l.update.as_deref().unwrap_or("Update Lasso"));
            m.insert("icon-name".into(), var("system-software-update".to_string()));
        }
        ID_SEP1 | ID_SEP2 | ID_SEP3 | ID_SEP_UPDATE => {
            m.insert("type".into(), var("separator".to_string()));
        }
        _ => {}
    }
    m
}

type Layout = (i32, PropMap, Vec<Var>);

fn layout(parent: i32, r: &Labels) -> Layout {
    let kids = if parent == 0 { ids(r).into_iter().map(|id| var((id, item(id, r), Vec::<Var>::new()))).collect() } else { Vec::new() };
    (parent, item(parent, r), kids)
}

fn register(conn: &Connection, name: &str) {
    if let Ok(m) = Message::new_method_call(WATCHER, "/StatusNotifierWatcher", WATCHER, "RegisterStatusNotifierItem") {
        let _ = conn.send(m.append1(name));
    }
}

fn clicked(send: &Arc<dyn Fn(&str) + Send + Sync>, id: i32) {
    let c = match id {
        ID_RESTORE => "restore",
        ID_REGION => "region",
        ID_FULL => "full",
        ID_WINDOW => "window",
        ID_LIBRARY => "library",
        ID_PINS => "pins-clickable",
        ID_SETTINGS => "settings",
        ID_UPDATE => "update",
        ID_QUIT => "quit",
        _ => return,
    };
    send(c);
}

const INTROSPECT: &str = r#"<!DOCTYPE node PUBLIC "-//freedesktop//DTD D-BUS Object Introspection 1.0//EN" "http://www.freedesktop.org/standards/dbus/1.0/introspect.dtd">
<node>
 <interface name="org.kde.StatusNotifierItem">
  <method name="Activate"><arg type="i" direction="in"/><arg type="i" direction="in"/></method>
  <method name="SecondaryActivate"><arg type="i" direction="in"/><arg type="i" direction="in"/></method>
  <method name="ContextMenu"><arg type="i" direction="in"/><arg type="i" direction="in"/></method>
  <method name="Scroll"><arg type="i" direction="in"/><arg type="s" direction="in"/></method>
  <method name="ProvideXdgActivationToken"><arg type="s" direction="in"/></method>
 </interface>
 <interface name="com.canonical.dbusmenu">
  <method name="GetLayout"><arg type="i" direction="in"/><arg type="i" direction="in"/><arg type="as" direction="in"/><arg type="u" direction="out"/><arg type="(ia{sv}av)" direction="out"/></method>
  <method name="Event"><arg type="i" direction="in"/><arg type="s" direction="in"/><arg type="v" direction="in"/><arg type="u" direction="in"/></method>
  <signal name="LayoutUpdated"><arg type="u"/><arg type="i"/></signal>
 </interface>
</node>"#;

fn answer(msg: &Message, send: &Arc<dyn Fn(&str) + Send + Sync>, restore: &Shared, rev: u32, icons: &Pixmaps) -> Option<Message> {
    let r_label = restore.lock().map(|r| r.clone()).unwrap_or_default();
    let path = msg.path()?.to_string();
    let iface = msg.interface().map(|i| i.to_string()).unwrap_or_default();
    let member = msg.member()?.to_string();
    let all = || if path == SNI_PATH { Some(sni_props(icons)) } else if path == MENU_PATH { Some(menu_props()) } else { None };
    let r = msg.method_return();
    Some(match (iface.as_str(), member.as_str()) {
        ("org.freedesktop.DBus.Introspectable", "Introspect") => r.append1(INTROSPECT),
        (PROPS, "GetAll") => r.append1(all()?),
        (PROPS, "Get") => {
            let (_, name) = msg.read2::<String, String>().ok()?;
            let v = all()?.remove(&name)?;
            r.append1(v)
        }
        (SNI, "Activate") => {
            send("region");
            r
        }
        (SNI, "SecondaryActivate") => {
            send("full");
            r
        }
        (SNI, "ProvideXdgActivationToken" | "ContextMenu" | "Scroll") => r,
        (MENU, "GetLayout") => {
            let parent = msg.read1::<i32>().unwrap_or(0);
            r.append2(rev, layout(parent, &r_label))
        }
        (MENU, "GetGroupProperties") => {
            let ids = msg.read1::<Vec<i32>>().unwrap_or_default();
            let ids = if ids.is_empty() { [vec![0], self::ids(&r_label)].concat() } else { ids };
            let v: Vec<(i32, PropMap)> = ids.into_iter().map(|id| (id, item(id, &r_label))).collect();
            r.append1(v)
        }
        (MENU, "GetProperty") => {
            let (id, name) = msg.read2::<i32, String>().ok()?;
            r.append1(item(id, &r_label).remove(&name)?)
        }
        (MENU, "Event") => {
            let (id, kind) = msg.read2::<i32, String>().ok()?;
            if kind == "clicked" {
                clicked(send, id);
            }
            r
        }
        (MENU, "EventGroup") => {
            let evs = msg.read1::<Vec<(i32, String, Var, u32)>>().unwrap_or_default();
            for (id, kind, _, _) in evs {
                if kind == "clicked" {
                    clicked(send, id);
                }
            }
            r.append1(Vec::<i32>::new())
        }
        (MENU, "AboutToShow") => r.append1(false),
        (MENU, "AboutToShowGroup") => r.append2(Vec::<i32>::new(), Vec::<i32>::new()),
        _ => return None,
    })
}

fn wait(fds: &[i32], timeout: Duration) -> bool {
    let mut p: Vec<libc::pollfd> = fds.iter().map(|&fd| libc::pollfd { fd, events: libc::POLLIN, revents: 0 }).collect();
    let ms = timeout.as_millis().min(i32::MAX as u128) as i32;
    unsafe { libc::poll(p.as_mut_ptr(), p.len() as libc::nfds_t, ms) };
    p[0].revents & (libc::POLLHUP | libc::POLLERR | libc::POLLNVAL) == 0
}

fn run(send: Arc<dyn Fn(&str) + Send + Sync>, restore: Shared, wake_r: UnixStream) -> Result<(), String> {
    let mut ch = dbus::channel::Channel::get_private(dbus::channel::BusType::Session).map_err(|e| format!("no D-Bus session: {e}"))?;
    ch.set_watch_enabled(true);
    let conn = Connection::from(ch);
    let name = format!("{SNI}-{}-1", std::process::id());
    conn.request_name(name.as_str(), false, true, true).map_err(|e| format!("bus name: {e}"))?;
    let icons = pixmaps();
    let rev = Arc::new(std::sync::atomic::AtomicU32::new(1));
    {
        let (send, restore, rev) = (send.clone(), restore.clone(), rev.clone());
        conn.start_receive(
            MatchRule::new_method_call(),
            Box::new(move |msg: Message, c: &Connection| {
                if msg.msg_type() == MessageType::MethodCall {
                    let reply = answer(&msg, &send, &restore, rev.load(std::sync::atomic::Ordering::Relaxed), &icons).unwrap_or_else(|| {
                        let e = dbus::strings::ErrorName::from("org.freedesktop.DBus.Error.UnknownMethod");
                        msg.error(&e, &std::ffi::CString::new("not supported").unwrap())
                    });
                    if !msg.get_no_reply() {
                        let _ = c.send(reply);
                    }
                }
                true
            }),
        );
    }
    {
        let name = name.clone();
        let rule = MatchRule::new_signal("org.freedesktop.DBus", "NameOwnerChanged").with_sender("org.freedesktop.DBus");
        conn.add_match(rule, move |(who, _, new): (String, String, String), c, _| {
            if who == WATCHER && !new.is_empty() {
                register(c, &name);
            }
            true
        })
        .map_err(|e| e.to_string())?;
    }
    register(&conn, &name);
    crate::log::line("tray", "icon offered to the panel");
    let mut shown = restore.lock().map(|r| r.clone()).unwrap_or_default();
    loop {
        while conn.process(Duration::ZERO).map_err(|e| e.to_string())? {}
        if !wait(&[conn.channel().watch().fd, wake_r.as_raw_fd()], Duration::from_secs(3600)) {
            return Err("D-Bus connection lost".into());
        }
        let mut b = [0u8; 64];
        while (&wake_r).read(&mut b).map_or(false, |n| n > 0) {}
        let now = restore.lock().map(|r| r.clone()).unwrap_or_default();
        if now != shown {
            shown = now.clone();
            let r = rev.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1;
            let changed: Vec<(i32, PropMap)> = vec![(ID_RESTORE, item(ID_RESTORE, &now)), (ID_UPDATE, item(ID_UPDATE, &now))];
            if let Ok(s) = Message::new_signal(MENU_PATH, MENU, "ItemsPropertiesUpdated") {
                let _ = conn.send(s.append2(changed, Vec::<(i32, Vec<String>)>::new()));
            }
            if let Ok(s) = Message::new_signal(MENU_PATH, MENU, "LayoutUpdated") {
                let _ = conn.send(s.append2(r, 0i32));
            }
        }
        conn.process(Duration::ZERO).map_err(|e| format!("D-Bus connection lost: {e}"))?;
    }
}
