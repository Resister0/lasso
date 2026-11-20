//! The picture as the OCR engines like it: grey, dark text on a light background, contrast stretched, screen-sized
//! text made 2-3x bigger (Tesseract reads 10-14 px screen text badly), a white border around it (text that touches
//! the edge gets lost). `Map` turns the engine's boxes back into picture pixels.

use crate::shapes::{Pic, R};

pub struct Gray {
    pub w: u32,
    pub h: u32,
    pub px: Vec<u8>,
}

#[derive(Clone, Copy, Debug)]
pub struct Opts {
    /// enlarge at most to about this many pixels (time + memory)
    pub max_pixels: f32,
    /// longest side the engine takes (Windows OCR: 2600) - bigger pictures are made smaller
    pub max_side: u32,
    pub max_scale: f32,
    /// white border, in pixels of the enlarged picture
    pub pad: u32,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Map {
    pub scale: f32,
    pub pad: f32,
}

impl Map {
    pub fn back(&self, r: R) -> R {
        let x = ((r.x - self.pad) / self.scale).max(0.0);
        let y = ((r.y - self.pad) / self.scale).max(0.0);
        R::new(x, y, r.w / self.scale, r.h / self.scale)
    }
}

pub fn prepare(pic: &Pic, o: Opts) -> (Gray, Map) {
    let mut g = gray_on_white(pic);
    if is_dark(&g) {
        for p in &mut g.px {
            *p = 255 - *p;
        }
    }
    stretch(&mut g);
    let s = pick_scale(g.w, g.h, o);
    let (g, s) = if (s - 1.0).abs() > 0.001 { resize(&g, s) } else { (g, 1.0) };
    (pad(&g, o.pad), Map { scale: s, pad: o.pad as f32 })
}

/// Lasso's pictures are premultiplied RGBA: put them on white, then luma.
fn gray_on_white(pic: &Pic) -> Gray {
    let px = pic
        .px
        .chunks_exact(4)
        .map(|p| {
            let bg = 255 - p[3] as u32;
            let (r, g, b) = ((p[0] as u32 + bg).min(255), (p[1] as u32 + bg).min(255), (p[2] as u32 + bg).min(255));
            ((r * 77 + g * 150 + b * 29) >> 8) as u8
        })
        .collect();
    Gray { w: pic.w, h: pic.h, px }
}

fn histogram(g: &Gray) -> [u32; 256] {
    let mut h = [0u32; 256];
    for &p in &g.px {
        h[p as usize] += 1;
    }
    h
}

/// The grey level below which `frac` of the pixels are.
fn percentile(h: &[u32; 256], total: u32, frac: f32) -> u8 {
    let want = (total as f32 * frac) as u32;
    let mut n = 0;
    for (i, c) in h.iter().enumerate() {
        n += c;
        if n > want {
            return i as u8;
        }
    }
    255
}

/// Mostly dark = a dark theme: light text on dark (the engines want it the other way round).
fn is_dark(g: &Gray) -> bool {
    !g.px.is_empty() && percentile(&histogram(g), g.px.len() as u32, 0.5) < 128
}

/// Spread the grey levels between the 1st and 99th percentile over 0..255 (greyed-out / low-contrast text).
fn stretch(g: &mut Gray) {
    if g.px.is_empty() {
        return;
    }
    let h = histogram(g);
    let n = g.px.len() as u32;
    let (lo, hi) = (percentile(&h, n, 0.01) as i32, percentile(&h, n, 0.99) as i32);
    if hi - lo < 24 || (lo == 0 && hi == 255) {
        return;
    }
    let lut: Vec<u8> = (0..256).map(|v| (((v - lo) * 255) / (hi - lo)).clamp(0, 255) as u8).collect();
    for p in &mut g.px {
        *p = lut[*p as usize];
    }
}

/// Whole steps up (2x, 3x keep thin strokes even) while the picture stays under `max_pixels`; only ever smaller to fit
/// `max_side`.
fn pick_scale(w: u32, h: u32, o: Opts) -> f32 {
    let (wf, hf) = (w.max(1) as f32, h.max(1) as f32);
    let mut s = (o.max_pixels / (wf * hf)).sqrt().floor().clamp(1.0, o.max_scale.max(1.0));
    let room = o.max_side.saturating_sub(2 * o.pad).max(1) as f32;
    let long = wf.max(hf);
    if long * s > room {
        s = room / long;
    }
    s
}

fn resize(g: &Gray, s: f32) -> (Gray, f32) {
    let nw = ((g.w as f32 * s).round() as u32).max(1);
    let nh = ((g.h as f32 * s).round() as u32).max(1);
    let Some(img) = image::GrayImage::from_raw(g.w, g.h, g.px.clone()) else {
        return (Gray { w: g.w, h: g.h, px: g.px.clone() }, 1.0);
    };
    let out = image::imageops::resize(&img, nw, nh, image::imageops::FilterType::CatmullRom);
    (Gray { w: nw, h: nh, px: out.into_raw() }, nw as f32 / g.w as f32)
}

fn pad(g: &Gray, p: u32) -> Gray {
    let (w, h) = (g.w + 2 * p, g.h + 2 * p);
    let mut px = vec![255u8; (w * h) as usize];
    for y in 0..g.h {
        let s = (y * g.w) as usize;
        let d = ((y + p) * w + p) as usize;
        px[d..d + g.w as usize].copy_from_slice(&g.px[s..s + g.w as usize]);
    }
    Gray { w, h, px }
}

/// 8-bit grey PNG (what the engines read from stdin / a file)
pub fn png(g: &Gray) -> Vec<u8> {
    let mut out = Vec::with_capacity(g.px.len() / 2);
    {
        let mut enc = png::Encoder::new(&mut out, g.w, g.h);
        enc.set_color(png::ColorType::Grayscale);
        enc.set_depth(png::BitDepth::Eight);
        enc.set_compression(png::Compression::Fast);
        if let Ok(mut w) = enc.write_header() {
            let _ = w.write_image_data(&g.px);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const O: Opts = Opts { max_pixels: 12e6, max_side: 20000, max_scale: 3.0, pad: 10 };

    fn pic(w: u32, h: u32, rgba: [u8; 4]) -> Pic {
        let mut p = Pic::new(w, h);
        for c in p.px.chunks_exact_mut(4) {
            c.copy_from_slice(&rgba);
        }
        p
    }

    #[test]
    fn transparent_is_white_and_dark_turns_light() {
        let g = gray_on_white(&pic(2, 2, [0, 0, 0, 0]));
        assert_eq!(g.px, vec![255; 4]);
        // dark theme: background 30, text 230 -> background light, text dark
        let mut p = pic(10, 10, [30, 30, 30, 255]);
        for i in 0..10 {
            p.px[i * 4..i * 4 + 4].copy_from_slice(&[230, 230, 230, 255]);
        }
        let (g, m) = prepare(&p, O);
        assert_eq!(m, Map { scale: 3.0, pad: 10.0 });
        assert_eq!((g.w, g.h), (50, 50));
        let at = |x: u32, y: u32| g.px[(y * g.w + x) as usize];
        assert_eq!(at(0, 0), 255); // the border
        assert!(at(25, 35) > 200, "background {}", at(25, 35));
        assert!(at(25, 11) < 50, "text {}", at(25, 11));
        // a light picture stays the right way round
        let (g, _) = prepare(&pic(10, 10, [240, 240, 240, 255]), O);
        assert!(g.px.iter().all(|&p| p > 200));
    }

    #[test]
    fn stretch_spreads_low_contrast() {
        let mut g = Gray { w: 100, h: 1, px: (0..100).map(|i| if i < 50 { 120 } else { 180 }).collect() };
        stretch(&mut g);
        assert_eq!((g.px[0], g.px[99]), (0, 255));
        // flat grey: left alone
        let mut f = Gray { w: 4, h: 1, px: vec![128; 4] };
        stretch(&mut f);
        assert_eq!(f.px, vec![128; 4]);
    }

    #[test]
    fn scales() {
        assert_eq!(pick_scale(400, 100, O), 3.0); // a small area: 3x
        assert_eq!(pick_scale(1920, 1080, O), 2.0); // full HD: 2x (8.3 MP)
        assert_eq!(pick_scale(3840, 2160, O), 1.0); // 4K: as it is
        // Windows OCR: longest side 2600 with the border
        let win = Opts { max_side: 2600, ..O };
        let s = pick_scale(3840, 2160, win);
        assert!((3840.0 * s + 20.0 - 2600.0).abs() < 0.5, "{s}");
        assert_eq!(pick_scale(800, 100, win), 3.0);
        assert_eq!(pick_scale(1000, 100, win), 2580.0 / 1000.0);
    }

    #[test]
    fn boxes_map_back() {
        let m = Map { scale: 2.0, pad: 16.0 };
        assert_eq!(m.back(R::new(36.0, 56.0, 40.0, 20.0)), R::new(10.0, 20.0, 20.0, 10.0));
        assert_eq!(m.back(R::new(0.0, 0.0, 8.0, 8.0)).x, 0.0);
    }

    #[test]
    fn png_is_grey() {
        let data = png(&Gray { w: 3, h: 2, px: vec![0, 128, 255, 1, 2, 3] });
        let img = image::load_from_memory(&data).unwrap();
        assert_eq!(img.color(), image::ColorType::L8);
        assert_eq!(img.into_luma8().into_raw(), vec![0, 128, 255, 1, 2, 3]);
    }
}
