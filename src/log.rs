//! `lasso.log` in the config folder: UTC time + `[area] message`, small (cut to the last 200 KB at start).
//! Never home paths in it (the log may be sent with a bug report).

use std::io::Write;
use std::sync::Mutex;

static LOCK: Mutex<()> = Mutex::new(());

pub fn line(area: &str, msg: &str) {
    let _g = LOCK.lock();
    let t = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default();
    let s = t.as_secs();
    let (h, m, sec, ms) = ((s / 3600) % 24, (s / 60) % 60, s % 60, t.subsec_millis());
    let msg = crate::util::scrub_home(msg);
    let text = format!("{h:02}:{m:02}:{sec:02}.{ms:03} [{area}] {msg}\n");
    eprint!("lasso: {text}");
    let p = crate::paths::config_dir().join("lasso.log");
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(p) {
        let _ = f.write_all(text.as_bytes());
    }
}

pub fn trim() {
    let p = crate::paths::config_dir().join("lasso.log");
    if let Ok(data) = std::fs::read(&p) {
        if data.len() > 400_000 {
            let _ = std::fs::write(&p, &data[data.len() - 200_000..]);
        }
    }
}
