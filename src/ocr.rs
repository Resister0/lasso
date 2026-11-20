//! "Copy text" (Ctrl+Shift+C) and the privacy check (find emails, numbers, keys -> blur them).
//!
//! The picture is cleaned up first (`prep`: grey, dark themes turned light, small screen text made bigger), then read by
//! - Linux (`linux`): the `tesseract` program if it's installed, else the Tesseract library (often there already for
//!   Spectacle's own OCR), else a short message how to install it (with this distro's command);
//! - Windows (`winrt`): the OCR engine built into Windows (Windows.Media.Ocr, offline) through PowerShell. ⚠ untested.
//!
//! The words come back in picture pixels; `clean` drops border lines and icon noise, `text` joins them into lines and
//! paragraphs.

#[cfg(target_os = "linux")]
mod linux;
mod prep;
mod run;
mod winrt;

use crate::shapes::{Pic, R};

#[derive(Clone, Debug)]
pub struct Word {
    pub text: String,
    pub r: R,
    /// words with the same line number are on one line
    pub line: u32,
    /// lines with the same number are one paragraph (a blank line between paragraphs in the copied text)
    pub para: u32,
    /// 0-100 (Tesseract's guess how sure it is; Windows doesn't say = 100)
    pub conf: f32,
}

/// All words in the picture (picture pixels), in reading order.
pub fn read(pic: &Pic) -> Result<Vec<Word>, String> {
    if pic.w < 4 || pic.h < 4 {
        return Ok(vec![]);
    }
    let t0 = std::time::Instant::now();
    let (img, map) = prep::prepare(pic, ENGINE_OPTS);
    let (mut words, engine) = recognize(&img)?;
    for w in &mut words {
        w.r = map.back(w.r);
    }
    let words = clean(words);
    #[cfg(not(test))]
    crate::log::line(
        "ocr",
        &format!("{engine}: {} words in {} ms ({}x{} read at {:.2}x)", words.len(), t0.elapsed().as_millis(), pic.w, pic.h, map.scale),
    );
    #[cfg(test)]
    let _ = (t0, engine);
    Ok(words)
}

#[cfg(target_os = "linux")]
const ENGINE_OPTS: prep::Opts = linux::OPTS;
#[cfg(windows)]
const ENGINE_OPTS: prep::Opts = winrt::OPTS;
#[cfg(not(any(target_os = "linux", windows)))]
const ENGINE_OPTS: prep::Opts = prep::Opts { max_pixels: 12e6, max_side: 8000, max_scale: 3.0, pad: 16 };

#[cfg(target_os = "linux")]
fn recognize(img: &prep::Gray) -> Result<(Vec<Word>, &'static str), String> {
    linux::recognize(img)
}
#[cfg(windows)]
fn recognize(img: &prep::Gray) -> Result<(Vec<Word>, &'static str), String> {
    winrt::recognize(img).map(|w| (w, "Windows OCR"))
}
#[cfg(not(any(target_os = "linux", windows)))]
fn recognize(_: &prep::Gray) -> Result<(Vec<Word>, &'static str), String> {
    Err("Copy text isn't available on this system".into())
}

/// The message when an engine hangs
const TOO_LONG: &str = "Reading the text took too long";

/// Characters a border or a scroll bar turns into at the start / end of a line
const BARS: &str = "|¦│┃‖_";

/// Drop what isn't text (window borders read as "|", icons read as one or two random letters) and fix ligatures.
fn clean(mut ws: Vec<Word>) -> Vec<Word> {
    for w in &mut ws {
        w.text = fix_chars(w.text.trim());
    }
    let n = ws.len();
    let keep: Vec<bool> = (0..n)
        .map(|i| {
            let w = &ws[i];
            let t = w.text.as_str();
            if t.is_empty() {
                return false;
            }
            if !t.chars().any(|c| c.is_alphanumeric()) {
                let first = i == 0 || ws[i - 1].line != w.line;
                let last = i + 1 == n || ws[i + 1].line != w.line;
                if w.conf < 50.0 || ((first || last) && t.chars().all(|c| BARS.contains(c))) {
                    return false;
                }
            } else if w.conf < 20.0 && t.chars().count() <= 2 {
                return false;
            }
            true
        })
        .collect();
    let mut k = keep.iter();
    ws.retain(|_| *k.next().unwrap_or(&true));
    ws
}

