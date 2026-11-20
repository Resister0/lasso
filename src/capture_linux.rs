//! Grabbing the screen on Linux.
//!
//! KDE Plasma: KWin's `org.kde.KWin.ScreenShot2` (what Spectacle uses): silent, fast, one call per monitor, in that
//! monitor's real pixels. KWin only answers apps whose installed .desktop file lists the interface in
//! `X-KDE-DBUS-Restricted-Interfaces` - Lasso writes its own .desktop file on first start (desktop.rs).
//! Other desktops (GNOME, ...): the xdg-desktop-portal Screenshot call (whole desktop, maybe a question once) ⚠.
//!
//! The window list (Window mode, "click = the window under the mouse") comes from a tiny KWin script that calls
//! Lasso back over D-Bus with every visible window's frame - done AFTER the picture is on screen (fast freeze).

use crate::shapes::{Pic, R};
use crate::wl::OutputInfo;
use dbus::arg::{PropMap, RefArg, Variant};
use dbus::blocking::Connection;
use std::collections::HashMap;
use std::io::Read;
use std::os::fd::{FromRawFd, OwnedFd};
use std::time::{Duration, Instant};

pub struct Shot {
    pub output: OutputInfo,
    pub pic: Pic,
}

fn pipe() -> Result<(OwnedFd, OwnedFd), String> {
    let mut fds = [0i32; 2];
    if unsafe { libc::pipe2(fds.as_mut_ptr(), libc::O_CLOEXEC) } != 0 {
        return Err("pipe failed".into());
    }
    Ok(unsafe { (OwnedFd::from_raw_fd(fds[0]), OwnedFd::from_raw_fd(fds[1])) })
}

fn prop_u32(m: &PropMap, k: &str) -> Option<u32> {
    m.get(k).and_then(|v| v.0.as_u64()).map(|v| v as u32)
}

/// QImage formats KWin uses -> RGBA (straight; screenshots are opaque)
fn to_rgba(raw: &[u8], w: u32, h: u32, stride: u32, format: u32) -> Result<Pic, String> {
    let mut pic = Pic::new(w, h);
    let bgra = match format {
        4..=6 => true,       // RGB32 / ARGB32 / ARGB32_Premultiplied: 0xAARRGGBB words = B G R A bytes
        16..=18 => false,    // RGBX8888 / RGBA8888 / premultiplied: R G B A bytes
        f => return Err(format!("picture format {f} not handled")),
    };
    if raw.len() < (stride * (h - 1) + w * 4) as usize {
        return Err(format!("short picture ({} bytes)", raw.len()));
    }
    for y in 0..h as usize {
        let s = &raw[y * stride as usize..y * stride as usize + w as usize * 4];
        let d = &mut pic.px[y * w as usize * 4..(y + 1) * w as usize * 4];
        for (sp, dp) in s.chunks_exact(4).zip(d.chunks_exact_mut(4)) {
            if bgra {
                dp[0] = sp[2];
                dp[1] = sp[1];
                dp[2] = sp[0];
            } else {
                dp[0] = sp[0];
                dp[1] = sp[1];
                dp[2] = sp[2];
            }
            dp[3] = 255;
        }
    }
    Ok(pic)
}

fn kwin_call(conn: &Connection, method: &str, name: Option<&str>, cursor: bool) -> Result<Pic, String> {
    let (r, w) = pipe()?;
    // read while KWin writes (it may write before or after it answers)
    let reader = std::thread::spawn(move || {
        let mut f = std::fs::File::from(r);
        let mut buf = Vec::with_capacity(8 << 20);
        let _ = f.read_to_end(&mut buf);
        buf
    });
    let mut opts: PropMap = HashMap::new();
    opts.insert("include-cursor".into(), Variant(Box::new(cursor) as Box<dyn RefArg>));
    if std::env::var_os("LASSO_NO_NATIVE").is_none() {
        opts.insert("native-resolution".into(), Variant(Box::new(true) as Box<dyn RefArg>));
    }
    let proxy = conn.with_proxy("org.kde.KWin", "/org/kde/KWin/ScreenShot2", Duration::from_secs(5));
    // the dbus crate's own fd type (closed when the call is done)
    let w = unsafe { dbus::arg::OwnedFd::new(std::os::fd::IntoRawFd::into_raw_fd(w)) };
    let res: Result<(PropMap,), dbus::Error> = match name {
        Some(n) => proxy.method_call("org.kde.KWin.ScreenShot2", method, (n, opts, w)),
        None => proxy.method_call("org.kde.KWin.ScreenShot2", method, (opts, w)),
    };
    // `w` was moved into the call and is closed now: the reader sees EOF once KWin closes its copy
    let (meta,) = res.map_err(|e| format!("KWin screenshot: {}", e.message().unwrap_or("refused")))?;
    let raw = reader.join().map_err(|_| "reader".to_string())?;
    let (w, h) = (prop_u32(&meta, "width").unwrap_or(0), prop_u32(&meta, "height").unwrap_or(0));
    let stride = prop_u32(&meta, "stride").unwrap_or(w * 4);
    let format = prop_u32(&meta, "format").unwrap_or(6);
    if std::env::var_os("LASSO_DEBUG_GRAB").is_some() {
        let keys: Vec<String> = meta.iter().map(|(k, v)| format!("{k}={:?}", v.0)).collect();
        let nz = raw.iter().filter(|b| **b != 0).count();
        crate::log::line("capture", &format!("meta {keys:?} raw {} bytes, {nz} non-zero", raw.len()));
    }
    if w == 0 || h == 0 {
        return Err("KWin sent an empty picture".into());
    }
    to_rgba(&raw, w, h, stride, format)
}

