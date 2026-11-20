//! The share-link calls to the Echo Cloud server ("echo-cloud"), over a small `Http` trait so the logic can be
//! unit-tested without a network (tests: a fake `Http` + a mock server on 127.0.0.1 in `share/tests.rs`).
//!
//! Same calls Echo makes (its `ec_call`): JSON in and out, `{"error": ...}` = a message for the user,
//! `Authorization: Bearer <token>`. The Lasso part of the server (kind = image, /health "lasso") is in
//! `notes/server-patches/` - until it's deployed `ready()` says no and nothing is uploaded.

use serde_json::{json, Value};

/// One HTTP answer: status + body
#[derive(Clone, Debug)]
pub struct Resp {
    pub code: u16,
    pub body: String,
}

pub trait Http: Send + Sync {
    /// One request. `Ok` for any HTTP answer (also 4xx / 5xx), `Err` = no answer (offline, DNS, TLS, timeout).
    fn call(&self, method: &str, url: &str, headers: &[(&str, String)], body: Option<Vec<u8>>, timeout_s: u64) -> Result<Resp, String>;
}

#[derive(Clone, Debug, PartialEq)]
pub enum Fail {
    /// no answer from the server (the text is for the log)
    Offline(String),
    /// 401: the sign-in ended (180 days unused, signed out elsewhere) - sign in again
    SignedOut,
    /// the server isn't updated for Lasso yet (no "lasso" in /health)
    NotReady,
    /// the server's own message ("Echo Cloud is full ...", "Too many uploads ...") or what went wrong
    Msg(String),
}

impl Fail {
    /// What the user reads
    pub fn text(&self) -> String {
        match self {
            Fail::Offline(_) => "Couldn't reach the Sonela server - check your internet".into(),
            Fail::SignedOut => "Signed out - sign in again (Settings > Share links)".into(),
            Fail::NotReady => "Share links need the Sonela server update first".into(),
            Fail::Msg(m) => m.clone(),
        }
    }
}

/// A made link
#[derive(Clone, Debug, PartialEq)]
pub struct Link {
    pub url: String,
    /// the id in the link (`.../s/<id>`)
    pub id: String,
    /// unix seconds, 0 = never
    pub expires: i64,
    /// a view-once link that was already opened (the server keeps it; a re-share needs a new link)
    pub viewed: bool,
}

pub struct Api<'a> {
    /// "https://echoclip.link" (no / at the end)
    pub base: String,
    /// "" = not signed in
    pub token: String,
    pub http: &'a dyn Http,
}

/// Lasso's server version this client speaks (the server's /health says `"lasso": N`, N >= this)
pub const PROTOCOL: u64 = 1;
/// Pictures over this are refused by the server (export's "Fits Discord" keeps them at 10 MB anyway)
pub const MAX_BYTES: usize = 25 * 1000 * 1000;

