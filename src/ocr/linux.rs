//! Tesseract on Linux: the `tesseract` program when it's installed (own process = a crash can't take Lasso down,
//! and a time limit), else the Tesseract library through dlopen (KDE systems often have it already for Spectacle's
//! OCR, without the program), else a short message with this distro's install command.

use super::prep::{self, Gray};
use super::run::{self, Fail};
use super::Word;
use crate::shapes::R;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

pub const OPTS: prep::Opts = prep::Opts { max_pixels: 12e6, max_side: 20000, max_scale: 3.0, pad: 16 };

pub fn recognize(img: &Gray) -> Result<(Vec<Word>, &'static str), String> {
    let want = locale_lang();
    if let Some(exe) = find_cli() {
        let langs = pick_langs(&cli_langs(&exe), want).ok_or_else(no_lang_msg)?;
        let png = prep::png(img);
        let mut ws = cli_read(&exe, &png, &langs, 3)?;
        if ws.is_empty() {
            // one word / one line: "automatic" sometimes finds nothing, "one block of text" does
            ws = cli_read(&exe, &png, &langs, 6)?;
        }
        return Ok((ws, "tesseract"));
    }
    if let Some(api) = lib::load() {
        let (dir, have) = tessdata().unwrap_or_default();
        let langs = pick_langs(&have, want).ok_or_else(no_lang_msg)?;
        let dir = if dir.as_os_str().is_empty() { None } else { Some(dir) };
        return lib::read(api, img, dir.as_deref(), &langs).map(|w| (w, "Tesseract library"));
    }
    Err(missing_msg())
}

// ---------- the program ----------

fn find_cli() -> Option<PathBuf> {
    let mut dirs: Vec<PathBuf> = std::env::var_os("PATH").map(|p| std::env::split_paths(&p).collect()).unwrap_or_default();
    for d in ["/usr/bin", "/usr/local/bin", "/bin", "/run/current-system/sw/bin", "/var/lib/snapd/snap/bin", "/snap/bin"] {
        dirs.push(d.into());
    }
    if let Some(h) = std::env::var_os("HOME") {
        dirs.push(Path::new(&h).join(".nix-profile/bin"));
    }
    find_in(&dirs, "tesseract")
}

fn find_in(dirs: &[PathBuf], name: &str) -> Option<PathBuf> {
    use std::os::unix::fs::PermissionsExt;
    dirs.iter()
        .map(|d| d.join(name))
        .find(|p| std::fs::metadata(p).map(|m| m.is_file() && m.permissions().mode() & 0o111 != 0).unwrap_or(false))
}

/// The host's own environment for the program: inside an AppImage, LD_LIBRARY_PATH points at Lasso's bundled
/// libraries, which can break the system's tesseract.
fn host_env(cmd: &mut Command) {
    if std::env::var_os("OMP_THREAD_LIMIT").is_none() {
        // OpenMP's spinning threads make one small picture slower, not faster
        cmd.env("OMP_THREAD_LIMIT", "1");
    }
    if let (Ok(dir), Ok(ld)) = (std::env::var("APPDIR"), std::env::var("LD_LIBRARY_PATH")) {
        match strip_dir(&ld, &dir) {
            Some(v) => cmd.env("LD_LIBRARY_PATH", v),
            None => cmd.env_remove("LD_LIBRARY_PATH"),
        };
    }
}

fn strip_dir(list: &str, dir: &str) -> Option<String> {
    let keep: Vec<&str> = list.split(':').filter(|p| !p.is_empty() && (dir.is_empty() || !p.starts_with(dir))).collect();
    (!keep.is_empty()).then(|| keep.join(":"))
}

fn cli_langs(exe: &Path) -> Vec<String> {
    let mut c = Command::new(exe);
    c.arg("--list-langs");
    host_env(&mut c);
    match run::run(c, None, Duration::from_secs(10)) {
        Ok(d) => parse_list_langs(&String::from_utf8_lossy(&d.out)),
        Err(_) => vec![],
    }
}

fn parse_list_langs(s: &str) -> Vec<String> {
    s.lines().map(str::trim).filter(|l| !l.is_empty() && !l.contains(' ') && !l.ends_with(':')).map(String::from).collect()
}