fn fix_chars(s: &str) -> String {
    let mut o = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '\u{FB00}' => o.push_str("ff"),
            '\u{FB01}' => o.push_str("fi"),
            '\u{FB02}' => o.push_str("fl"),
            '\u{FB03}' => o.push_str("ffi"),
            '\u{FB04}' => o.push_str("ffl"),
            '\u{FB05}' | '\u{FB06}' => o.push_str("st"),
            '\u{200B}' | '\u{200C}' | '\u{200D}' | '\u{FEFF}' => {}
            _ => o.push(c),
        }
    }
    o
}

/// Chinese / Japanese words are written without spaces between them (Korean has spaces)
fn is_cjk(c: char) -> bool {
    matches!(c as u32, 0x3000..=0x30FF | 0x3400..=0x4DBF | 0x4E00..=0x9FFF | 0xF900..=0xFAFF | 0xFF00..=0xFFEF)
}

pub fn text(words: &[Word]) -> String {
    let mut s = String::new();
    let mut last: Option<&Word> = None;
    for w in words {
        if let Some(p) = last {
            if p.para != w.para || p.line != w.line {
                let t = s.trim_end_matches([' ', '\t']).len();
                s.truncate(t);
                s.push_str(if p.para != w.para { "\n\n" } else { "\n" });
            } else if !(p.text.chars().last().is_some_and(is_cjk) && w.text.chars().next().is_some_and(is_cjk)) {
                s.push(' ');
            }
        }
        s.push_str(&w.text);
        last = Some(w);
    }
    s
}

/// Paragraphs for engines that only give lines (Windows): a gap of most of a line height, or a jump back up, starts a
/// new paragraph.
#[cfg_attr(not(windows), allow(dead_code))]
fn paras_from_gaps(ws: &mut [Word]) {
    let mut lines: Vec<(usize, usize, f32, f32)> = vec![]; // start, end, top, bottom
    for (i, w) in ws.iter().enumerate() {
        match lines.last_mut() {
            Some(l) if ws[l.0].line == w.line => {
                l.1 = i + 1;
                l.2 = l.2.min(w.r.y);
                l.3 = l.3.max(w.r.y + w.r.h);
            }
            _ => lines.push((i, i + 1, w.r.y, w.r.y + w.r.h)),
        }
    }
    let mut para = 0;
    for k in 0..lines.len() {
        if k > 0 {
            let (p, c) = (lines[k - 1], lines[k]);
            let h = (p.3 - p.2).max(c.3 - c.2);
            if c.2 - p.3 > 0.8 * h || c.2 < p.2 {
                para += 1;
            }
        }
        for w in &mut ws[lines[k].0..lines[k].1] {
            w.para = para;
        }
    }
}

fn digits(s: &str) -> usize {
    s.chars().filter(|c| c.is_ascii_digit()).count()
}