impl Api<'_> {
    fn headers(&self) -> Vec<(&'static str, String)> {
        let mut h = vec![("content-type", "application/json".to_string())];
        if !self.token.is_empty() {
            h.push(("authorization", format!("Bearer {}", self.token)));
        }
        h
    }

    /// Echo's ec_call: JSON in and out
    pub fn call(&self, method: &str, path: &str, body: Option<Value>) -> Result<Value, Fail> {
        let url = format!("{}{path}", self.base);
        let r = self.http.call(method, &url, &self.headers(), body.map(|b| b.to_string().into_bytes()), 30).map_err(Fail::Offline)?;
        answer(r)
    }

    /// Is the server updated for Lasso? Only a GET, writes nothing - asked before every upload, so a Lasso with
    /// `share_on=1` and an old server never calls the image endpoints.
    pub fn ready(&self) -> Result<(), Fail> {
        let r = self.http.call("GET", &format!("{}/health", self.base), &[], None, 15).map_err(Fail::Offline)?;
        let v: Value = serde_json::from_str(&r.body).unwrap_or(Value::Null);
        if r.code == 200 && v["lasso"].as_u64().unwrap_or(0) >= PROTOCOL {
            Ok(())
        } else {
            Err(Fail::NotReady)
        }
    }

    /// Sign-in step 1: the server emails a 6-digit code (Echo Cloud's sign-in, same account)
    pub fn auth_start(&self, email: &str) -> Result<(), Fail> {
        self.call("POST", "/auth/start", Some(json!({ "email": email, "app": "lasso" }))).map(|_| ())
    }

    /// Sign-in step 2: the code -> a session token
    pub fn auth_verify(&self, email: &str, code: &str) -> Result<String, Fail> {
        let v = self.call("POST", "/auth/verify", Some(json!({ "email": email, "code": code })))?;
        match v["token"].as_str() {
            Some(t) if !t.is_empty() => Ok(t.to_string()),
            _ => Err(Fail::Msg("The server didn't sign you in - try again".into())),
        }
    }

    pub fn logout(&self) -> Result<(), Fail> {
        self.call("POST", "/auth/logout", Some(json!({}))).map(|_| ())
    }

    /// A presigned upload link for exactly this file and size. `share_only`: the file goes when its link does.
    pub fn upload_url(&self, path: &str, size: usize) -> Result<String, Fail> {
        if size > MAX_BYTES {
            return Err(Fail::Msg("This picture is too big to share (over 25 MB)".into()));
        }
        let v = self.call("POST", "/upload", Some(json!({ "path": path, "size": size, "share_only": true, "kind": "image" })))?;
        match v["url"].as_str() {
            Some(u) if u.starts_with("https://") || u.starts_with("http://127.0.0.1") => Ok(u.to_string()),
            _ => Err(Fail::Msg("The server didn't give an upload link".into())),
        }
    }

    /// The bytes to the presigned link (straight to R2, not through the server)
    pub fn put(&self, url: &str, bytes: Vec<u8>) -> Result<(), Fail> {
        let n = bytes.len();
        let r = self.http.call("PUT", url, &[("content-length", n.to_string())], Some(bytes), 120).map_err(Fail::Offline)?;
        if r.code >= 300 {
            return Err(Fail::Msg(format!("Upload failed ({})", r.code)));
        }
        Ok(())
    }

    /// The link for an uploaded picture (again for the same path = the same link, new expiry)
    pub fn link(&self, path: &str, o: &LinkReq) -> Result<Link, Fail> {
        let v = self.call(
            "POST",
            "/share",
            Some(json!({ "path": path, "kind": "image", "days": o.days, "once": o.once, "width": o.w, "height": o.h, "title": o.title })),
        )?;
        let url = v["url"].as_str().unwrap_or("").to_string();
        let id = id_of(&url).ok_or_else(|| Fail::Msg("The server didn't return a link".into()))?;
        Ok(Link { url, id, expires: v["expires"].as_i64().unwrap_or(0), viewed: v["viewed"].as_bool().unwrap_or(false) })
    }

    /// "Stop sharing": the link dies now, the picture is deleted (it was uploaded only for the link)
    pub fn remove(&self, path: &str) -> Result<u64, Fail> {
        let v = self.call("POST", "/share/remove", Some(json!({ "path": path })))?;
        Ok(v["removed"].as_u64().unwrap_or(0))
    }
}

/// What `/share` gets besides the path
#[derive(Clone, Debug, Default)]
pub struct LinkReq {
    /// 1 / 7 / 30, 0 = never
    pub days: u32,
    /// delete after the first view (6c #9)
    pub once: bool,
    pub w: u32,
    pub h: u32,
    pub title: String,
}

fn answer(r: Resp) -> Result<Value, Fail> {
    let v: Value = serde_json::from_str(&r.body).unwrap_or(Value::Null);
    if r.code == 401 {
        return Err(Fail::SignedOut);
    }
    if let Some(e) = v["error"].as_str() {
        return Err(Fail::Msg(e.to_string()));
    }
    if r.code >= 300 {
        return Err(Fail::Msg(format!("The Sonela server answered {}", r.code)));
    }
    Ok(v)
}