fn cli_read(exe: &Path, png: &[u8], langs: &str, psm: u32) -> Result<Vec<Word>, String> {
    let mut c = Command::new(exe);
    c.args(["stdin", "stdout", "-l", langs, "--psm", &psm.to_string(), "tsv"]);
    host_env(&mut c);
    match run::run(c, Some(png.to_vec()), Duration::from_secs(40)) {
        Err(Fail::NotFound) => Err(missing_msg()),
        Err(Fail::TimedOut) => Err(super::TOO_LONG.into()),
        Err(Fail::Io(e)) => Err(format!("Tesseract: {e}")),
        Ok(d) if !d.ok => {
            let why = d.reason();
            if why.contains("Failed loading language") || why.contains("Error opening data file") {
                Err(no_lang_msg())
            } else {
                Err(format!("Tesseract: {why}"))
            }
        }
        Ok(d) => Ok(parse_tsv(&String::from_utf8_lossy(&d.out))),
    }
}

/// Tesseract's TSV: level page block par line word left top width height conf text. Level 5 = a word. (The program
/// prints a header line, the library doesn't.)
pub fn parse_tsv(s: &str) -> Vec<Word> {
    let mut out = vec![];
    for l in s.lines() {
        let f: Vec<&str> = l.splitn(12, '\t').collect();
        if f.len() < 12 || f[0].trim() != "5" {
            continue;
        }
        let n = |i: usize| f[i].trim().parse::<f32>().unwrap_or(0.0);
        let u = |i: usize| f[i].trim().parse::<u32>().unwrap_or(0) & 0x3ff;
        let text = f[11].trim().to_string();
        if text.is_empty() {
            continue;
        }
        out.push(Word {
            text,
            r: R::new(n(6), n(7), n(8), n(9)),
            line: (u(2) << 20) | (u(3) << 10) | u(4),
            para: (u(2) << 10) | u(3),
            conf: n(10),
        });
    }
    out
}

// ---------- languages ----------

/// English + the desktop's own language when both are installed (more languages = slower and more mix-ups).
/// `have` empty = unknown, just try English. None = Tesseract has no languages at all.
fn pick_langs(have: &[String], want: Option<&str>) -> Option<String> {
    if have.is_empty() {
        return Some("eng".into());
    }
    let real: Vec<&str> = have.iter().map(String::as_str).filter(|l| !matches!(*l, "osd" | "equ" | "snum") && !l.contains('/')).collect();
    let first = *real.first()?;
    let mut l = vec![];
    if real.contains(&"eng") {
        l.push("eng");
    }
    if let Some(w) = want {
        if w != "eng" && real.contains(&w) {
            l.push(w);
        }
    }
    if l.is_empty() {
        l.push(first);
    }
    Some(l.join("+"))
}

fn locale_lang() -> Option<&'static str> {
    for k in ["LC_ALL", "LC_MESSAGES", "LANG"] {
        if let Ok(v) = std::env::var(k) {
            if !v.is_empty() && v != "C" && v != "POSIX" && !v.starts_with("C.") {
                return tess_lang(&v);
            }
        }
    }
    None
}

/// "da_DK.UTF-8" -> "dan" (Tesseract's language names)
fn tess_lang(loc: &str) -> Option<&'static str> {
    let l = loc.split(['.', '@']).next()?;
    let (lang, region) = l.split_once('_').unwrap_or((l, ""));
    Some(match lang {
        "en" => "eng",
        "da" => "dan",
        "de" => "deu",
        "fr" => "fra",
        "es" => "spa",
        "it" => "ita",
        "nl" => "nld",
        "sv" => "swe",
        "nb" | "nn" | "no" => "nor",
        "fi" => "fin",
        "is" => "isl",
        "pl" => "pol",
        "pt" => "por",
        "ru" => "rus",
        "uk" => "ukr",
        "cs" => "ces",
        "sk" => "slk",
        "tr" => "tur",
        "hu" => "hun",
        "ro" => "ron",
        "el" => "ell",
        "bg" => "bul",
        "hr" => "hrv",
        "sr" => "srp",
        "sl" => "slv",
        "et" => "est",
        "lv" => "lav",
        "lt" => "lit",
        "ja" => "jpn",
        "ko" => "kor",
        "zh" if matches!(region, "TW" | "HK" | "MO") => "chi_tra",
        "zh" => "chi_sim",
        "ar" => "ara",
        "he" => "heb",
        "hi" => "hin",
        "th" => "tha",
        "vi" => "vie",
        "id" => "ind",
        _ => return None,
    })
}

