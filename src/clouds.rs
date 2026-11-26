//! Your own places for the backup (26 Nov 2026, the user: "look what Echo does"): like Echo's own clouds - Google
//! Drive, pCloud, MEGA, WebDAV, SFTP, S3 and a folder / NAS, as many as you like, through rclone. Echo Cloud stays one
//! more place (share/backup.rs talks to it directly); share links stay on Echo Cloud only (sonela.app/s/ pages: the
//! picture shows in Discord, expiry / view-once / private-info check / view counts work, no account needed).
//!
//! - rclone: Echo's pinned 1.75.2, checked against its SHA-256, downloaded on first use into Lasso's data folder
//!   (`~/.local/share/lasso/rclone`, `%LOCALAPPDATA%\Lasso\rclone`); the Flatpak has /app/bin/rclone; tests point
//!   `LASSO_RCLONE` at a copy (a test copy never downloads).
//! - The logins: Lasso's own rclone config (`<config>/rclone.conf`, readable only by you - like rclone itself keeps
//!   them), remotes `place_<id>`. The list: `<config>/places.json` (id, kind, label, root folder, on/off).
//! - Google Drive asks only for `drive.file` (Lasso sees only the files it made) with Echo's Google login (its secret
//!   comes in at build time - build.rs); without the secret rclone's own login is used.
//! - In the place: `<root>/Lasso/Captures/<yyyy>/<mm>/<id>.png` and `<root>/Lasso/Games/<game>/<file>`.

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

const RCLONE_VER: &str = "1.75.2";
#[cfg(target_os = "linux")]
const RCLONE_ZIP: (&str, &str) = (
    "https://github.com/rclone/rclone/releases/download/v1.75.2/rclone-v1.75.2-linux-amd64.zip",
    "349ac8fba6ff65d6247043f1750cdcb518ec5d500ef91463a10d37c0ccdf3702",
);
#[cfg(windows)]
const RCLONE_ZIP: (&str, &str) = (
    "https://github.com/rclone/rclone/releases/download/v1.75.2/rclone-v1.75.2-windows-amd64.zip",
    "8a9db2bad1dc79a65b3348fd857069598e2aab7cf4d118b77b1df23055aff818",
);
#[cfg(windows)]
const EXE: &str = "rclone.exe";
#[cfg(not(windows))]
const EXE: &str = "rclone";

/// The page the browser shows after the Google Drive / pCloud sign-in (rclone's own says "go back to rclone"). Go
/// template; ONE line (rclone refuses values with \n). Not saved in rclone.conf.
const OAUTH_PAGE: &str = r##"<!doctype html><html lang="en"><head><meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1"><title>{{ if .OK }}Connected to Lasso{{ else }}Lasso couldn't connect{{ end }}</title><style>body{margin:0;min-height:100vh;display:grid;place-items:center;background:#0e0f13;color:#eceef2;font:15px/1.55 system-ui,"Segoe UI",Roboto,Arial,sans-serif;padding:24px}.c{max-width:420px;background:#16181d;border:1px solid #262a31;border-radius:18px;padding:32px 28px;text-align:center}h1{font-size:22px;margin:0 0 8px}p{color:#a0a6b1;margin:0 0 6px}.ok{color:#14a38b;font-size:30px}.bad{color:#e5534b;font-size:30px}.err{margin-top:14px;text-align:left;background:#0e0f13;border:1px solid #262a31;border-radius:10px;padding:10px 12px;font-size:12.5px;word-break:break-word}</style></head><body><div class="c">{{ if .OK }}<div class="ok">&#10003;</div><h1>You're connected</h1><p>Your cloud is now linked to Lasso. You can close this tab - Lasso finishes by itself.</p>{{ else }}<div class="bad">!</div><h1>That didn't work</h1><p>Close this tab, go back to Lasso and try again.</p><div class="err">{{ .Name }}{{ if .Description }}<br>{{ .Description }}{{ end }}</div>{{ end }}<p style="font-size:12.5px;color:#6b7280;margin-top:16px">Lasso · sonela.app</p></div></body></html>"##;