/// Every monitor at once (one thread each), in each monitor's own pixels.
pub fn grab_outputs(outs: &[OutputInfo], cursor: bool) -> Result<Vec<Shot>, String> {
    if let Some(p) = std::env::var_os("LASSO_TEST_PICTURE") {
        // test hook: a fixed picture instead of the screen (headless test pictures that look like a desktop)
        let img = image::open(&p).map_err(|e| e.to_string())?.to_rgba8();
        // every monitor gets the picture; the 2nd, 3rd ... get it inverted (multi-monitor tests can tell them apart)
        let mut shots = vec![];
        for (k, o) in outs.iter().enumerate() {
            let mut im = image::imageops::resize(&img, o.pw.max(1) as u32, o.ph.max(1) as u32, image::imageops::FilterType::Triangle);
            if k > 0 {
                im.pixels_mut().for_each(|p| {
                    p.0[0] = 255 - p.0[0];
                    p.0[1] = 255 - p.0[1];
                    p.0[2] = 255 - p.0[2];
                });
            }
            shots.push(Shot { output: o.clone(), pic: Pic { w: im.width(), h: im.height(), px: im.into_raw() } });
        }
        return Ok(shots);
    }
    grab_real(outs, cursor)
}

static CONNS: std::sync::LazyLock<std::sync::Mutex<HashMap<String, Connection>>> = std::sync::LazyLock::new(Default::default);

/// KWin (else the portal), never the test picture
pub fn grab_real(outs: &[OutputInfo], cursor: bool) -> Result<Vec<Shot>, String> {
    let t0 = Instant::now();
    let mut handles = vec![];
    for o in outs.iter().cloned() {
        handles.push(std::thread::spawn(move || -> Result<Shot, String> {
            // one D-Bus connection per monitor, kept between captures (a new one costs a few ms on every key press)
            let kept = CONNS.lock().unwrap().remove(&o.name);
            let conn = match kept {
                Some(c) => c,
                None => Connection::new_session().map_err(|e| e.to_string())?,
            };
            let r = kwin_call(&conn, "CaptureScreen", Some(&o.name), cursor);
            if r.is_ok() {
                CONNS.lock().unwrap().insert(o.name.clone(), conn);
            }
            Ok(Shot { output: o, pic: r? })
        }));
    }
    let mut shots = vec![];
    let mut err = None;
    for h in handles {
        match h.join() {
            Ok(Ok(s)) => shots.push(s),
            Ok(Err(e)) => err = Some(e),
            Err(_) => err = Some("capture thread".into()),
        }
    }
    crate::log::line("capture", &format!("KWin: {} screen(s) in {} ms", shots.len(), t0.elapsed().as_millis()));
    if shots.is_empty() {
        // not KWin (or not allowed): the portal
        match portal_screenshot() {
            Ok(pic) => {
                let o = outs.first().cloned().unwrap_or_default();
                return Ok(vec![Shot { output: o, pic }]);
            }
            Err(pe) => return Err(format!("{} / portal: {pe}", err.unwrap_or_default())),
        }
    }
    Ok(shots)
}

/// xdg-desktop-portal Screenshot (GNOME & co.): the whole desktop as a PNG file. ⚠ untested
pub fn portal_screenshot() -> Result<Pic, String> {
    use dbus::message::MatchRule;
    let conn = Connection::new_session().map_err(|e| e.to_string())?;
    let token = format!("lasso{}", std::process::id());
    let sender = conn.unique_name().trim_start_matches(':').replace('.', "_");
    let path = format!("/org/freedesktop/portal/desktop/request/{sender}/{token}");
    let got: std::sync::Arc<std::sync::Mutex<Option<(u32, PropMap)>>> = Default::default();
    let g2 = got.clone();
    let rule = MatchRule::new_signal("org.freedesktop.portal.Request", "Response").with_path(path.clone());
    conn.add_match(rule, move |(code, res): (u32, PropMap), _, _| {
        *g2.lock().unwrap() = Some((code, res));
        false
    })
    .map_err(|e| e.to_string())?;
    let mut opts: PropMap = HashMap::new();
    opts.insert("handle_token".into(), Variant(Box::new(token) as Box<dyn RefArg>));
    opts.insert("interactive".into(), Variant(Box::new(false) as Box<dyn RefArg>));
    let proxy = conn.with_proxy("org.freedesktop.portal.Desktop", "/org/freedesktop/portal/desktop", Duration::from_secs(5));
    let _: (dbus::Path,) = proxy.method_call("org.freedesktop.portal.Screenshot", "Screenshot", ("", opts)).map_err(|e| e.to_string())?;
    let until = Instant::now() + Duration::from_secs(30);
    while got.lock().unwrap().is_none() && Instant::now() < until {
        conn.process(Duration::from_millis(200)).map_err(|e| e.to_string())?;
    }
    let Some((code, res)) = got.lock().unwrap().take() else { return Err("no answer".into()) };
    if code != 0 {
        return Err("cancelled".into());
    }
    let uri = res.get("uri").and_then(|v| v.0.as_str()).ok_or("no uri")?.to_string();
    let file = uri.trim_start_matches("file://");
    let file = crate::util::url_decode(file);
    let img = image::open(&file).map_err(|e| e.to_string())?.to_rgba8();
    let _ = std::fs::remove_file(&file);
    Ok(Pic { w: img.width(), h: img.height(), px: img.into_raw() })
}

