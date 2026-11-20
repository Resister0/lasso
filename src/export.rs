//! Turning the finished picture into a file / clipboard bytes.
//!
//! "Fits Discord, always" (notes 6c #1): a 3-monitor or 4K PNG can pass Discord's 10 MB free limit. Copy / save /
//! link first try a smaller lossless PNG, then high-quality JPEG, then a scaled copy - the user never sees "file
//! too large". Every save writes a brand-new file (never patches an old one: the aCropalypse lesson).

use crate::shapes::{Pic, IR};
use std::path::{Path, PathBuf};

pub const DISCORD_LIMIT: usize = 10 * 1000 * 1000;

/// PNG of an opaque or straight-alpha picture. `fast` = clipboard (speed first), else smaller file.
pub fn png(pic: &Pic, fast: bool) -> Vec<u8> {
    let mut out = Vec::with_capacity(pic.px.len() / 3);
    {
        let mut enc = png::Encoder::new(&mut out, pic.w, pic.h);
        enc.set_color(png::ColorType::Rgba);
        enc.set_depth(png::BitDepth::Eight);
        enc.set_compression(if fast { png::Compression::Fast } else { png::Compression::Balanced });
        enc.set_filter(png::Filter::Adaptive);
        let opaque = pic.px.chunks_exact(4).all(|p| p[3] == 255);
        if opaque {
            // RGB is ~25 % smaller
            enc.set_color(png::ColorType::Rgb);
            let rgb: Vec<u8> = pic.px.chunks_exact(4).flat_map(|p| [p[0], p[1], p[2]]).collect();
            if let Ok(mut w) = enc.write_header() {
                let _ = w.write_image_data(&rgb);
            }
        } else if let Ok(mut w) = enc.write_header() {
            let straight = unpremultiply(&pic.px);
            let _ = w.write_image_data(&straight);
        }
    }
    out
}

fn unpremultiply(px: &[u8]) -> Vec<u8> {
    px.chunks_exact(4)
        .flat_map(|p| {
            let a = p[3] as u32;
            if a == 0 || a == 255 {
                [p[0], p[1], p[2], p[3]]
            } else {
                [((p[0] as u32 * 255 + a / 2) / a).min(255) as u8, ((p[1] as u32 * 255 + a / 2) / a).min(255) as u8, ((p[2] as u32 * 255 + a / 2) / a).min(255) as u8, p[3]]
            }
        })
        .collect()
}

fn to_image(pic: &Pic) -> image::RgbaImage {
    image::RgbaImage::from_raw(pic.w, pic.h, unpremultiply(&pic.px)).unwrap_or_default()
}

pub fn jpeg(pic: &Pic, q: u8) -> Vec<u8> {
    let rgb = image::DynamicImage::ImageRgba8(to_image(pic)).to_rgb8();
    let mut out = std::io::Cursor::new(Vec::new());
    let enc = image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, q);
    let _ = rgb.write_with_encoder(enc);
    out.into_inner()
}

pub fn webp(pic: &Pic) -> Vec<u8> {
    let img = to_image(pic);
    let mut out = std::io::Cursor::new(Vec::new());
    let enc = image::codecs::webp::WebPEncoder::new_lossless(&mut out);
    let _ = img.write_with_encoder(enc);
    out.into_inner()
}

pub struct Encoded {
    pub bytes: Vec<u8>,
    pub ext: &'static str,
    pub mime: &'static str,
    /// Shown to the user when Lasso had to shrink it
    pub note: String,
}