/// Google Drive: Echo's Google login (project "Echo", desktop client "Echo app (rclone)") - the user, 26 Nov 2026:
/// "Reuse Echo's". A desktop app's id and "secret" aren't secret (Google says so); the secret still comes in at build
/// time (build.rs) because GitHub's push protection blocks it in a public repo. Empty secret = rclone's own login.
const GDRIVE_CLIENT_ID: &str = "38239043070-6q95iidaueoc8bij14u1fpj8g1qdjs9p.apps.googleusercontent.com";

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(default)]
pub struct Place {
    pub id: u32,
    /// "gdrive", "pcloud", "mega", "webdav", "sftp", "s3", "folder"
    pub kind: String,
    /// what the row shows under the name: the email / server / folder
    pub label: String,
    /// the folder inside the cloud that holds Lasso/ ("" = the top)
    pub root: String,
    /// the backup goes here (a switch per place)
    pub on: bool,
}

impl Default for Place {
    fn default() -> Self {
        Place { id: 0, kind: String::new(), label: String::new(), root: String::new(), on: true }
    }
}

/// The kinds "Add a place" offers, in this order: (kind, name)
pub const KINDS: [(&str, &str); 7] = [
    ("gdrive", "Google Drive"),
    ("pcloud", "pCloud"),
    ("mega", "MEGA"),
    ("webdav", "WebDAV server"),
    ("sftp", "SFTP server"),
    ("s3", "S3 storage"),
    ("folder", "Folder / NAS"),
];

pub fn kind_name(k: &str) -> &'static str {
    KINDS.iter().find(|(id, _)| *id == k).map(|(_, n)| *n).unwrap_or("Cloud")
}

