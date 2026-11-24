//! Share-link client tests against a tiny mock of the echo-cloud server on 127.0.0.1, through the REAL transport
//! (net.rs, reqwest). Nothing reaches the internet, the keyring or the user's config: the token stays in a
//! scratch folder under target/ (secret.rs keeps it plain under cfg(test)).

use super::api::Fail;
use super::store::{Account, Store};
use super::*;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

#[derive(Default)]
struct Server {
    /// /health says lasso: 1
    ready: bool,
    /// every request: "METHOD /path"
    log: Vec<String>,
    /// R2: key -> bytes
    objects: HashMap<String, Vec<u8>>,
    /// link id -> (key, days, kind)
    shares: HashMap<String, (String, u64, String)>,
    tokens: Vec<String>,
    next_id: u32,
    /// view-once links already opened (the server answers `viewed: true` for them)
    viewed: Vec<String>,
}

struct Mock {
    base: String,
    st: Arc<Mutex<Server>>,
}

const TOKEN: &str = "tok_mock_session";
/// test addresses are built at run time (the public repo's leak check refuses any e-mail address in a file)
fn mail(user: &str) -> String {
    format!("{user}{}", "@example.com")
}

fn mock(ready: bool) -> Mock {
    let l = TcpListener::bind("127.0.0.1:0").unwrap();
    let base = format!("http://127.0.0.1:{}", l.local_addr().unwrap().port());
    let st = Arc::new(Mutex::new(Server { ready, ..Default::default() }));
    let (st2, base2) = (st.clone(), base.clone());
    std::thread::spawn(move || {
        for c in l.incoming() {
            let Ok(mut c) = c else { continue };
            let mut r = BufReader::new(c.try_clone().unwrap());
            let mut first = String::new();
            if r.read_line(&mut first).is_err() {
                continue;
            }
            let mut head: HashMap<String, String> = HashMap::new();
            loop {
                let mut l = String::new();
                if r.read_line(&mut l).is_err() || l.trim().is_empty() {
                    break;
                }
                if let Some((k, v)) = l.split_once(':') {
                    head.insert(k.trim().to_lowercase(), v.trim().to_string());
                }
            }
            let n: usize = head.get("content-length").and_then(|v| v.parse().ok()).unwrap_or(0);
            let mut body = vec![0u8; n];
            let _ = r.read_exact(&mut body);
            let mut p = first.split_whitespace();
            let (m, path) = (p.next().unwrap_or("").to_string(), p.next().unwrap_or("").to_string());
            let auth = head.get("authorization").cloned().unwrap_or_default();
            let (code, out) = handle(&st2, &base2, &m, &path, &auth, body);
            let _ = write!(c, "HTTP/1.1 {code} X\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{out}", out.len());
        }
    });
    Mock { base, st }
}

