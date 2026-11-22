//! What Lasso remembers about share links, next to config.ini:
//! - `share-account.json`: the signed-in email + the token in its protected form (secret.rs)
//! - `share-links.json`: one entry per shared capture (library id -> link, server path, expiry) for the library's
//!   link icon, "Stop sharing", "Update link" and the expiry choice. Expired entries drop out when read.
//! The settings and library windows are their own processes: everything goes through these files.

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
pub struct Account {
    pub email: String,
    /// protected: "ks:" / "enc:<hex>" / plain (test copies, no keyring)
    pub token: String,
}

#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
pub struct Shared {
    /// the library id of the capture ("" = a picture that isn't in the library)
    pub capture: String,
    pub url: String,
    pub id: String,
    /// where it is in the account ("Lasso/2026/11/<name>.png")
    pub path: String,
    /// the expiry the user picked (1 / 7 / 30, 0 = never) - "Update link" keeps it
    pub days: u32,
    #[serde(default)]
    pub once: bool,
    /// unix seconds, 0 = never
    pub expires: i64,
    pub created: i64,
    #[serde(default)]
    pub updated: i64,
    /// picture size (sent again with a new expiry - the page's og:image size)
    #[serde(default)]
    pub w: u32,
    #[serde(default)]
    pub h: u32,
}

pub struct Store {
    dir: PathBuf,
}

impl Store {
    pub fn at(dir: &Path) -> Store {
        Store { dir: dir.to_path_buf() }
    }

    pub fn app() -> Store {
        Store::at(&crate::paths::config_dir())
    }

    fn write(&self, name: &str, text: &str) -> Result<(), String> {
        let _ = std::fs::create_dir_all(&self.dir);
        let p = self.dir.join(name);
        let tmp = p.with_extension("json.tmp");
        std::fs::write(&tmp, text).map_err(|e| e.to_string())?;
        super::secret::private_file(&tmp);
        std::fs::rename(&tmp, &p).map_err(|e| e.to_string())
    }

    pub fn account(&self) -> Option<Account> {
        let s = std::fs::read_to_string(self.dir.join("share-account.json")).ok()?;
        serde_json::from_str::<Account>(&s).ok().filter(|a| !a.email.is_empty() && !a.token.is_empty())
    }

    pub fn set_account(&self, a: &Account) -> Result<(), String> {
        self.write("share-account.json", &serde_json::to_string_pretty(a).map_err(|e| e.to_string())?)
    }

    /// The device key + its last session token (share links without an account, 21 Nov 2026). A plain private file,
    /// never the keyring: no KDE Wallet prompt, and the key only reaches this PC's own picture links.
    pub fn device(&self) -> (String, String) {
        let s = std::fs::read_to_string(self.dir.join("share-device.json")).unwrap_or_default();
        let v: serde_json::Value = serde_json::from_str(&s).unwrap_or_default();
        (v["key"].as_str().unwrap_or("").to_string(), v["token"].as_str().unwrap_or("").to_string())
    }

    pub fn set_device(&self, key: &str, token: &str) -> Result<(), String> {
        self.write("share-device.json", &serde_json::json!({ "key": key, "token": token }).to_string())
    }

    pub fn clear_account(&self) {
        let _ = std::fs::remove_file(self.dir.join("share-account.json"));
    }

    /// Links that still work (expired ones are left out and dropped from the file at the next write)
    pub fn links(&self, now: i64) -> Vec<Shared> {
        let s = std::fs::read_to_string(self.dir.join("share-links.json")).unwrap_or_default();
        let all: Vec<Shared> = serde_json::from_str(&s).unwrap_or_default();
        all.into_iter().filter(|l| l.expires == 0 || l.expires > now).collect()
    }

    fn set_links(&self, l: &[Shared]) -> Result<(), String> {
        self.write("share-links.json", &serde_json::to_string_pretty(l).map_err(|e| e.to_string())?)
    }

    pub fn get(&self, capture: &str, now: i64) -> Option<Shared> {
        if capture.is_empty() {
            return None;
        }
        self.links(now).into_iter().find(|l| l.capture == capture)
    }

    /// Adds or replaces (same capture or same link id)
    pub fn put(&self, s: Shared, now: i64) -> Result<(), String> {
        let mut l = self.links(now);
        l.retain(|x| x.id != s.id && (s.capture.is_empty() || x.capture != s.capture));
        l.push(s);
        // newest last, at most 2000 kept (links that never expire pile up otherwise)
        if l.len() > 2000 {
            let n = l.len() - 2000;
            l.drain(..n);
        }
        self.set_links(&l)
    }

    pub fn remove(&self, link_id: &str, now: i64) -> Result<(), String> {
        let mut l = self.links(now);
        l.retain(|x| x.id != link_id);
        self.set_links(&l)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let d = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("target").join("share-test").join(name);
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn link(capture: &str, id: &str, expires: i64) -> Shared {
        Shared { capture: capture.into(), url: format!("https://sonela.app/s/{id}"), id: id.into(), path: format!("Lasso/2026/11/{capture}.png"), days: 30, expires, created: 1, ..Default::default() }
    }

    #[test]
    fn account_roundtrip() {
        let d = scratch("account");
        let s = Store::at(&d);
        assert_eq!(s.account(), None);
        let a = Account { email: format!("someone{}", "@example.com"), token: "tok".into() };
        s.set_account(&a).unwrap();
        assert_eq!(s.account(), Some(a));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(std::fs::metadata(d.join("share-account.json")).unwrap().permissions().mode() & 0o777, 0o600);
        }
        s.clear_account();
        assert_eq!(s.account(), None);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn links_replace_and_expire() {
        let d = scratch("links");
        let s = Store::at(&d);
        s.put(link("a", "aaaaaaaaaa", 0), 100).unwrap();
        s.put(link("b", "bbbbbbbbbb", 150), 100).unwrap();
        // same capture again (Update link / new expiry): replaced, not doubled
        s.put(link("a", "aaaaaaaaaa", 500), 100).unwrap();
        assert_eq!(s.links(100).len(), 2);
        assert_eq!(s.get("a", 100).unwrap().expires, 500);
        // b expired at 150
        assert_eq!(s.links(200).len(), 1);
        assert_eq!(s.get("b", 200), None);
        assert_eq!(s.get("", 100), None);
        s.remove("aaaaaaaaaa", 100).unwrap();
        assert_eq!(s.get("a", 100), None);
        let _ = std::fs::remove_dir_all(&d);
    }
}