/// One field of the "Add a place" form: (key, label, hint, hidden like a password)
pub type Field = (&'static str, &'static str, &'static str, bool);

/// What a kind asks for (empty = the browser sign-in, or the folder picker)
pub fn fields(kind: &str) -> Vec<Field> {
    match kind {
        "mega" => vec![("user", "MEGA email", "the email you sign in to MEGA with", false), ("pass", "MEGA password", "", true)],
        "webdav" => vec![
            ("url", "Server address", "https://cloud.example.com/remote.php/dav/files/you", false),
            ("user", "User name", "", false),
            ("pass", "Password", "", true),
        ],
        "sftp" => vec![
            ("host", "Server address", "nas.local or 192.168.1.20", false),
            ("user", "User name", "", false),
            ("pass", "Password (empty = your SSH key)", "", true),
            ("port", "Port", "22", false),
            ("path", "Folder on the server (optional)", "/volume1/backup", false),
        ],
        "s3" => vec![
            ("url", "Endpoint address", "https://s3.example.com", false),
            ("bucket", "Bucket", "", false),
            ("user", "Access key", "", false),
            ("pass", "Secret key", "", true),
        ],
        _ => vec![],
    }
}

/// The two files (a struct so the tests use scratch ones)
#[derive(Clone)]
pub struct Rc {
    pub bin: PathBuf,
    pub conf: PathBuf,
    pub list: PathBuf,
}

impl Rc {
    /// Lasso's own: the rclone program (downloaded if needed - a blocking call), its config, the list of places
    pub fn app() -> Result<Rc, String> {
        let dir = crate::paths::config_dir();
        Ok(Rc { bin: ensure_rclone()?, conf: dir.join("rclone.conf"), list: dir.join("places.json") })
    }

    /// Only the list (no rclone needed): what Settings shows
    pub fn list_only() -> Rc {
        let dir = crate::paths::config_dir();
        Rc { bin: PathBuf::new(), conf: dir.join("rclone.conf"), list: dir.join("places.json") }
    }

    fn cmd(&self) -> Command {
        let mut c = Command::new(&self.bin);
        c.arg("--config").arg(&self.conf).arg("--ask-password=false");
        c.stdin(Stdio::null());
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            c.creation_flags(0x0800_0000); // CREATE_NO_WINDOW: no black window flashing up
        }
        c
    }

    /// rclone ... -> its output; Err = its last error line. At most `secs` (a browser sign-in nobody finishes, a
    /// server that doesn't answer: rclone is stopped).
    pub fn run(&self, args: &[&str], secs: u64) -> Result<String, String> {
        let mut ch = self.cmd().args(args).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn().map_err(|e| format!("rclone didn't start ({e})"))?;
        let (mut out, mut err) = (ch.stdout.take(), ch.stderr.take());
        let o = std::thread::spawn(move || {
            let mut s = String::new();
            if let Some(o) = out.as_mut() {
                let _ = std::io::Read::read_to_string(o, &mut s);
            }
            s
        });
        let e = std::thread::spawn(move || {
            let mut s = String::new();
            if let Some(e) = err.as_mut() {
                let _ = std::io::Read::read_to_string(e, &mut s);
            }
            s
        });
        let t0 = Instant::now();
        let st = loop {
            if let Ok(Some(st)) = ch.try_wait() {
                break Some(st);
            }
            if t0.elapsed() > Duration::from_secs(secs) {
                let _ = ch.kill();
                let _ = ch.wait();
                break None;
            }
            std::thread::sleep(Duration::from_millis(100));
        };
        let (out, err) = (o.join().unwrap_or_default(), e.join().unwrap_or_default());
        match st {
            None => Err("it took too long - try again".into()),
            Some(s) if s.success() => Ok(out),
            Some(_) => {
                let last = err.trim().lines().last().unwrap_or("rclone failed").trim().to_string();
                // "2026/11/26 05:01:02 CRITICAL: Failed to create file system ...: <why>" -> <why>
                let why = last.rsplit(": ").next().unwrap_or(&last).to_string();
                crate::log::line("clouds", &format!("rclone {} failed: {}", args.first().unwrap_or(&""), crate::util::scrub_home(&last)));
                Err(if why.is_empty() { last } else { why })
            }
        }
    }

    pub fn places(&self) -> Vec<Place> {
        std::fs::read_to_string(&self.list).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default()
    }

    fn save(&self, v: &[Place]) -> Result<(), String> {
        let tmp = self.list.with_extension("json.tmp");
        std::fs::write(&tmp, serde_json::to_string_pretty(v).unwrap_or_default()).map_err(|e| e.to_string())?;
        std::fs::rename(&tmp, &self.list).map_err(|e| e.to_string())
    }

    /// "place_3:<root>/<sub>"
    pub fn path(&self, p: &Place, sub: &str) -> String {
        let root = p.root.trim_end_matches(['/', '\\']);
        if root.is_empty() {
            format!("place_{}:{sub}", p.id)
        } else {
            format!("place_{}:{root}/{sub}", p.id)
        }
    }

    /// Add a place (blocking: Google Drive / pCloud wait for the browser sign-in, up to 5 minutes). `vals` = the form
    /// (key -> value). The place is checked by making its Lasso folder.
    pub fn connect(&self, kind: &str, vals: &[(String, String)]) -> Result<Place, String> {
        let v = |k: &str| vals.iter().find(|(a, _)| a == k).map(|(_, b)| b.trim().to_string()).unwrap_or_default();
        // the password as typed (spaces count)
        let pw = vals.iter().find(|(a, _)| a == "pass").map(|(_, b)| b.clone()).unwrap_or_default();
        let need = |val: &str, what: &str| if val.is_empty() { Err(format!("Enter the {what}")) } else { Ok(()) };
        let mut list = self.places();
        let id = list.iter().map(|p| p.id).max().unwrap_or(0) + 1;
        let name = format!("place_{id}");
        let (mut label, mut root) = (v("user"), String::new());
        // a remote left over from an attempt that stopped half-way
        let _ = self.run(&["config", "delete", &name], 20);
        let made = match kind {
            "gdrive" => {
                let (cid, sec) = (GDRIVE_CLIENT_ID, env!("LASSO_GDRIVE_SECRET"));
                label = "Google Drive".into();
                let mut a = vec!["config", "create", name.as_str(), "drive", "scope", "drive.file", "config_template", OAUTH_PAGE];
                if !cid.is_empty() && !sec.is_empty() {
                    a.extend(["client_id", cid, "client_secret", sec]);
                }
                self.run(&a, 300)
            }
            "pcloud" => {
                label = "pCloud".into();
                self.run(&["config", "create", &name, "pcloud", "config_template", OAUTH_PAGE], 300)
            }
            "mega" => {
                need(&v("user"), "MEGA email")?;
                need(&pw, "MEGA password")?;
                self.run(&["config", "create", &name, "mega", "user", &v("user"), "pass", &pw, "--obscure"], 60)
            }
            "webdav" => {
                let url = v("url");
                need(&url, "server address")?;
                label = url.clone();
                self.run(&["config", "create", &name, "webdav", "url", &url, "vendor", "other", "user", &v("user"), "pass", &pw, "--obscure"], 60)
            }
            "sftp" => {
                let host = v("host");
                need(&host, "server address")?;
                need(&v("user"), "user name")?;
                let port = if v("port").is_empty() { "22".to_string() } else { v("port") };
                label = format!("{}@{host}", v("user"));
                root = v("path");
                let u = v("user");
                let mut a = vec!["config", "create", name.as_str(), "sftp", "host", host.as_str(), "user", u.as_str(), "port", port.as_str()];
                if !pw.is_empty() {
                    a.extend(["pass", pw.as_str()]);
                }
                a.push("--obscure");
                self.run(&a, 60)
            }
            "s3" => {
                let (ep, bucket) = (v("url"), v("bucket"));
                need(&ep, "endpoint address")?;
                need(&bucket, "bucket name")?;
                need(&v("user"), "access key")?;
                need(&pw, "secret key")?;
                label = format!("{bucket} @ {ep}");
                root = bucket;
                self.run(&["config", "create", &name, "s3", "provider", "Other", "endpoint", &ep, "access_key_id", &v("user"), "secret_access_key", &pw], 60)
            }
            "folder" => {
                let p = v("path");
                need(&p, "folder")?;
                if !Path::new(&p).is_dir() {
                    return Err("That folder doesn't exist (is the drive or network share connected?)".into());
                }
                label = p.clone();
                root = p;
                self.run(&["config", "create", &name, "local"], 30)
            }
            _ => return Err("Unknown kind of place".into()),
        };
        if let Err(e) = made {
            let _ = self.run(&["config", "delete", &name], 20);
            return Err(e);
        }
        private_file(&self.conf);
        if label.is_empty() {
            label = kind_name(kind).to_string();
        }
        let p = Place { id, kind: kind.into(), label, root, on: true };
        if let Err(e) = self.run(&["mkdir", &self.path(&p, "Lasso")], 60) {
            let _ = self.run(&["config", "delete", &name], 20);
            return Err(format!("Signed in, but the place didn't answer: {e}"));
        }
        list.push(p.clone());
        self.save(&list)?;
        crate::log::line("clouds", &format!("added {} as {name}", kind_name(kind)));
        Ok(p)
    }

    /// Stop using a place: its login goes, the pictures already there stay
    pub fn remove(&self, id: u32) -> Result<(), String> {
        let mut list = self.places();
        list.retain(|p| p.id != id);
        self.save(&list)?;
        let _ = self.run(&["config", "delete", &format!("place_{id}")], 20);
        crate::log::line("clouds", &format!("removed place_{id}"));
        Ok(())
    }

/// Without rclone (it was removed by hand): just take it off the list
    pub fn remove_from_list(&self, id: u32) -> Result<(), String> {
        let mut list = self.places();
        list.retain(|p| p.id != id);
        self.save(&list)
    }

        pub fn set_on(&self, id: u32, on: bool) -> Result<(), String> {
        let mut list = self.places();
        for p in list.iter_mut().filter(|p| p.id == id) {
            p.on = on;
        }
        self.save(&list)
    }

    /// One file into the place (`sub` = "Lasso/Captures/2026/11/x.png")
    pub fn put(&self, p: &Place, local: &Path, sub: &str) -> Result<(), String> {
        self.run(&["copyto", &local.to_string_lossy(), &self.path(p, sub), "--retries", "2", "--low-level-retries", "3"], 600).map(|_| ())
    }
}