/// The picture in the wanted format; with `fits` it is kept under Discord's 10 MB.
pub fn encode(pic: &Pic, format: &str, fits: bool, fast: bool) -> Encoded {
    let first = match format {
        "jpg" | "jpeg" => Encoded { bytes: jpeg(pic, 92), ext: "jpg", mime: "image/jpeg", note: String::new() },
        "webp" => Encoded { bytes: webp(pic), ext: "webp", mime: "image/webp", note: String::new() },
        _ => Encoded { bytes: png(pic, fast), ext: "png", mime: "image/png", note: String::new() },
    };
    if !fits || first.bytes.len() <= DISCORD_LIMIT {
        return first;
    }
    // 1. PNG, smaller settings
    if first.ext == "png" && fast {
        let b = png(pic, false);
        if b.len() <= DISCORD_LIMIT {
            return Encoded { bytes: b, note: String::new(), ..first };
        }
    }
    // 2. high quality JPEG
    for q in [92u8, 85] {
        let b = jpeg(pic, q);
        if b.len() <= DISCORD_LIMIT {
            return Encoded { bytes: b, ext: "jpg", mime: "image/jpeg", note: format!("Made smaller to fit Discord (JPEG {q} %)") };
        }
    }
    // 3. scaled down
    let mut img = to_image(pic);
    loop {
        let (w, h) = ((img.width() as f32 * 0.8) as u32, (img.height() as f32 * 0.8) as u32);
        if w < 64 || h < 64 {
            break;
        }
        img = image::imageops::resize(&img, w, h, image::imageops::FilterType::Lanczos3);
        let p = Pic { w, h, px: img.clone().into_raw() };
        let b = jpeg(&p, 88);
        if b.len() <= DISCORD_LIMIT {
            return Encoded { bytes: b, ext: "jpg", mime: "image/jpeg", note: format!("Made smaller to fit Discord ({w} × {h})") };
        }
    }
    first
}

/// A free file name in `dir` (adds " (2)", " (3)" ...)
pub fn free_name(dir: &Path, base: &str, ext: &str) -> PathBuf {
    let mut p = dir.join(format!("{base}.{ext}"));
    let mut n = 2;
    while p.exists() {
        p = dir.join(format!("{base} ({n}).{ext}"));
        n += 1;
    }
    p
}

/// Save as: where the bytes really go. The confirmed path when its extension is the kind written (jpeg = jpg);
/// else the right extension (replacing a picture one, appended to anything else: "shot" -> "shot.png") AND a free
/// name - a path the user never confirmed must never replace a file (review D10).
pub fn save_as_path(confirmed: &Path, ext: &str) -> PathBuf {
    let norm = |e: &str| if e == "jpeg" { "jpg".to_string() } else { e.to_string() };
    let have = confirmed.extension().map(|e| norm(&e.to_string_lossy().to_lowercase()));
    if have.as_deref() == Some(&norm(ext)) {
        return confirmed.to_path_buf();
    }
    let name = confirmed.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_else(|| "Lasso".into());
    let pictures = ["png", "jpg", "webp", "bmp", "gif", "tif", "tiff", "avif", "heic"];
    let base = match have {
        Some(h) if pictures.contains(&h.as_str()) => confirmed.file_stem().map(|n| n.to_string_lossy().to_string()).unwrap_or(name),
        _ => name,
    };
    free_name(confirmed.parent().unwrap_or(Path::new(".")), &base, ext)
}

/// Writes a new file (temp + rename: a crash never leaves half a picture under the real name).
pub fn write_new(path: &Path, bytes: &[u8]) -> Result<(), String> {
    if let Some(d) = path.parent() {
        std::fs::create_dir_all(d).map_err(|e| format!("can't make the folder: {e}"))?;
    }
    let tmp = path.with_extension(format!("{}.lasso-tmp", path.extension().map(|e| e.to_string_lossy().to_string()).unwrap_or_default()));
    {
        use std::io::Write;
        let mut f = std::fs::File::create(&tmp).map_err(|e| format!("can't write: {e}"))?;
        f.write_all(bytes).map_err(|e| format!("can't write: {e}"))?;
        // on the disk before the rename: a power cut never leaves a 0-byte file under the real name
        let _ = f.sync_all();
    }
    // rename replaces the old file in one step - a brand-new file, never the old bytes patched (aCropalypse)
    std::fs::rename(&tmp, path).map_err(|e| {
        let _ = std::fs::remove_file(&tmp);
        format!("can't write: {e}")
    })
}