/// "https://sonela.app/s/abc234defg" -> "abc234defg" (Echo's ids: 6-20 of a-z 0-9). None = not a share link.
pub fn id_of(url: &str) -> Option<String> {
    if !(url.starts_with("https://") || url.starts_with("http://127.0.0.1")) {
        return None;
    }
    let id = url.trim_end_matches('/').rsplit_once("/s/")?.1;
    (id.len() >= 6 && id.len() <= 20 && id.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit())).then(|| id.to_string())
}

/// Where the picture goes in the account: `Lasso/<yyyy>/<mm>/<name>.<ext>` (the server puts `u/<user>/` in front)
pub fn key_for(name: &str, ext: &str, year: i32, month: u32) -> String {
    let clean: String = name.chars().map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '-' }).take(80).collect();
    let clean = clean.trim_matches('-');
    let clean = if clean.is_empty() { "shot" } else { clean };
    format!("Lasso/{year:04}/{month:02}/{clean}.{ext}")
}

/// Email as typed -> what the server wants ("" = not an email)
pub fn clean_email(e: &str) -> String {
    let e = e.trim().to_lowercase();
    let ok = e.len() <= 200 && !e.chars().any(char::is_whitespace) && e.split_once('@').is_some_and(|(a, d)| !a.is_empty() && d.contains('.') && !d.starts_with('.') && !d.ends_with('.'));
    if ok {
        e
    } else {
        String::new()
    }
}

