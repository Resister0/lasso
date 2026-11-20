//! Windows clipboard, written by hand (arboard re-encoded every picture as PNG on the UI thread):
//! - a picture = "PNG" (the PNG Lasso already made: keeps transparency; Discord, browsers, Office take it) +
//!   CF_DIBV5 (bottom-up, for Paint / Word / older apps; Windows makes CF_DIB / CF_BITMAP from it). Written on a
//!   worker thread, in order;
//! - files = CF_HDROP + "Preferred DropEffect" = copy (Explorer pastes copies, chat apps upload the files);
//! - reading a picture: "PNG", then a copied picture file, then the DIB (through arboard).
//! ⚠ Type-checked from Linux only; never run on a real Windows PC yet.

use crate::shapes::Pic;
use std::sync::mpsc::{channel, Sender};
use std::sync::Mutex;
use windows_sys::Win32::Foundation::{GlobalFree, HANDLE, HGLOBAL, HWND};
use windows_sys::Win32::System::DataExchange::*;
use windows_sys::Win32::System::Memory::{GlobalAlloc, GlobalLock, GlobalSize, GlobalUnlock, GMEM_MOVEABLE};
use windows_sys::Win32::System::Ole::{CF_DIBV5, CF_HDROP};

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// The clipboard is shared by every app: someone else may have it open for a moment
fn open(owner: HWND) -> bool {
    for _ in 0..20 {
        if unsafe { OpenClipboard(owner) } != 0 {
            return true;
        }
        std::thread::sleep(std::time::Duration::from_millis(15));
    }
    false
}

unsafe fn put(format: u32, data: &[u8]) -> bool {
    let h = GlobalAlloc(GMEM_MOVEABLE, data.len().max(1));
    if h.is_null() {
        return false;
    }
    let p = GlobalLock(h) as *mut u8;
    if p.is_null() {
        GlobalFree(h);
        return false;
    }
    std::ptr::copy_nonoverlapping(data.as_ptr(), p, data.len());
    GlobalUnlock(h);
    if SetClipboardData(format, h as HANDLE).is_null() {
        GlobalFree(h);
        return false;
    }
    true
}

fn format(name: &str) -> u32 {
    let n = wide(name);
    unsafe { RegisterClipboardFormatW(n.as_ptr()) }
}

/// BITMAPV5HEADER (124 bytes) + bottom-up BGRA rows, straight alpha
fn dibv5(p: &Pic) -> Vec<u8> {
    let (w, h) = (p.w as usize, p.h as usize);
    let mut v = Vec::with_capacity(124 + w * h * 4);
    let le32 = |v: &mut Vec<u8>, x: u32| v.extend_from_slice(&x.to_le_bytes());
    le32(&mut v, 124); // bV5Size
    le32(&mut v, w as u32); // width
    le32(&mut v, h as u32); // height > 0 = bottom-up (Word can't paste top-down DIBs)
    v.extend_from_slice(&1u16.to_le_bytes()); // planes
    v.extend_from_slice(&32u16.to_le_bytes()); // bits
    le32(&mut v, 3); // BI_BITFIELDS
    le32(&mut v, (w * h * 4) as u32);
    le32(&mut v, 2835); // 72 dpi
    le32(&mut v, 2835);
    le32(&mut v, 0);
    le32(&mut v, 0);
    le32(&mut v, 0x00ff_0000); // red mask
    le32(&mut v, 0x0000_ff00);
    le32(&mut v, 0x0000_00ff);
    le32(&mut v, 0xff00_0000); // alpha mask
    le32(&mut v, 0x7352_4742); // LCS_sRGB
    v.extend_from_slice(&[0u8; 36]); // endpoints
    le32(&mut v, 0);
    le32(&mut v, 0);
    le32(&mut v, 0); // gamma
    le32(&mut v, 4); // LCS_GM_IMAGES
    le32(&mut v, 0);
    le32(&mut v, 0);
    le32(&mut v, 0);
    debug_assert_eq!(v.len(), 124);
    for y in (0..h).rev() {
        for q in p.px[y * w * 4..(y + 1) * w * 4].chunks_exact(4) {
            let a = q[3] as u32;
            let un = |c: u8| if a == 0 || a == 255 { c } else { ((c as u32 * 255 + a / 2) / a).min(255) as u8 };
            v.extend_from_slice(&[un(q[2]), un(q[1]), un(q[0]), q[3]]);
        }
    }
    v
}