/// Visible windows, top first, in logical desktop coordinates (KWin script). Empty on other desktops.
pub fn kwin_windows() -> Vec<(R, String)> {
    let t0 = Instant::now();
    let r = kwin_windows_inner();
    match &r {
        Ok(v) => crate::log::line("capture", &format!("window list: {} in {} ms", v.len(), t0.elapsed().as_millis())),
        Err(e) => crate::log::line("capture", &format!("window list: {e}")),
    }
    r.unwrap_or_default()
}

fn kwin_windows_inner() -> Result<Vec<(R, String)>, String> {
    use dbus::channel::MatchingReceiver;
    use dbus::message::MatchRule;
    let conn = Connection::new_session().map_err(|e| e.to_string())?;
    let me = conn.unique_name().to_string();
    let script = format!(
        r#"(function() {{
const out = [];
const ws = workspace.stackingOrder;
for (let i = ws.length - 1; i >= 0; i--) {{
  const w = ws[i];
  if (!w || w.minimized || w.deleted || !(w.normalWindow || w.dialog)) continue;
  if (!w.onAllDesktops && w.desktops && w.desktops.indexOf(workspace.currentDesktop) < 0) continue;
  const g = w.frameGeometry;
  const name = String(w.resourceClass || "") + "|" + String(w.caption || "");
  out.push([Math.round(g.x), Math.round(g.y), Math.round(g.width), Math.round(g.height), name.replace(/[,;]/g, " ")].join(","));
}}
callDBus("{me}", "/lasso", "link.sonela.Lasso", "windows", out.join(";"));
}})();"#
    );
    let dir = crate::paths::runtime_dir();
    let file = dir.join(format!("lasso-windows-{}.js", std::process::id()));
    std::fs::write(&file, script).map_err(|e| e.to_string())?;
    let got: std::sync::Arc<std::sync::Mutex<Option<String>>> = Default::default();
    let g2 = got.clone();
    conn.start_receive(
        MatchRule::new_method_call(),
        Box::new(move |msg, c| {
            if msg.member().as_deref() == Some("windows") {
                if let Some(s) = msg.get1::<String>() {
                    *g2.lock().unwrap() = Some(s);
                }
                let _ = dbus::channel::Sender::send(c, msg.method_return());
            }
            true
        }),
    );
    let plugin = format!("lasso-windows-{}", std::process::id());
    let scripting = conn.with_proxy("org.kde.KWin", "/Scripting", Duration::from_secs(2));
    let _: Result<(bool,), _> = scripting.method_call("org.kde.kwin.Scripting", "unloadScript", (plugin.as_str(),));
    let (id,): (i32,) = scripting
        .method_call("org.kde.kwin.Scripting", "loadScript", (file.to_string_lossy().to_string(), plugin.as_str()))
        .map_err(|e| format!("loadScript: {e}"))?;
    let sp = conn.with_proxy("org.kde.KWin", format!("/Scripting/Script{id}"), Duration::from_secs(2));
    let run: Result<(), dbus::Error> = sp.method_call("org.kde.kwin.Script", "run", ());
    if run.is_err() {
        // older KWin: /<id>
        let sp = conn.with_proxy("org.kde.KWin", format!("/{id}"), Duration::from_secs(2));
        let _: Result<(), _> = sp.method_call("org.kde.kwin.Script", "run", ());
    }
    let until = Instant::now() + Duration::from_millis(1500);
    while got.lock().unwrap().is_none() && Instant::now() < until {
        let _ = conn.process(Duration::from_millis(50));
    }
    let _: Result<(bool,), _> = scripting.method_call("org.kde.kwin.Scripting", "unloadScript", (plugin.as_str(),));
    let _ = std::fs::remove_file(&file);
    let s = got.lock().unwrap().take().ok_or("no answer from the KWin script")?;
    let mut out = vec![];
    for item in s.split(';').filter(|x| !x.is_empty()) {
        let f: Vec<&str> = item.split(',').collect();
        if f.len() >= 4 {
            let n = |i: usize| f[i].trim().parse::<f32>().unwrap_or(0.0);
            out.push((R::new(n(0), n(1), n(2), n(3)), f.get(4).unwrap_or(&"").to_string()));
        }
    }
    Ok(out)
}