/// The 6-digit code as typed ("123 456", "123-456") -> "123456", None = not 6 digits
pub fn clean_code(c: &str) -> Option<String> {
    let d: String = c.chars().filter(|c| c.is_ascii_digit()).collect();
    (d.len() == 6 && c.chars().all(|c| c.is_ascii_digit() || c == ' ' || c == '-')).then_some(d)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    /// Answers from a list, remembers what was asked
    struct Fake {
        answers: Mutex<Vec<Result<Resp, String>>>,
        asked: Mutex<Vec<(String, String, Option<String>, Vec<(String, String)>)>>,
    }
    impl Fake {
        fn new(a: Vec<Result<(u16, &str), &str>>) -> Fake {
            Fake {
                answers: Mutex::new(a.into_iter().rev().map(|r| r.map(|(c, b)| Resp { code: c, body: b.into() }).map_err(|e| e.to_string())).collect()),
                asked: Mutex::new(vec![]),
            }
        }
    }
    impl Http for Fake {
        fn call(&self, method: &str, url: &str, headers: &[(&str, String)], body: Option<Vec<u8>>, _t: u64) -> Result<Resp, String> {
            self.asked.lock().unwrap().push((
                method.into(),
                url.into(),
                body.map(|b| String::from_utf8_lossy(&b).to_string()),
                headers.iter().map(|(k, v)| (k.to_string(), v.clone())).collect(),
            ));
            self.answers.lock().unwrap().pop().unwrap_or(Err("no more answers".into()))
        }
    }

    fn api<'a>(f: &'a Fake, token: &str) -> Api<'a> {
        Api { base: "https://api.test".into(), token: token.into(), http: f }
    }

    #[test]
    fn ready_needs_the_lasso_flag() {
        let f = Fake::new(vec![Ok((200, r#"{"service":"echo-cloud","ok":true}"#)), Ok((200, r#"{"ok":true,"lasso":1}"#)), Err("dns")]);
        assert_eq!(api(&f, "").ready(), Err(Fail::NotReady));
        assert_eq!(api(&f, "").ready(), Ok(()));
        assert!(matches!(api(&f, "").ready(), Err(Fail::Offline(_))));
        let asked = f.asked.lock().unwrap();
        assert!(asked.iter().all(|a| a.0 == "GET" && a.1 == "https://api.test/health"));
    }

    #[test]
    fn errors_and_sign_out() {
        let f = Fake::new(vec![
            Ok((413, r#"{"error":"Echo Cloud is full","full":true}"#)),
            Ok((401, r#"{"error":"Signed out - sign in to Echo Cloud again"}"#)),
            Ok((500, "oops")),
        ]);
        let a = api(&f, "tok");
        assert_eq!(a.upload_url("Lasso/x.png", 10), Err(Fail::Msg("Echo Cloud is full".into())));
        assert_eq!(a.upload_url("Lasso/x.png", 10), Err(Fail::SignedOut));
        assert_eq!(a.upload_url("Lasso/x.png", 10), Err(Fail::Msg("The Sonela server answered 500".into())));
        // the token goes in the header, never in the URL or body
        for (_, url, body, h) in f.asked.lock().unwrap().iter() {
            assert!(!url.contains("tok") && !body.as_deref().unwrap_or("").contains("tok"));
            assert!(h.contains(&("authorization".into(), "Bearer tok".into())));
        }
    }

    #[test]
    fn too_big_is_refused_without_a_call() {
        let f = Fake::new(vec![]);
        assert!(matches!(api(&f, "t").upload_url("Lasso/a.png", MAX_BYTES + 1), Err(Fail::Msg(_))));
        assert!(f.asked.lock().unwrap().is_empty());
    }

    #[test]
    fn upload_and_link_bodies() {
        let f = Fake::new(vec![
            Ok((200, r#"{"url":"https://acc.r2.cloudflarestorage.com/echo-cloud/u/x/Lasso/2026/11/a.png?X-Amz=1","size":3}"#)),
            Ok((200, "")),
            Ok((200, r#"{"url":"https://sonela.app/s/abc234defg","expires":1800000000}"#)),
            Ok((200, r#"{"url":"javascript:alert(1)"}"#)),
        ]);
        let a = api(&f, "t");
        let u = a.upload_url("Lasso/2026/11/a.png", 3).unwrap();
        a.put(&u, vec![1, 2, 3]).unwrap();
        let req = LinkReq { days: 30, once: false, w: 800, h: 600, title: String::new() };
        let l = a.link("Lasso/2026/11/a.png", &req).unwrap();
        assert_eq!(l, Link { url: "https://sonela.app/s/abc234defg".into(), id: "abc234defg".into(), expires: 1_800_000_000, viewed: false });
        assert!(a.link("Lasso/2026/11/a.png", &req).is_err());
        let asked = f.asked.lock().unwrap();
        let up: Value = serde_json::from_str(asked[0].2.as_deref().unwrap()).unwrap();
        assert_eq!(up["share_only"], true);
        assert_eq!(up["kind"], "image");
        assert_eq!(up["size"], 3);
        assert_eq!(asked[1].0, "PUT");
        assert!(asked[1].3.contains(&("content-length".into(), "3".into())));
        // the presigned PUT never carries the session token
        assert!(!asked[1].3.iter().any(|(k, _)| k == "authorization"));
        let sh: Value = serde_json::from_str(asked[2].2.as_deref().unwrap()).unwrap();
        assert_eq!((sh["kind"].as_str(), sh["days"].as_u64(), sh["width"].as_u64()), (Some("image"), Some(30), Some(800)));
    }

    #[test]
    fn helpers() {
        assert_eq!(id_of("https://sonela.app/s/abc234defg"), Some("abc234defg".into()));
        assert_eq!(id_of("https://sonela.app/s/abc234defg/"), Some("abc234defg".into()));
        assert_eq!(id_of("https://sonela.app/s/ABC"), None);
        assert_eq!(id_of("ftp://x/s/abc234defg"), None);
        assert_eq!(key_for("20261113-041500-0042", "png", 2026, 11), "Lasso/2026/11/20261113-041500-0042.png");
        assert_eq!(key_for("../../etc", "webp", 2026, 1), "Lasso/2026/01/etc.webp");
        assert_eq!(key_for("", "png", 2026, 1), "Lasso/2026/01/shot.png");
        let at = "@example.com";
        assert_eq!(clean_email(&format!("  Someone{} ", at.to_uppercase())), format!("someone{at}"));
        assert_eq!(clean_email("no-at.example.com"), "");
        assert_eq!(clean_email(&format!("a{}", "@b")), "");
        assert_eq!(clean_code("123 456"), Some("123456".into()));
        assert_eq!(clean_code("12345"), None);
        assert_eq!(clean_code("12a456"), None);
    }
}
