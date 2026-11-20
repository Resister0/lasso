//! The real `Http` for share links: reqwest (blocking) + rustls/ring, the same stack as Echo's lib_net.rs.
//! Blocking - worker threads only. A client is made per call (share links are rare; nothing stays in memory).

use super::api::{Http, Resp};
use std::time::Duration;

pub struct Net;

const UA: &str = concat!("Lasso/", env!("CARGO_PKG_VERSION"));

/// The error as text WITHOUT any URL: a presigned upload URL carries a credential + signature (valid for hours)
/// and lasso.log goes out with bug reports
fn describe(e: reqwest::Error) -> String {
    use std::error::Error;
    let e = e.without_url();
    let mut s = e.to_string();
    let mut src = e.source();
    while let Some(x) = src {
        s.push_str(": ");
        s.push_str(&x.to_string());
        src = x.source();
    }
    strip_queries(&s)
}

/// Belt and braces: anything after a "?" in a URL-looking word is cut ("https://x/y?X-Amz-Signature=..." -> "https://x/y?...")
pub fn strip_queries(s: &str) -> String {
    s.split(' ')
        .map(|w| match (w.contains("://"), w.find('?')) {
            (true, Some(i)) => format!("{}?...", &w[..i]),
            _ => w.to_string(),
        })
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    #[test]
    fn no_signed_urls_in_text() {
        let t = super::strip_queries("error sending request for url (https://r2.example/u/1/Lasso/a.png?X-Amz-Credential=AK&X-Amz-Signature=abc): timed out");
        assert!(!t.contains("Signature") && !t.contains("Credential"), "{t}");
        assert!(t.contains("timed out"));
    }
}

impl Http for Net {
    fn call(&self, method: &str, url: &str, headers: &[(&str, String)], body: Option<Vec<u8>>, timeout_s: u64) -> Result<Resp, String> {
        if rustls::crypto::CryptoProvider::get_default().is_none() {
            let _ = rustls::crypto::ring::default_provider().install_default();
        }
        let c = reqwest::blocking::Client::builder()
            .user_agent(UA)
            .connect_timeout(Duration::from_secs(15))
            .timeout(Duration::from_secs(timeout_s))
            // a presigned R2 link or the API never redirects; never follow one with the token on it
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|e| format!("network setup failed: {e}"))?;
        let m = reqwest::Method::from_bytes(method.as_bytes()).map_err(|e| e.to_string())?;
        let mut r = c.request(m, url);
        for (k, v) in headers {
            // reqwest sets content-length from the body itself
            if !k.eq_ignore_ascii_case("content-length") {
                r = r.header(*k, v);
            }
        }
        if let Some(b) = body {
            r = r.body(b);
        }
        let resp = r.send().map_err(describe)?;
        let code = resp.status().as_u16();
        let body = resp.text().map_err(describe)?;
        Ok(Resp { code, body })
    }
}