/// The config holds logins: only this user may read it (Windows: %APPDATA% is the user's own already)
fn private_file(p: &Path) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(p, std::fs::Permissions::from_mode(0o600));
    }
    #[cfg(not(unix))]
    let _ = p;
}

/// Where Lasso keeps its own rclone (next to the Library: ~/.local/share/lasso/rclone, %LOCALAPPDATA%\Lasso\rclone)
fn rclone_dir() -> PathBuf {
    crate::paths::library_dir().parent().map(|d| d.join("rclone")).unwrap_or_else(|| crate::paths::config_dir().join("rclone"))
}

/// Is rclone here already (no download needed)?
pub fn rclone_ready() -> bool {
    if std::env::var_os("LASSO_RCLONE").is_some() || std::env::var_os("FLATPAK_ID").is_some() {
        return true;
    }
    rclone_dir().join(format!("v{RCLONE_VER}")).join(EXE).is_file()
}

/// rclone, downloaded once (blocking). Tests: LASSO_RCLONE; the Flatpak: /app/bin/rclone.
pub fn ensure_rclone() -> Result<PathBuf, String> {
    if let Some(p) = std::env::var_os("LASSO_RCLONE").map(PathBuf::from) {
        return if p.is_file() { Ok(p) } else { Err("LASSO_RCLONE isn't a file".into()) };
    }
    #[cfg(target_os = "linux")]
    if std::env::var_os("FLATPAK_ID").is_some() || Path::new("/.flatpak-info").exists() {
        return Ok(PathBuf::from("/app/bin/rclone"));
    }
    if std::env::var_os("LASSO_HOME").is_some() {
        return Err("test copies don't download rclone (set LASSO_RCLONE)".into());
    }
    let dir = rclone_dir();
    let exe = dir.join(format!("v{RCLONE_VER}")).join(EXE);
    if exe.is_file() {
        return Ok(exe);
    }
    #[cfg(any(target_os = "linux", windows))]
    {
        std::fs::create_dir_all(exe.parent().unwrap_or(&dir)).map_err(|e| e.to_string())?;
        crate::log::line("clouds", &format!("downloading rclone {RCLONE_VER}"));
        let (url, want) = RCLONE_ZIP;
        let (code, zip) = crate::update::http_get(url).map_err(|e| format!("Couldn't download rclone: {e}"))?;
        if code != 200 {
            return Err(format!("Couldn't download rclone (the server answered {code})"));
        }
        let got = sha256_hex(&zip);
        if !got.eq_ignore_ascii_case(want) {
            crate::log::line("clouds", "rclone download had the wrong fingerprint - not used");
            return Err("The rclone download didn't match its fingerprint - not used".into());
        }
        let bin = exe_from_zip(&zip)?;
        let tmp = exe.with_extension("part");
        std::fs::write(&tmp, &bin).map_err(|e| e.to_string())?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o755));
        }
        std::fs::rename(&tmp, &exe).map_err(|e| e.to_string())?;
        // older versions: gone
        if let Ok(rd) = std::fs::read_dir(&dir) {
            for e in rd.flatten() {
                if e.path() != exe.parent().unwrap_or(&dir) && e.file_name().to_string_lossy().starts_with('v') {
                    let _ = std::fs::remove_dir_all(e.path());
                }
            }
        }
        return Ok(exe);
    }
    #[allow(unreachable_code)]
    Err("rclone isn't available on this system".into())
}