/// The installed language files (for the library: the program lists them itself)
fn tessdata() -> Option<(PathBuf, Vec<String>)> {
    let mut dirs: Vec<PathBuf> = vec![];
    if let Some(p) = std::env::var_os("TESSDATA_PREFIX") {
        dirs.push(Path::new(&p).to_path_buf());
        dirs.push(Path::new(&p).join("tessdata"));
    }
    for d in [
        "/usr/share/tesseract/tessdata",
        "/usr/share/tesseract-ocr/5/tessdata",
        "/usr/share/tesseract-ocr/4.00/tessdata",
        "/usr/share/tesseract-ocr/tessdata",
        "/usr/share/tessdata",
        "/usr/local/share/tessdata",
        "/usr/local/share/tesseract/tessdata",
    ] {
        dirs.push(d.into());
    }
    for d in dirs {
        let Ok(rd) = std::fs::read_dir(&d) else { continue };
        let mut l: Vec<String> =
            rd.flatten().filter_map(|e| e.file_name().to_str().and_then(|n| n.strip_suffix(".traineddata")).map(String::from)).collect();
        if !l.is_empty() {
            l.sort();
            return Some((d, l));
        }
    }
    None
}

// ---------- install messages ----------

#[derive(Clone, Copy, Debug, PartialEq)]
enum Pm {
    Dnf,
    Ostree,
    Apt,
    Pacman,
    Zypper,
}

fn os_ids(os_release: &str) -> Vec<String> {
    let val = |k: &str| {
        os_release.lines().find_map(|l| l.strip_prefix(k).and_then(|r| r.strip_prefix('='))).map(|v| v.trim().trim_matches(['"', '\'']).to_lowercase())
    };
    let mut ids = vec![];
    if let Some(i) = val("ID") {
        ids.push(i);
    }
    if let Some(l) = val("ID_LIKE") {
        ids.extend(l.split_whitespace().map(String::from));
    }
    ids
}

fn pm_for(ids: &[String], ostree: bool) -> Option<Pm> {
    for id in ids {
        let pm = match id.as_str() {
            "fedora" | "nobara" | "rhel" | "centos" | "rocky" | "almalinux" | "ultramarine" | "bazzite" => Pm::Dnf,
            "debian" | "ubuntu" | "linuxmint" | "pop" | "elementary" | "zorin" | "kali" | "neon" => Pm::Apt,
            "arch" | "manjaro" | "endeavouros" | "cachyos" | "garuda" => Pm::Pacman,
            s if s.starts_with("opensuse") || s == "suse" || s == "sles" => Pm::Zypper,
            _ => continue,
        };
        return Some(if pm == Pm::Dnf && ostree { Pm::Ostree } else { pm });
    }
    None
}

fn this_pm() -> Option<Pm> {
    let s = std::fs::read_to_string("/etc/os-release").or_else(|_| std::fs::read_to_string("/usr/lib/os-release")).unwrap_or_default();
    pm_for(&os_ids(&s), Path::new("/run/ostree-booted").exists())
}

fn missing_for(pm: Option<Pm>) -> String {
    let cmd = match pm {
        Some(Pm::Dnf) => "sudo dnf install tesseract",
        Some(Pm::Ostree) => "rpm-ostree install tesseract",
        Some(Pm::Apt) => "sudo apt install tesseract-ocr",
        Some(Pm::Pacman) => "sudo pacman -S tesseract tesseract-data-eng",
        Some(Pm::Zypper) => "sudo zypper install tesseract-ocr",
        None => return "Install Tesseract (package \"tesseract\" or \"tesseract-ocr\") to copy text".into(),
    };
    format!("Install Tesseract to copy text: {cmd}")
}