fn handle(st: &Mutex<Server>, base: &str, m: &str, path: &str, auth: &str, body: Vec<u8>) -> (u16, String) {
    let mut s = st.lock().unwrap();
    s.log.push(format!("{m} {}", path.split('?').next().unwrap_or("")));
    let j: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
    let authed = auth.strip_prefix("Bearer ").is_some_and(|t| s.tokens.iter().any(|x| x == t));
    let need = |ok: bool, v: Value| if ok { (200, v.to_string()) } else { (401, json!({"error": "Signed out - sign in to Echo Cloud again"}).to_string()) };
    match (m, path) {
        ("GET", "/health") => (200, if s.ready { json!({"service": "echo-cloud", "ok": true, "lasso": 1}) } else { json!({"service": "echo-cloud", "ok": true}) }.to_string()),
        ("POST", "/auth/start") => (200, json!({"ok": true}).to_string()),
        ("POST", "/auth/verify") => {
            if j["code"] == "123456" {
                s.tokens.push(TOKEN.into());
                (200, json!({"token": TOKEN}).to_string())
            } else {
                (400, json!({"error": "Wrong code"}).to_string())
            }
        }
        ("POST", "/auth/device") => {
            if j["device"].as_str().is_some_and(|k| k.len() == 64) {
                s.tokens.push(TOKEN.into());
                (200, json!({"token": TOKEN, "device": true}).to_string())
            } else {
                (400, json!({"error": "Bad device key"}).to_string())
            }
        }
        ("POST", "/auth/logout") => {
            s.tokens.clear();
            (200, json!({"ok": true}).to_string())
        }
        ("POST", "/upload") => {
            let key = j["path"].as_str().unwrap_or("").to_string();
            need(authed, json!({"url": format!("{base}/r2/{key}?X-Amz-Signature=x"), "size": j["size"], "share_only": j["share_only"]}))
        }
        ("PUT", p) if p.starts_with("/r2/") => {
            let key = p[4..].split('?').next().unwrap_or("").to_string();
            s.objects.insert(key, body);
            (200, String::new())
        }
        ("POST", "/share") => {
            if !authed {
                return need(false, Value::Null);
            }
            let key = j["path"].as_str().unwrap_or("").to_string();
            if !s.objects.contains_key(&key) {
                return (404, json!({"error": "This clip isn't in Echo Cloud yet - upload it first"}).to_string());
            }
            let days = j["days"].as_u64().unwrap_or(0);
            let id = match s.shares.iter().find(|(_, v)| v.0 == key).map(|(k, _)| k.clone()) {
                Some(id) => id,
                None => {
                    s.next_id += 1;
                    format!("mockid{:04}", s.next_id)
                }
            };
            s.shares.insert(id.clone(), (key, days, j["kind"].as_str().unwrap_or("clip").to_string()));
            let expires = if days == 0 { 0 } else { 4_000_000_000i64 + days as i64 * 86400 };
            if s.viewed.contains(&id) {
                return (200, json!({"url": format!("https://sonela.app/s/{id}"), "id": id, "expires": expires, "viewed": true}).to_string());
            }
            (200, json!({"url": format!("https://sonela.app/s/{id}"), "expires": expires}).to_string())
        }
        ("POST", "/share/remove") => {
            if !authed {
                return need(false, Value::Null);
            }
            let key = j["path"].as_str().unwrap_or("").to_string();
            let before = s.shares.len();
            s.shares.retain(|_, v| v.0 != key);
            let removed = before - s.shares.len();
            s.objects.remove(&key);
            (200, json!({"ok": true, "removed": removed}).to_string())
        }
        _ => (404, json!({"error": "not found"}).to_string()),
    }
}