pub fn sha256_hex(b: &[u8]) -> String {
    ring::digest::digest(&ring::digest::SHA256, b).as_ref().iter().map(|x| format!("{x:02x}")).collect()
}

/// rclone's release zip -> the program inside it (rclone-v1.75.2-<os>-amd64/rclone[.exe])
fn exe_from_zip(zip: &[u8]) -> Result<Vec<u8>, String> {
    let mut z = zip::ZipArchive::new(std::io::Cursor::new(zip)).map_err(|e| format!("bad rclone download ({e})"))?;
    for i in 0..z.len() {
        let mut e = z.by_index(i).map_err(|e| e.to_string())?;
        let name = e.name().map(|n| n.to_string()).unwrap_or_default();
        if name.rsplit('/').next() == Some(EXE) && e.is_file() {
            let mut out = Vec::with_capacity(e.size() as usize);
            std::io::Read::read_to_end(&mut e, &mut out).map_err(|e| e.to_string())?;
            return Ok(out);
        }
    }
    Err(format!("{EXE} wasn't in the rclone download"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fields_and_names() {
        assert!(fields("gdrive").is_empty());
        assert_eq!(fields("mega").len(), 2);
        assert!(fields("s3").iter().any(|f| f.0 == "bucket"));
        assert_eq!(kind_name("folder"), "Folder / NAS");
        let r = Rc { bin: PathBuf::new(), conf: PathBuf::new(), list: PathBuf::new() };
        let p = Place { id: 3, kind: "sftp".into(), root: "/volume1/backup/".into(), ..Default::default() };
        assert_eq!(r.path(&p, "Lasso/x.png"), "place_3:/volume1/backup/Lasso/x.png");
        let p = Place { id: 4, ..Default::default() };
        assert_eq!(r.path(&p, "Lasso"), "place_4:Lasso");
    }

    #[test]
    fn takes_only_the_program_from_the_zip() {
        use std::io::Write;
        let mut buf = std::io::Cursor::new(Vec::new());
        {
            let mut z = zip::ZipWriter::new(&mut buf);
            let o = zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored);
            z.start_file("rclone-v1.75.2-x-amd64/README.txt", o).unwrap();
            z.write_all(b"readme").unwrap();
            z.start_file(format!("rclone-v1.75.2-x-amd64/{EXE}"), o).unwrap();
            z.write_all(b"the program").unwrap();
            z.finish().unwrap();
        }
        assert_eq!(exe_from_zip(buf.get_ref()).unwrap(), b"the program");
        assert!(exe_from_zip(b"not a zip").is_err());
    }

    #[test]
    fn sha256_known() {
        assert_eq!(sha256_hex(b"abc"), "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad");
    }

    /// The whole round trip with a real rclone (only when LASSO_RCLONE points at one): add a folder place, put a
    /// file into it, remove the place - the file stays
    #[test]
    fn folder_place_round_trip() {
        let Some(bin) = std::env::var_os("LASSO_RCLONE").map(PathBuf::from).filter(|p| p.is_file()) else {
            eprintln!("LASSO_RCLONE not set - skipped");
            return;
        };
        let d = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("target").join("clouds-test");
        let _ = std::fs::remove_dir_all(&d);
        let target = d.join("nas");
        std::fs::create_dir_all(&target).unwrap();
        let r = Rc { bin, conf: d.join("rclone.conf"), list: d.join("places.json") };
        assert!(r.connect("folder", &[("path".into(), d.join("missing").to_string_lossy().to_string())]).is_err());
        let p = r.connect("folder", &[("path".into(), target.to_string_lossy().to_string())]).unwrap();
        assert!(target.join("Lasso").is_dir());
        let src = d.join("pic.png");
        std::fs::write(&src, b"png").unwrap();
        r.put(&p, &src, "Lasso/Captures/2026/11/a.png").unwrap();
        assert_eq!(std::fs::read(target.join("Lasso/Captures/2026/11/a.png")).unwrap(), b"png");
        assert_eq!(r.places().len(), 1);
        r.remove(p.id).unwrap();
        assert!(r.places().is_empty());
        assert!(target.join("Lasso/Captures/2026/11/a.png").is_file());
        let _ = std::fs::remove_dir_all(&d);
    }
}