fn no_lang_for(pm: Option<Pm>) -> String {
    let cmd = match pm {
        Some(Pm::Dnf) => "sudo dnf install tesseract-langpack-eng",
        Some(Pm::Ostree) => "rpm-ostree install tesseract-langpack-eng",
        Some(Pm::Apt) => "sudo apt install tesseract-ocr-eng",
        Some(Pm::Pacman) => "sudo pacman -S tesseract-data-eng",
        Some(Pm::Zypper) => "sudo zypper install tesseract-ocr-traineddata-english",
        None => return "Tesseract has no languages: install its English data (tesseract-ocr-eng / tesseract-langpack-eng)".into(),
    };
    format!("Tesseract needs a language to copy text: {cmd}")
}

fn missing_msg() -> String {
    missing_for(this_pm())
}

fn no_lang_msg() -> String {
    no_lang_for(this_pm())
}

// ---------- the library ----------

mod lib {
    use super::super::Word;
    use super::{no_lang_msg, parse_tsv, Gray};
    use std::ffi::{c_char, c_int, c_void, CStr, CString};
    use std::path::Path;
    use std::sync::Mutex;

    type H = *mut c_void;

    /// Tesseract's C API (capi.h) - the same in versions 4 and 5
    pub struct Api {
        create: unsafe extern "C" fn() -> H,
        delete: unsafe extern "C" fn(H),
        init3: unsafe extern "C" fn(H, *const c_char, *const c_char) -> c_int,
        end: unsafe extern "C" fn(H),
        set_psm: unsafe extern "C" fn(H, c_int),
        set_var: unsafe extern "C" fn(H, *const c_char, *const c_char) -> c_int,
        set_image: unsafe extern "C" fn(H, *const u8, c_int, c_int, c_int, c_int),
        recognize: unsafe extern "C" fn(H, *mut c_void) -> c_int,
        tsv: unsafe extern "C" fn(H, c_int) -> *mut c_char,
        del_text: unsafe extern "C" fn(*const c_char),
    }

    /// Only a found library is kept (installing Tesseract while Lasso runs works without a restart)
    static LOADED: Mutex<Option<&'static Api>> = Mutex::new(None);
    static BUSY: Mutex<()> = Mutex::new(());