enum Job {
    Image { bytes: Vec<u8>, pic: Option<Pic> },
    Files(Vec<String>),
}

static WORKER: Mutex<Option<Sender<Job>>> = Mutex::new(None);

fn send(job: Job) {
    let mut g = WORKER.lock().unwrap();
    if g.is_none() {
        let (tx, rx) = channel::<Job>();
        let ok = std::thread::Builder::new()
            .name("clipboard".into())
            .spawn(move || {
                for j in rx {
                    let r = match j {
                        Job::Image { bytes, pic } => write_image(&bytes, pic),
                        Job::Files(f) => write_files(&f),
                    };
                    if let Err(e) = r {
                        crate::log::line("clipboard", &e);
                    }
                }
            })
            .is_ok();
        if !ok {
            crate::log::line("clipboard", "no worker thread");
            return;
        }
        *g = Some(tx);
    }
    if let Some(tx) = g.as_ref() {
        let _ = tx.send(job);
    }
}

/// A finished picture (encoded `bytes`: PNG, or JPEG / WebP when "Fits Discord" made it smaller). `pic` if the
/// caller has the pixels already (saves decoding).
pub fn set_image(bytes: Vec<u8>, pic: Option<Pic>) {
    send(Job::Image { bytes, pic });
}

pub fn set_files(paths: &[&str]) {
    send(Job::Files(paths.iter().map(|s| s.to_string()).collect()));
}

fn write_image(bytes: &[u8], pic: Option<Pic>) -> Result<(), String> {
    let is_png = bytes.starts_with(b"\x89PNG");
    let pic = match pic {
        Some(p) => p,
        None => crate::export::decode(bytes)?,
    };
    let png = if is_png { std::borrow::Cow::Borrowed(bytes) } else { std::borrow::Cow::Owned(crate::export::png(&pic, true)) };
    let dib = dibv5(&pic);
    drop(pic);
    let png_fmt = format("PNG");
    if !open(super::tray_hwnd()) {
        return Err("the clipboard is busy (another app keeps it open)".into());
    }
    unsafe {
        EmptyClipboard();
        // PNG first: apps take the first format they know
        let a = png_fmt != 0 && put(png_fmt, &png);
        let b = put(CF_DIBV5 as u32, &dib);
        CloseClipboard();
        if !a && !b {
            return Err("SetClipboardData failed".into());
        }
    }
    Ok(())
}