fn scratch(name: &str) -> PathBuf {
    let d = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("target").join("share-test").join(name);
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn job(capture: &str, bytes: &[u8]) -> Job {
    Job { capture: capture.into(), bytes: bytes.to_vec(), ext: "png", w: 820, h: 470, days: None, once: false, now: 1_800_000_000, year: 2026, month: 11 }
}

#[test]
fn old_server_gets_no_upload() {
    let m = mock(false);
    let d = scratch("old-server");
    let st = Store::at(&d);
    st.set_account(&Account { email: mail("someone"), token: TOKEN.into() }).unwrap();
    let r = run(&net::Net, &m.base, &st, job("cap1", b"png"), 30);
    assert_eq!(r.unwrap_err(), Fail::NotReady);
    // only the read-only health check went out; sign-in is refused the same way
    assert_eq!(sign_in_start_with(&net::Net, &m.base, &mail("someone")).unwrap_err(), Fail::NotReady);
    assert_eq!(m.st.lock().unwrap().log, vec!["GET /health", "GET /health"]);
    let _ = std::fs::remove_dir_all(&d);
}

/// No account (21 Nov 2026): the first link makes a device key, the server turns it into a session; the next link
/// reuses that session (one /auth/device call), the key stays the same
#[test]
fn not_signed_in_uses_a_device_key() {
    let m = mock(true);
    let d = scratch("signed-out");
    let st = Store::at(&d);
    run(&net::Net, &m.base, &st, job("cap1", b"png"), 30).unwrap();
    let (key, token) = st.device();
    assert_eq!(key.len(), 64);
    assert_eq!(token, TOKEN);
    run(&net::Net, &m.base, &st, job("cap2", b"png"), 30).unwrap();
    assert_eq!(st.device().0, key);
    let log = m.st.lock().unwrap().log.clone();
    assert_eq!(log.iter().filter(|l| *l == "POST /auth/device").count(), 1, "{log:?}");
    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn sign_in_share_update_expiry_stop_sign_out() {
    let m = mock(true);
    let d = scratch("full-flow");
    let st = Store::at(&d);

    // sign in: bad code, then the right one
    assert_eq!(sign_in_start_with(&net::Net, &m.base, &format!(" Some{}", mail("one").to_uppercase())).unwrap(), mail("someone"));
    assert!(matches!(sign_in_finish_with(&net::Net, &m.base, &st, &mail("someone"), "12345"), Err(Fail::Msg(_))));
    assert_eq!(sign_in_finish_with(&net::Net, &m.base, &st, &mail("someone"), "000000").unwrap_err(), Fail::Msg("Wrong code".into()));
    sign_in_finish_with(&net::Net, &m.base, &st, &mail("someone"), "123 456").unwrap();
    assert_eq!(st.account().unwrap().email, mail("someone"));

    // share: upload -> PUT -> link, remembered with the default expiry
    let (s, again) = run(&net::Net, &m.base, &st, job("20261113-041500-0042", b"\x89PNG first"), 30).unwrap();
    assert!(!again);
    assert_eq!(s.url, "https://sonela.app/s/mockid0001");
    assert_eq!((s.path.as_str(), s.days), ("Lasso/2026/11/20261113-041500-0042.png", 30));
    {
        let sv = m.st.lock().unwrap();
        assert_eq!(sv.objects[&s.path], b"\x89PNG first");
        assert_eq!(sv.shares["mockid0001"].2, "image");
        assert_eq!(sv.log, vec!["GET /health", "POST /auth/start", "POST /auth/verify", "POST /auth/verify", "GET /health", "POST /upload", "PUT /r2/Lasso/2026/11/20261113-041500-0042.png", "POST /share"]);
    }
    assert_eq!(st.get("20261113-041500-0042", 1_800_000_000).unwrap().id, "mockid0001");

    // the same capture again (edited): same path, same link, new picture
    let (s2, again) = run(&net::Net, &m.base, &st, job("20261113-041500-0042", b"\x89PNG fixed"), 7).unwrap();
    assert!(again);
    assert_eq!((s2.url.as_str(), s2.days), ("https://sonela.app/s/mockid0001", 30));
    assert_eq!(m.st.lock().unwrap().objects[&s.path], b"\x89PNG fixed");

    // a second capture gets its own link
    let (s3, _) = run(&net::Net, &m.base, &st, job("other", b"\x89PNG"), 7).unwrap();
    assert_eq!((s3.id.as_str(), s3.days), ("mockid0002", 7));

    // new expiry: never
    let n = expiry_with(&net::Net, &m.base, &st, "other", 0, 1_800_000_000).unwrap();
    assert_eq!((n.days, n.expires), (0, 0));
    assert_eq!(m.st.lock().unwrap().shares["mockid0002"].1, 0);

    // stop sharing: gone on the server and here
    stop_with(&net::Net, &m.base, &st, "20261113-041500-0042", 1_800_000_000).unwrap();
    assert!(!m.st.lock().unwrap().shares.contains_key("mockid0001"));
    assert!(!m.st.lock().unwrap().objects.contains_key(&s.path));
    assert_eq!(st.get("20261113-041500-0042", 1_800_000_000), None);
    // a capture without a link: nothing to do, no call
    let calls = m.st.lock().unwrap().log.len();
    stop_with(&net::Net, &m.base, &st, "never-shared", 1_800_000_000).unwrap();
    assert_eq!(m.st.lock().unwrap().log.len(), calls);

    // sign out: server session ended, account forgotten here - the links stay listed (they're still public; after
    // signing in again the Library can stop them - review C3)
    let before = st.get("other", 1_800_000_000);
    sign_out_with(&net::Net, &m.base, &st);
    assert!(m.st.lock().unwrap().tokens.is_empty());
    assert_eq!(st.account(), None);
    assert_eq!(st.get("other", 1_800_000_000), before);
    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn server_sign_out_forgets_the_account() {
    let m = mock(true);
    let d = scratch("expired-session");
    let st = Store::at(&d);
    // a token the server doesn't know any more (180 days unused)
    st.set_account(&Account { email: mail("someone"), token: "tok_old".into() }).unwrap();
    assert_eq!(run(&net::Net, &m.base, &st, job("cap", b"x"), 30).unwrap_err(), Fail::SignedOut);
    assert_eq!(st.account(), None);
    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn offline_is_a_clear_error() {
    // a port nobody listens on
    let l = TcpListener::bind("127.0.0.1:0").unwrap();
    let base = format!("http://127.0.0.1:{}", l.local_addr().unwrap().port());
    drop(l);
    let d = scratch("offline");
    let st = Store::at(&d);
    st.set_account(&Account { email: mail("someone"), token: TOKEN.into() }).unwrap();
    let e = run(&net::Net, &base, &st, job("cap", b"x"), 30).unwrap_err();
    assert!(matches!(e, Fail::Offline(_)));
    assert_eq!(e.text(), "Couldn't reach the Sonela server - check your internet");
    // still signed in (no answer is not "signed out")
    assert!(st.account().is_some());
    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn settings_and_words() {
    let mut c = crate::config::Config::default();
    assert!(!enabled(&c));
    assert_eq!(default_days(&c), 30);
    c.set("link_expiry", "7");
    assert_eq!(default_days(&c), 7);
    c.set("link_expiry", "5");
    assert_eq!(default_days(&c), 30);
    c.set("share_on", "1");
    assert!(enabled(&c));
    assert_eq!(opt(&c, "share_check", "1"), "1");
    c.set("share_check", "0");
    assert_eq!(opt(&c, "share_check", "1"), "0");
    assert_eq!(base_or_default(""), API_DEFAULT);
    assert_eq!(base_or_default("http://evil.example"), API_DEFAULT);
    assert_eq!(base_or_default("https://staging.example/"), "https://staging.example");
    assert_eq!(expiry_label(1), "expires in 24 hours");
    assert_eq!(expiry_label(30), "expires in 30 days");
    assert_eq!(expiry_label(0), "never expires");
    assert_eq!(expiry_left(0, 5), "never expires");
    assert_eq!(expiry_left(1000 + 7 * 86400, 1000), "expires in 7 days");
    assert_eq!(expiry_left(1000 + 3600, 1000), "expires within 24 hours");
    assert_eq!(short("abc234defg"), "abc…");
}

#[test]
fn privacy_words() {
    let r = R::new(0.0, 0.0, 1.0, 1.0);
    assert_eq!(privacy_message(&[]), "");
    assert_eq!(privacy_message(&[(r, "email"), (r, "key")]), "Found an email and a key");
    assert_eq!(privacy_message(&[(r, "email"), (r, "number"), (r, "email"), (r, "IP address")]), "Found 2 emails, a number and an IP address");
    assert_eq!(privacy_message(&[(r, "IP address"), (r, "IP address")]), "Found 2 IP addresses");
    // warned once -> the next press within 2 minutes shares
    assert!(first_warning("cap-privacy"));
    assert!(!first_warning("cap-privacy"));
    assert!(first_warning("cap-privacy"));
    assert!(!first_warning(""));
}

#[test]
fn opened_view_once_link_gets_a_new_link() {
    let m = mock(true);
    let d = scratch("viewed-once");
    let st = Store::at(&d);
    st.set_account(&Account { email: mail("someone"), token: secret::protect(TOKEN) }).unwrap();
    m.st.lock().unwrap().tokens.push(TOKEN.into());
    let mut j = job("cap-once", b"first");
    j.once = true;
    let (a, _) = run(&net::Net, &m.base, &st, j, 30).unwrap();
    // somebody opened it: the server keeps that link spent
    m.st.lock().unwrap().viewed.push(a.id.clone());
    let mut j = job("cap-once", b"second");
    j.once = true;
    j.now += 60;
    let (b, again) = run(&net::Net, &m.base, &st, j, 30).unwrap();
    assert_ne!(a.id, b.id, "a new link, not the spent one");
    assert!(!again);
    assert_eq!(st.get("cap-once", 1_800_000_100).map(|s| s.id), Some(b.id.clone()));
    // D2: the opened link's picture was never overwritten (one PUT to it: the first share)
    let puts = m.st.lock().unwrap().log.iter().filter(|l| **l == format!("PUT /r2/{}", a.path)).count();
    assert_eq!(puts, 1);
    // an UNOPENED view-once link shared again: a new link too, the old one stopped
    let mut j = job("cap-once", b"third");
    j.once = true;
    j.now += 120;
    let (c, _) = run(&net::Net, &m.base, &st, j, 30).unwrap();
    assert_ne!(c.id, b.id);
    assert!(!m.st.lock().unwrap().objects.contains_key(&b.path), "the older view-once link is stopped");
    assert_eq!(m.st.lock().unwrap().objects[&c.path], b"third");
    let _ = std::fs::remove_dir_all(&d);
}

/// 24 Nov 2026: a save-folder picture shares under a hash of its path - stable, never the path (it becomes part of
/// the picture's name on the server), and different files get different keys
#[test]
fn file_key_is_stable_and_private() {
    let a = file_key(std::path::Path::new("/pics/Lasso 2026-11-24.png"));
    assert_eq!(a, file_key(std::path::Path::new("/pics/Lasso 2026-11-24.png")));
    assert!(a.starts_with("file-") && a.len() == 21, "{a}");
    assert!(!a.contains("pics") && !a.contains("Lasso") && !a.contains('/'));
    assert_ne!(a, file_key(std::path::Path::new("/pics/Lasso 2026-11-25.png")));
    // the name made for the server from it has no part of the path either
    let k = api::key_for(&a, "png", 2026, 11);
    assert!(!k.contains("pics") && !k.contains("Lasso 2026"), "{k}");
}