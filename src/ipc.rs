//! One Lasso at a time: a second `lasso --region` (bound to a key in the desktop's own shortcut settings, or
//! from a script) just tells the running one, then exits. The Settings / Library windows (own processes) talk
//! to the running Lasso the same way.
//! Linux: a Unix socket in $XDG_RUNTIME_DIR (only this user can open it). Windows: 127.0.0.1 TCP + a random
//! token in a file in the user's own %APPDATA%\Lasso (like Aimdot).

use std::io::{BufRead, BufReader, Write};

#[cfg(unix)]
pub fn send(cmd: &str) -> Result<String, String> {
    let mut s = std::os::unix::net::UnixStream::connect(crate::paths::socket_path()).map_err(|e| e.to_string())?;
    let _ = s.set_read_timeout(Some(std::time::Duration::from_secs(5)));
    s.write_all(format!("{cmd}\n").as_bytes()).map_err(|e| e.to_string())?;
    let mut line = String::new();
    BufReader::new(s).read_line(&mut line).map_err(|e| e.to_string())?;
    Ok(line.trim().to_string())
}

/// Linux: one Lasso per user - an exclusive lock held for the whole run (two starts at the same moment can't both
/// become the running Lasso). false = another Lasso holds it.
#[cfg(unix)]
pub fn lock_single() -> bool {
    static LOCK: std::sync::OnceLock<std::fs::File> = std::sync::OnceLock::new();
    match lock_at(&crate::paths::runtime_dir().join("lasso.lock")) {
        Ok(Some(f)) => {
            let _ = LOCK.set(f);
            true
        }
        Ok(None) => false,
        // can't even make the lock file: go on (the socket check still stops most doubles)
        Err(()) => true,
    }
}

/// The exclusive lock on that file: Some(file) = ours (keep the file open), None = someone else holds it
#[cfg(unix)]
fn lock_at(p: &std::path::Path) -> Result<Option<std::fs::File>, ()> {
    use std::os::fd::AsRawFd;
    let f = std::fs::OpenOptions::new().create(true).write(true).truncate(false).open(p).map_err(|_| ())?;
    if unsafe { libc::flock(f.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
        return Ok(None);
    }
    Ok(Some(f))
}

#[cfg(all(test, unix))]
mod tests {
    #[test]
    fn only_one_lasso_holds_the_lock() {
        // B5: two Lassos started at the same moment - only one may become the running one
        let p = std::env::temp_dir().join(format!("lasso-lock-test-{}", std::process::id()));
        let first = super::lock_at(&p).unwrap();
        assert!(first.is_some());
        assert!(super::lock_at(&p).unwrap().is_none(), "a second start must not get the lock");
        drop(first);
        assert!(super::lock_at(&p).unwrap().is_some(), "free again once the first one quits");
        let _ = std::fs::remove_file(&p);
    }
}

#[cfg(unix)]
pub fn serve(on_cmd: impl Fn(String) + Send + 'static) -> Result<(), String> {
    use std::os::unix::net::UnixListener;
    let p = crate::paths::socket_path();
    if p.exists() {
        // a socket left over from a crash (nobody answers): take it over
        if std::os::unix::net::UnixStream::connect(&p).is_ok() {
            return Err("Lasso is already running".into());
        }
        let _ = std::fs::remove_file(&p);
    }
    let l = UnixListener::bind(&p).map_err(|e| e.to_string())?;
    std::thread::Builder::new()
        .name("ipc".into())
        .spawn(move || {
            for c in l.incoming().flatten() {
                let mut line = String::new();
                let mut r = BufReader::new(&c);
                if r.read_line(&mut line).is_ok() {
                    let cmd = line.trim().to_string();
                    let _ = (&c).write_all(b"ok\n");
                    on_cmd(cmd);
                }
            }
        })
        .map_err(|e| e.to_string())?;
    Ok(())
}

#[cfg(windows)]
fn port_file() -> std::path::PathBuf {
    crate::paths::config_dir().join("lasso.port")
}

#[cfg(windows)]
pub fn send(cmd: &str) -> Result<String, String> {
    let s = std::fs::read_to_string(port_file()).map_err(|e| e.to_string())?;
    let (port, token) = s.trim().split_once(' ').ok_or("bad port file")?;
    let mut c = std::net::TcpStream::connect(("127.0.0.1", port.parse::<u16>().map_err(|e| e.to_string())?)).map_err(|e| e.to_string())?;
    let _ = c.set_read_timeout(Some(std::time::Duration::from_secs(5)));
    c.write_all(format!("{token} {cmd}\n").as_bytes()).map_err(|e| e.to_string())?;
    let mut line = String::new();
    BufReader::new(c).read_line(&mut line).map_err(|e| e.to_string())?;
    Ok(line.trim().to_string())
}

#[cfg(windows)]
pub fn serve(on_cmd: impl Fn(String) + Send + 'static) -> Result<(), String> {
    if send("ping").is_ok() {
        return Err("Lasso is already running".into());
    }
    let l = std::net::TcpListener::bind(("127.0.0.1", 0)).map_err(|e| e.to_string())?;
    let port = l.local_addr().map_err(|e| e.to_string())?.port();
    let token = format!("{:x}{:x}", std::process::id(), std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0));
    std::fs::write(port_file(), format!("{port} {token}")).map_err(|e| e.to_string())?;
    std::thread::spawn(move || {
        for c in l.incoming().flatten() {
            let mut line = String::new();
            if BufReader::new(&c).read_line(&mut line).is_ok() {
                if let Some((t, cmd)) = line.trim().split_once(' ') {
                    if t == token {
                        let _ = (&c).write_all(b"ok\n");
                        on_cmd(cmd.to_string());
                    }
                }
            }
        }
    });
    Ok(())
}