/// "Background" for sharing (CleanShot): Lasso-teal gradient, padding, rounded corners, soft shadow.
pub fn background(pic: &Pic, pad: u32) -> Pic {
    use tiny_skia::*;
    let (w, h) = (pic.w + 2 * pad, pic.h + 2 * pad);
    let Some(mut pm) = Pixmap::new(w, h) else { return Pic { w: pic.w, h: pic.h, px: pic.px.clone() } };
    if let Some(sh) = LinearGradient::new(
        Point::from_xy(0.0, 0.0),
        Point::from_xy(w as f32, h as f32),
        vec![
            GradientStop::new(0.0, Color::from_rgba8(0x36, 0xc9, 0xa8, 255)),
            GradientStop::new(0.5, Color::from_rgba8(0x14, 0xa3, 0x8b, 255)),
            GradientStop::new(1.0, Color::from_rgba8(0x0d, 0x80, 0x85, 255)),
        ],
        SpreadMode::Pad,
        Transform::identity(),
    ) {
        let p = Paint { shader: sh, ..Paint::default() };
        pm.fill_rect(Rect::from_xywh(0.0, 0.0, w as f32, h as f32).unwrap(), &p, Transform::identity(), None);
    }
    let radius = 12.0f32.min(pic.w as f32 / 4.0).min(pic.h as f32 / 4.0);
    let rr = |x: f32, y: f32, ww: f32, hh: f32| -> Option<tiny_skia::Path> {
        let mut pb = PathBuilder::new();
        let k = 0.4477 * radius;
        let (x1, y1) = (x + ww, y + hh);
        pb.move_to(x + radius, y);
        pb.line_to(x1 - radius, y);
        pb.cubic_to(x1 - k, y, x1, y + k, x1, y + radius);
        pb.line_to(x1, y1 - radius);
        pb.cubic_to(x1, y1 - k, x1 - k, y1, x1 - radius, y1);
        pb.line_to(x + radius, y1);
        pb.cubic_to(x + k, y1, x, y1 - k, x, y1 - radius);
        pb.line_to(x, y + radius);
        pb.cubic_to(x, y + k, x + k, y, x + radius, y);
        pb.close();
        pb.finish()
    };
    // shadow
    if let Some(path) = rr(pad as f32, pad as f32 + 8.0, pic.w as f32, pic.h as f32) {
        let mut sp = Paint::default();
        sp.set_color_rgba8(0, 0, 0, 110);
        pm.fill_path(&path, &sp, FillRule::Winding, Transform::identity(), None);
        let mut m = pm.as_mut();
        crate::shapes::blur_px(&mut m, IR { x: pad as i32 - 24, y: pad as i32 - 16, w: pic.w as i32 + 48, h: pic.h as i32 + 48 }, 12);
    }
    // the picture, rounded
    if let (Some(path), Some(src)) = (rr(pad as f32, pad as f32, pic.w as f32, pic.h as f32), PixmapRef::from_bytes(&pic.px, pic.w, pic.h)) {
        let mut mask = Mask::new(w, h).unwrap();
        mask.fill_path(&path, FillRule::Winding, true, Transform::identity());
        pm.draw_pixmap(pad as i32, pad as i32, src, &PixmapPaint::default(), Transform::identity(), Some(&mask));
    }
    Pic { w, h, px: pm.take() }
}

/// Decodes any picture file Lasso can open (PNG / JPG / WebP / GIF / BMP) to premultiplied RGBA.
pub fn decode(bytes: &[u8]) -> Result<Pic, String> {
    let img = image::load_from_memory(bytes).map_err(|e| format!("not a picture Lasso can open ({e})"))?.to_rgba8();
    let (w, h) = (img.width(), img.height());
    let mut px = img.into_raw();
    for p in px.chunks_exact_mut(4) {
        let a = p[3] as u32;
        if a < 255 {
            for c in &mut p[..3] {
                *c = ((*c as u32 * a + 127) / 255) as u8;
            }
        }
    }
    Ok(Pic { w, h, px })
}