    pub fn load() -> Option<&'static Api> {
        let mut g = LOADED.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(a) = *g {
            return Some(a);
        }
        let a: &'static Api = Box::leak(Box::new(candidates().iter().find_map(|n| open(n))?));
        *g = Some(a);
        Some(a)
    }

    /// Debian / Arch: libtesseract.so.5; Fedora: libtesseract.so.5.5 (major.minor); else any libtesseract.so.* found.
    fn candidates() -> Vec<String> {
        let mut c: Vec<String> = ["libtesseract.so.5", "libtesseract.so"].iter().map(|s| s.to_string()).collect();
        for m in (0..=9).rev() {
            c.push(format!("libtesseract.so.5.{m}"));
        }
        c.push("libtesseract.so.4".into());
        for d in ["/usr/lib64", "/usr/lib", "/usr/lib/x86_64-linux-gnu", "/usr/lib/aarch64-linux-gnu", "/usr/local/lib", "/usr/local/lib64"] {
            if let Ok(rd) = std::fs::read_dir(d) {
                for e in rd.flatten() {
                    if e.file_name().to_str().is_some_and(|n| n.starts_with("libtesseract.so.")) {
                        c.push(e.path().to_string_lossy().into_owned());
                    }
                }
            }
        }
        c
    }

    fn open(name: &str) -> Option<Api> {
        let cn = CString::new(name).ok()?;
        // SAFETY: plain dlopen / dlsym; every symbol is checked before use and has the C API's signature.
        unsafe {
            let h = libc::dlopen(cn.as_ptr(), libc::RTLD_NOW | libc::RTLD_LOCAL);
            if h.is_null() {
                return None;
            }
            macro_rules! sym {
                ($n:literal) => {{
                    let p = libc::dlsym(h, concat!($n, "\0").as_ptr() as *const c_char);
                    if p.is_null() {
                        libc::dlclose(h);
                        return None;
                    }
                    std::mem::transmute_copy(&p)
                }};
            }
            Some(Api {
                create: sym!("TessBaseAPICreate"),
                delete: sym!("TessBaseAPIDelete"),
                init3: sym!("TessBaseAPIInit3"),
                end: sym!("TessBaseAPIEnd"),
                set_psm: sym!("TessBaseAPISetPageSegMode"),
                set_var: sym!("TessBaseAPISetVariable"),
                set_image: sym!("TessBaseAPISetImage"),
                recognize: sym!("TessBaseAPIRecognize"),
                tsv: sym!("TessBaseAPIGetTsvText"),
                del_text: sym!("TessDeleteText"),
            })
        }
    }

    struct Handle(&'static Api, H);
    impl Drop for Handle {
        fn drop(&mut self) {
            // SAFETY: the handle came from TessBaseAPICreate and is dropped once
            unsafe {
                (self.0.end)(self.1);
                (self.0.delete)(self.1);
            }
        }
    }

    fn init(api: &'static Api, dir: Option<&Path>, langs: &str) -> Option<Handle> {
        let l = CString::new(langs).ok()?;
        let d = dir.and_then(|d| CString::new(d.as_os_str().as_encoded_bytes()).ok());
        // SAFETY: valid C strings that outlive the call; the handle is freed by `Handle`
        unsafe {
            let h = (api.create)();
            if h.is_null() {
                return None;
            }
            let h = Handle(api, h);
            ((api.init3)(h.1, d.as_ref().map_or(std::ptr::null(), |d| d.as_ptr()), l.as_ptr()) == 0).then_some(h)
        }
    }

    pub fn read(api: &'static Api, img: &Gray, dir: Option<&Path>, langs: &str) -> Result<Vec<Word>, String> {
        let _busy = BUSY.lock().unwrap_or_else(|e| e.into_inner());
        let res = (|| {
            let h = init(api, dir, langs).or_else(|| init(api, dir, "eng")).or_else(|| init(api, None, "eng")).ok_or_else(no_lang_msg)?;
            // SAFETY: the picture buffer lives through Recognize; the TSV text is copied, then freed with TessDeleteText
            unsafe {
                (api.set_var)(h.1, c"debug_file".as_ptr(), c"/dev/null".as_ptr());
                for psm in [3, 6] {
                    (api.set_psm)(h.1, psm);
                    (api.set_image)(h.1, img.px.as_ptr(), img.w as c_int, img.h as c_int, 1, img.w as c_int);
                    if (api.recognize)(h.1, std::ptr::null_mut()) != 0 {
                        return Err("Tesseract couldn't read this picture".to_string());
                    }
                    let p = (api.tsv)(h.1, 0);
                    if p.is_null() {
                        continue;
                    }
                    let s = CStr::from_ptr(p).to_string_lossy().into_owned();
                    (api.del_text)(p);
                    let ws = parse_tsv(&s);
                    if !ws.is_empty() {
                        return Ok(ws);
                    }
                }
            }
            Ok(vec![])
        })();
        // the engine took ~100 MB: give it back to the system (Lasso stays small while idle)
        #[cfg(target_env = "gnu")]
        // SAFETY: no arguments to get wrong; only returns free heap memory
        unsafe {
            libc::malloc_trim(0);
        }
        res
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tsv_fixture() {
        let ws = parse_tsv(include_str!("sample.tsv"));
        assert_eq!(ws.len(), 9);
        assert_eq!(ws[0].text, "|");
        assert_eq!(ws[1].r, R::new(40.0, 20.0, 88.0, 22.0));
        assert_eq!(ws[1].conf, 96.1);
        let t = super::super::text(&super::super::clean(ws));
        assert_eq!(t, "Lasso Settings\nCopy text\n\nRun: dnf");
        // the library's TSV has no header line
        let lib = "5\t1\t1\t1\t1\t1\t10\t20\t30\t40\t96.5\tHello\n";
        assert_eq!(parse_tsv(lib)[0].text, "Hello");
    }

    #[test]
    fn languages() {
        let s = |v: &[&str]| v.iter().map(|x| x.to_string()).collect::<Vec<_>>();
        assert_eq!(parse_list_langs("List of available languages in \"/usr/share/tesseract/tessdata/\" (3):\ndan\neng\nosd\n"), s(&["dan", "eng", "osd"]));
        assert_eq!(pick_langs(&s(&["dan", "eng", "osd"]), Some("dan")).as_deref(), Some("eng+dan"));
        assert_eq!(pick_langs(&s(&["dan", "eng", "osd"]), Some("deu")).as_deref(), Some("eng"));
        assert_eq!(pick_langs(&s(&["deu", "osd"]), None).as_deref(), Some("deu"));
        assert_eq!(pick_langs(&s(&["osd"]), None), None);
        assert_eq!(pick_langs(&[], Some("dan")).as_deref(), Some("eng"));
        assert_eq!(tess_lang("da_DK.UTF-8"), Some("dan"));
        assert_eq!(tess_lang("zh_TW.UTF-8"), Some("chi_tra"));
        assert_eq!(tess_lang("nb_NO"), Some("nor"));
        assert_eq!(tess_lang("xx_YY"), None);
    }

    #[test]
    fn install_messages() {
        let nobara = "NAME=\"Nobara Linux\"\nID=nobara\nID_LIKE=\"rhel centos fedora\"\n";
        assert_eq!(missing_for(pm_for(&os_ids(nobara), false)), "Install Tesseract to copy text: sudo dnf install tesseract");
        assert_eq!(pm_for(&os_ids(nobara), true), Some(Pm::Ostree));
        let mint = "ID=linuxmint\nID_LIKE=\"ubuntu debian\"\n";
        assert_eq!(missing_for(pm_for(&os_ids(mint), false)), "Install Tesseract to copy text: sudo apt install tesseract-ocr");
        let endeavour = "ID=endeavouros\nID_LIKE=arch\n";
        assert_eq!(missing_for(pm_for(&os_ids(endeavour), false)), "Install Tesseract to copy text: sudo pacman -S tesseract tesseract-data-eng");
        assert_eq!(pm_for(&os_ids("ID=\"opensuse-tumbleweed\"\nID_LIKE=\"opensuse suse\"\n"), false), Some(Pm::Zypper));
        let unknown = missing_for(pm_for(&os_ids("ID=gentoo\n"), false));
        assert!(unknown.contains("tesseract-ocr"), "{unknown}");
        assert_eq!(no_lang_for(Some(Pm::Apt)), "Tesseract needs a language to copy text: sudo apt install tesseract-ocr-eng");
        // short enough for the toast
        for pm in [Pm::Dnf, Pm::Ostree, Pm::Apt, Pm::Pacman, Pm::Zypper] {
            assert!(missing_for(Some(pm)).len() < 90);
        }
    }

    #[test]
    fn finds_programs_and_cleans_appimage_paths() {
        use std::os::unix::fs::PermissionsExt;
        let d = std::env::temp_dir().join(format!("lasso-ocr-find-{}", std::process::id()));
        let (a, b) = (d.join("a"), d.join("b"));
        std::fs::create_dir_all(&a).unwrap();
        std::fs::create_dir_all(&b).unwrap();
        std::fs::write(a.join("tesseract"), "not executable").unwrap();
        std::fs::write(b.join("tesseract"), "#!/bin/sh\n").unwrap();
        std::fs::set_permissions(b.join("tesseract"), std::fs::Permissions::from_mode(0o755)).unwrap();
        let found = find_in(&[d.join("missing"), a.clone(), b.clone()], "tesseract");
        let _ = std::fs::remove_dir_all(&d);
        assert_eq!(found, Some(b.join("tesseract")));

        assert_eq!(strip_dir("/tmp/.mount_x/usr/lib:/usr/lib64", "/tmp/.mount_x").as_deref(), Some("/usr/lib64"));
        assert_eq!(strip_dir("/tmp/.mount_x/usr/lib", "/tmp/.mount_x"), None);
    }
}