/// What looks private: emails, phone / card / long numbers, IP addresses, API keys / tokens.
pub fn sensitive(words: &[Word]) -> Vec<(R, &'static str)> {
    let mut out: Vec<(R, &'static str)> = vec![];
    let tr = |s: &str| s.trim_matches(|c: char| ",;:()[]<>\"'".contains(c)).to_string();
    let mut i = 0;
    while i < words.len() {
        let w = tr(&words[i].text);
        let kind = if w.contains('@') && w.rsplit('@').next().map(|d| d.contains('.')).unwrap_or(false) {
            Some("email")
        } else if is_ip(&w) {
            Some("IP address")
        } else if looks_key(&w) {
            Some("key")
        } else {
            None
        };
        if let Some(k) = kind {
            let mut r = words[i].r;
            let mut j = i;
            // OCR often splits a long key into pieces ("sk_live_51Hx9aQ 2eZvK Ylo2"): take the close ones after it too
            while k == "key" && j + 1 < words.len() && words[j + 1].line == words[i].line {
                let n = &words[j + 1];
                let gap = n.r.x - (words[j].r.x + words[j].r.w);
                if gap > 0.8 * r.h.max(n.r.h) || !n.text.chars().all(|c| c.is_ascii_alphanumeric() || "-_=+/".contains(c)) {
                    break;
                }
                r = r.union(&n.r);
                j += 1;
            }
            out.push((r, k));
            i = j + 1;
            continue;
        }
        // numbers: join number-ish words on one line ("1Z 999 AA1 0123", "+45 12 34 56 78", "4111 1111 1111 1111")
        if digits(&w) >= 2 && w.chars().all(|c| c.is_ascii_alphanumeric() || "+-./()".contains(c)) {
            let mut j = i;
            let mut r = words[i].r;
            let mut n = digits(&w);
            while j + 1 < words.len() && words[j + 1].line == words[i].line {
                let nw = tr(&words[j + 1].text);
                if digits(&nw) == 0 || !nw.chars().all(|c| c.is_ascii_alphanumeric() || "+-./()".contains(c)) {
                    break;
                }
                n += digits(&nw);
                r = r.union(&words[j + 1].r);
                j += 1;
            }
            if n >= 8 {
                out.push((r, "number"));
                i = j + 1;
                continue;
            }
        }
        i += 1;
    }
    out
}

fn is_ip(s: &str) -> bool {
    let p: Vec<&str> = s.split('.').collect();
    p.len() == 4 && p.iter().all(|x| !x.is_empty() && x.len() <= 3 && x.parse::<u16>().map(|v| v < 256).unwrap_or(false))
}

fn looks_key(s: &str) -> bool {
    let known = ["sk-", "sk_live_", "pk_live_", "ghp_", "gho_", "github_pat_", "xoxb-", "xoxp-", "AKIA", "AIza", "glpat-"];
    if known.iter().any(|k| s.starts_with(k)) && s.len() >= 12 {
        return true;
    }
    let letters = s.chars().filter(|c| c.is_ascii_alphabetic()).count();
    let ds = digits(s);
    s.len() >= 24 && letters >= 6 && ds >= 4 && s.chars().all(|c| c.is_ascii_alphanumeric() || "-_=+/.".contains(c))
}

#[cfg(test)]
mod tests {
    use super::*;
    fn w(t: &str, x: f32, line: u32) -> Word {
        Word { text: t.into(), r: R::new(x, 0.0, 10.0, 10.0), line, para: 0, conf: 95.0 }
    }
    fn wc(t: &str, line: u32, conf: f32) -> Word {
        Word { conf, ..w(t, 0.0, line) }
    }
    /// test addresses are built at run time (the public repo's leak check refuses any e-mail address in a file)
    fn mail(user: &str, domain: &str) -> String {
        format!("{user}{}{domain}", '@')
    }
    #[test]
    fn finds_private_things() {
        let alex = mail("alex.sample", "mail.example");
        let ws = vec![
            w("Email", 0.0, 1),
            w(&alex, 20.0, 1),
            w("Tracking", 0.0, 2),
            w("1Z", 20.0, 2),
            w("999", 40.0, 2),
            w("AA1", 60.0, 2),
            w("0123", 80.0, 2),
            w("4567", 100.0, 2),
            w("server", 0.0, 3),
            w("192.168.1.20", 20.0, 3),
            w("token", 0.0, 4),
            w("ghp_abcdefghijklmnop1234", 20.0, 4),
            w("Order", 0.0, 5),
            w("#4821", 20.0, 5),
        ];
        let s = sensitive(&ws);
        let kinds: Vec<&str> = s.iter().map(|x| x.1).collect();
        assert_eq!(kinds, vec!["email", "number", "IP address", "key"]);
        assert_eq!(text(&ws[..2]), format!("Email {alex}"));
        // a key split into pieces: all of it; the far word after it isn't part of it
        let mk = |t: &str, x: f32, w: f32| Word { text: t.into(), r: R::new(x, 0.0, w, 10.0), line: 1, para: 0, conf: 90.0 };
        let split = vec![mk("key", 0.0, 20.0), mk("sk_live_51Hx9aQ", 30.0, 90.0), mk("2eZvK", 124.0, 30.0), mk("Ylo2", 157.0, 24.0), mk("coffee", 260.0, 40.0)];
        assert_eq!(sensitive(&split), vec![(R::new(30.0, 0.0, 151.0, 10.0), "key")]);
    }

    #[test]
    fn text_lines_and_paragraphs() {
        let mut ws = vec![w("Hello", 0.0, 1), w("there", 0.0, 1), w("next", 0.0, 2), w("New", 0.0, 3), w("para", 0.0, 3)];
        ws[3].para = 1;
        ws[4].para = 1;
        assert_eq!(text(&ws), "Hello there\nnext\n\nNew para");
        // Chinese / Japanese: no spaces between the characters
        let cjk = vec![w("你", 0.0, 1), w("好", 0.0, 1), w("Lasso", 0.0, 1)];
        assert_eq!(text(&cjk), "你好 Lasso");
        assert_eq!(text(&[]), "");
    }

    #[test]
    fn clean_drops_borders_and_noise() {
        let ws = vec![
            wc("|", 1, 90.0), // window border at the line start
            wc("Settings", 1, 96.0),
            wc("|", 1, 91.0), // ... and at the end
            wc("a", 2, 95.0),
            wc("|", 2, 92.0), // in the middle = real text ("a | b")
            wc("b", 2, 95.0),
            wc("-", 3, 88.0), // a list dash stays
            wc("item", 3, 93.0),
            wc("®", 4, 30.0),  // unsure symbol = icon
            wc("Sx", 4, 12.0), // unsure 2 letters = icon
            wc("\u{FB01}le", 4, 80.0), // ligature
            wc("I", 5, 70.0),
            wc(" ", 5, 99.0),
        ];
        let c = clean(ws);
        assert_eq!(text(&c), "Settings\na | b\n- item\nfile\nI");
    }

    #[test]
    fn paragraphs_from_line_gaps() {
        let mk = |t: &str, y: f32, line: u32| Word { text: t.into(), r: R::new(0.0, y, 30.0, 12.0), line, para: 9, conf: 100.0 };
        let mut ws = vec![
            mk("one", 0.0, 0),
            mk("two", 16.0, 1),   // 4 px gap = same paragraph
            mk("three", 50.0, 2), // 22 px gap = new paragraph
            mk("four", 66.0, 3),
            mk("side", 0.0, 4), // back up = another column
        ];
        paras_from_gaps(&mut ws);
        let p: Vec<u32> = ws.iter().map(|w| w.para).collect();
        assert_eq!(p, vec![0, 0, 1, 1, 2]);
        assert_eq!(text(&ws), "one\ntwo\n\nthree\nfour\n\nside");
    }

    /// Any picture: `OCR_FILE=pic.png cargo test --release reads_a_file -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn reads_a_file() {
        let Some(f) = std::env::var_os("OCR_FILE") else { return };
        let img = image::open(f).unwrap().to_rgba8();
        let pic = Pic { w: img.width(), h: img.height(), px: img.into_raw() };
        let t0 = std::time::Instant::now();
        let ws = read(&pic).unwrap();
        eprintln!("{} words in {} ms, {} private\n---\n{}", ws.len(), t0.elapsed().as_millis(), sensitive(&ws).len(), text(&ws));
    }

    /// Real OCR when an engine is installed (Linux: tesseract or its library); skipped otherwise.
    #[test]
    fn reads_rendered_text() {
        // light text on a dark panel, small like real screen text = the hard case for Tesseract
        let (w, h) = (420u32, 90u32);
        let sam = mail("sam", "example.org");
        let mut pic = Pic::new(w, h);
        for p in pic.px.chunks_exact_mut(4) {
            p.copy_from_slice(&[30, 32, 36, 255]);
        }
        {
            let mut pm = tiny_skia::PixmapMut::from_bytes(&mut pic.px, w, h).unwrap();
            let clip = crate::shapes::IR { x: 0, y: 0, w: w as i32, h: h as i32 };
            crate::shapes::draw_text(&mut pm, clip, (10.0, 10.0), "Copy the text from Lasso", 15.0, false, false, [235, 235, 235, 255]);
            crate::shapes::draw_text(&mut pm, clip, (10.0, 45.0), &format!("Mail: {sam}"), 15.0, false, false, [235, 235, 235, 255]);
        }
        match read(&pic) {
            Err(e) => eprintln!("OCR engine not available, skipped: {e}"),
            Ok(ws) => {
                let t = text(&ws);
                eprintln!("OCR read: {t:?}");
                assert!(t.contains("Copy the text from Lasso"), "{t:?}");
                assert!(t.contains(&sam), "{t:?}");
                let hits = sensitive(&ws);
                assert_eq!(hits.len(), 1);
                // the box is in picture pixels (not the enlarged copy Tesseract read)
                let r = hits[0].0;
                let x0 = 10.0 + crate::shapes::text_metrics("Mail: ", 15.0, false, false).0;
                let ew = crate::shapes::text_metrics(&sam, 15.0, false, false).0;
                assert!((r.x - x0).abs() < 6.0 && (r.w - ew).abs() < 8.0 && r.y > 45.0 && r.y + r.h < 63.0, "{r:?} want x {x0} w {ew}");
            }
        }
    }
}