fn write_files(paths: &[String]) -> Result<(), String> {
    // DROPFILES { pFiles: 20, pt: 0,0, fNC: 0, fWide: 1 } + wide paths, each 0-ended, + a final 0
    let mut v: Vec<u8> = vec![];
    v.extend_from_slice(&20u32.to_le_bytes());
    v.extend_from_slice(&[0u8; 12]);
    v.extend_from_slice(&1u32.to_le_bytes());
    let mut n = 0;
    for p in paths {
        let full = std::fs::canonicalize(p).map(|x| x.to_string_lossy().trim_start_matches(r"\\?\").to_string()).unwrap_or_else(|_| p.clone());
        for c in full.encode_utf16().chain(std::iter::once(0)) {
            v.extend_from_slice(&c.to_le_bytes());
        }
        n += 1;
    }
    if n == 0 {
        return Ok(());
    }
    v.extend_from_slice(&[0, 0]);
    let effect = format("Preferred DropEffect");
    if !open(super::tray_hwnd()) {
        return Err("the clipboard is busy (another app keeps it open)".into());
    }
    unsafe {
        EmptyClipboard();
        let ok = put(CF_HDROP as u32, &v);
        if effect != 0 {
            put(effect, &1u32.to_le_bytes()); // DROPEFFECT_COPY
        }
        CloseClipboard();
        if !ok {
            return Err("SetClipboardData (files) failed".into());
        }
    }
    Ok(())
}

unsafe fn get(format: u32) -> Option<Vec<u8>> {
    if format == 0 || IsClipboardFormatAvailable(format) == 0 {
        return None;
    }
    let h = GetClipboardData(format) as HGLOBAL;
    if h.is_null() {
        return None;
    }
    let p = GlobalLock(h) as *const u8;
    if p.is_null() {
        return None;
    }
    let v = std::slice::from_raw_parts(p, GlobalSize(h)).to_vec();
    GlobalUnlock(h);
    Some(v)
}

/// The first file of a CF_HDROP block
fn first_file(v: &[u8]) -> Option<String> {
    let off = u32::from_le_bytes(v.get(0..4)?.try_into().ok()?) as usize;
    let wide_names = u32::from_le_bytes(v.get(16..20)?.try_into().ok()?) != 0;
    let rest = v.get(off..)?;
    if wide_names {
        let u: Vec<u16> = rest.chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).take_while(|c| *c != 0).collect();
        (!u.is_empty()).then(|| String::from_utf16_lossy(&u))
    } else {
        let b: Vec<u8> = rest.iter().copied().take_while(|c| *c != 0).collect();
        (!b.is_empty()).then(|| String::from_utf8_lossy(&b).to_string())
    }
}

/// A picture from the clipboard (Ctrl+V on the frozen screen)
pub fn read_image() -> Result<Pic, String> {
    let none = || "No picture on the clipboard".to_string();
    let (png, file) = unsafe {
        if !open(std::ptr::null_mut()) {
            return Err("the clipboard is busy (another app keeps it open)".into());
        }
        let png = get(format("PNG"));
        let file = if png.is_none() { get(CF_HDROP as u32).and_then(|v| first_file(&v)) } else { None };
        CloseClipboard();
        (png, file)
    };
    if let Some(b) = png {
        if let Ok(p) = crate::export::decode(&b) {
            return Ok(p);
        }
    }
    if let Some(f) = file {
        return std::fs::read(&f).map_err(|e| e.to_string()).and_then(|b| crate::export::decode(&b));
    }
    // a DIB (screenshots from other tools, Paint): arboard reads those well
    arboard::Clipboard::new().and_then(|mut c| c.get_image()).map_err(|_| none()).map(|img| {
        let mut px = img.bytes.into_owned();
        for p in px.chunks_exact_mut(4) {
            let a = p[3] as u32;
            for c in &mut p[..3] {
                *c = ((*c as u32 * a + 127) / 255) as u8;
            }
        }
        Pic { w: img.width as u32, h: img.height as u32, px }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dib_is_bottom_up_bgra() {
        let p = Pic { w: 1, h: 2, px: vec![255, 0, 0, 255, 0, 0, 255, 255] };
        let d = dibv5(&p);
        assert_eq!(d.len(), 124 + 8);
        // last row first: the blue pixel, as B G R A
        assert_eq!(&d[124..128], &[255, 0, 0, 255]);
        assert_eq!(&d[128..132], &[0, 0, 255, 255]);
    }

    #[test]
    fn hdrop_first_file() {
        let mut v = vec![];
        v.extend_from_slice(&20u32.to_le_bytes());
        v.extend_from_slice(&[0u8; 12]);
        v.extend_from_slice(&1u32.to_le_bytes());
        for c in "C:\\a b.png".encode_utf16().chain([0, 0]) {
            v.extend_from_slice(&c.to_le_bytes());
        }
        assert_eq!(first_file(&v).as_deref(), Some("C:\\a b.png"));
    }
}