pub fn thumbnail(pic: &Pic, max: u32) -> Pic {
    let s = (max as f32 / pic.w.max(pic.h) as f32).min(1.0);
    let (w, h) = (((pic.w as f32 * s).round() as u32).max(1), ((pic.h as f32 * s).round() as u32).max(1));
    let img = image::RgbaImage::from_raw(pic.w, pic.h, pic.px.clone()).unwrap_or_default();
    let t = image::imageops::thumbnail(&img, w, h);
    Pic { w, h, px: t.into_raw() }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn encodes_and_decodes() {
        let mut p = Pic::new(40, 30);
        p.px.iter_mut().enumerate().for_each(|(i, v)| *v = if i % 4 == 3 { 255 } else { (i * 7 % 256) as u8 });
        for f in ["png", "jpg", "webp"] {
            let e = encode(&p, f, true, true);
            let d = decode(&e.bytes).unwrap();
            assert_eq!((d.w, d.h), (40, 30), "{f}");
        }
        let b = background(&p, 20);
        assert_eq!((b.w, b.h), (80, 70));
    }

    #[test]
    fn saving_never_overwrites_and_replaces_atomically() {
        let d = std::env::temp_dir().join(format!("lasso-export-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        // B3: a JPEG save next to an existing shot.jpg gets a new name (the name is chosen with the REAL extension)
        std::fs::write(d.join("shot.jpg"), b"old photo").unwrap();
        let p = free_name(&d, "shot", "jpg");
        assert_eq!(p.file_name().unwrap(), "shot (2).jpg");
        assert_eq!(std::fs::read(d.join("shot.jpg")).unwrap(), b"old photo");
        // B8: replacing goes through a temp file + rename: the old bytes are never removed first, no temp left
        write_new(&d.join("shot.jpg"), b"new").unwrap();
        assert_eq!(std::fs::read(d.join("shot.jpg")).unwrap(), b"new");
        let left: Vec<String> = std::fs::read_dir(&d).unwrap().flatten().map(|e| e.file_name().to_string_lossy().to_string()).collect();
        assert_eq!(left, vec!["shot.jpg".to_string()]);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn fits_discord_with_a_huge_picture() {
        // 4000 x 3000 of noise: a PNG of ~45 MB; must come out under 10 MB
        let mut p = Pic::new(4000, 3000);
        let mut x: u32 = 1;
        for (i, v) in p.px.iter_mut().enumerate() {
            x ^= x << 13;
            x ^= x >> 17;
            x ^= x << 5;
            *v = if i % 4 == 3 { 255 } else { x as u8 };
        }
        let e = encode(&p, "png", true, true);
        assert!(e.bytes.len() <= DISCORD_LIMIT, "{} bytes", e.bytes.len());
        assert!(!e.note.is_empty());
        let d = decode(&e.bytes).unwrap();
        assert!(d.w >= 1000);
    }

    #[test]
    fn save_as_never_replaces_a_path_nobody_confirmed() {
        // D10: Fits Discord png -> jpg, no extension, an unknown kind: a free name next to it, never an overwrite
        let d = std::env::temp_dir().join(format!("lasso-saveas-test-{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        assert_eq!(super::save_as_path(&d.join("a.png"), "png"), d.join("a.png"));
        assert_eq!(super::save_as_path(&d.join("a.JPEG"), "jpg"), d.join("a.JPEG"));
        std::fs::write(d.join("a.jpg"), b"someone's photo").unwrap();
        assert_eq!(super::save_as_path(&d.join("a.png"), "jpg"), d.join("a (2).jpg"));
        assert_eq!(super::save_as_path(&d.join("shot"), "png"), d.join("shot.png"));
        assert_eq!(super::save_as_path(&d.join("v1.2"), "png"), d.join("v1.2.png"));
        assert_eq!(super::save_as_path(&d.join("old.gif"), "png"), d.join("old.png"));
        let _ = std::fs::remove_dir_all(&d);
    }
}
