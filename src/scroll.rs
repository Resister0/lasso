//! Scrolling capture (long web pages, chats, documents): the box is grabbed again and again while the user scrolls
//! the page under it; each new frame is matched against the last one (how far did the content move up?) and only the
//! new rows are added below. The result opens on the frozen screen with every tool.
//!
//! Matching: one fingerprint per row (a hash of the row's pixels), only rows with something on them count (blank rows
//! match everywhere). The shift with the most matching rows wins - it must cover at least half of them, else the
//! frame is skipped (the page jumped, an animation, ...). ⚠ sticky headers / footers inside the box are added again.

use crate::shapes::Pic;

/// The whole stitched picture so far + the last frame
pub struct Stitcher {
    pub pic: Pic,
    last: Pic,
    /// stop growing here (a runaway page): 30 000 rows
    pub max_h: u32,
}

fn row_hashes(p: &Pic) -> Vec<(u64, bool)> {
    let w = p.w as usize;
    (0..p.h as usize)
        .map(|y| {
            let row = &p.px[y * w * 4..(y + 1) * w * 4];
            let mut h: u64 = 0xcbf29ce484222325;
            let first = [row[0], row[1], row[2]];
            let mut varied = false;
            for px in row.chunks_exact(4) {
                for c in &px[..3] {
                    h ^= *c as u64;
                    h = h.wrapping_mul(0x100000001b3);
                }
                if !varied && (px[0] != first[0] || px[1] != first[1] || px[2] != first[2]) {
                    varied = true;
                }
            }
            (h, varied)
        })
        .collect()
}

/// How many rows the content moved up from `last` to `next` (0 = didn't move, None = no good match)
pub fn shift(last: &Pic, next: &Pic) -> Option<u32> {
    if last.w != next.w || last.h != next.h || last.h < 8 {
        return None;
    }
    let a = row_hashes(last);
    let b = row_hashes(next);
    let h = a.len();
    let informative = b.iter().filter(|r| r.1).count();
    if informative < 4 {
        return None;
    }
    // unchanged?
    let same = a.iter().zip(b.iter()).filter(|(x, y)| x.0 == y.0).count();
    if same * 100 >= h * 99 {
        return Some(0);
    }
    let mut best: Option<(usize, usize, usize)> = None; // (d, matches, informative rows looked at)
    for d in 1..h - 4 {
        let mut m = 0;
        let mut n = 0;
        for r in 0..h - d {
            if b[r].1 {
                n += 1;
                if b[r].0 == a[r + d].0 {
                    m += 1;
                }
            }
        }
        if n >= 4 && m * 2 >= n && best.map(|(_, bm, bn)| m * bn > bm * n || (m * bn == bm * n && m > bm)).unwrap_or(true) {
            best = Some((d, m, n));
        }
    }
    best.map(|(d, _, _)| d as u32)
}

impl Stitcher {
    pub fn new(first: Pic) -> Stitcher {
        let pic = Pic { w: first.w, h: first.h, px: first.px.clone() };
        Stitcher { pic, last: first, max_h: 30_000 }
    }

    /// Adds a new frame; returns how many rows it added
    pub fn add(&mut self, next: Pic) -> u32 {
        let Some(d) = shift(&self.last, &next) else { return 0 };
        if d == 0 || self.pic.h >= self.max_h {
            self.last = next;
            return 0;
        }
        let d = d.min(self.max_h - self.pic.h);
        let w = next.w as usize;
        let from = (next.h - d) as usize;
        self.pic.px.extend_from_slice(&next.px[from * w * 4..next.h as usize * w * 4]);
        self.pic.h += d;
        self.last = next;
        d
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A tall "page" with text-like lines; frame = rows [top, top + h)
    fn page(h: u32) -> Pic {
        let w = 120u32;
        let mut p = Pic::new(w, h);
        for y in 0..h {
            for x in 0..w {
                let i = ((y * w + x) * 4) as usize;
                let ink = (y % 17 < 9) && ((x * 7 + y * 13) % 11 < 5);
                let v = if ink { 30 } else { 250 };
                p.px[i..i + 4].copy_from_slice(&[v, v, (y % 251) as u8, 255]);
            }
        }
        p
    }

    fn frame(p: &Pic, top: u32, h: u32) -> Pic {
        p.crop(crate::shapes::IR { x: 0, y: top as i32, w: p.w as i32, h: h as i32 })
    }

    #[test]
    fn finds_the_shift_and_stitches() {
        let p = page(900);
        let mut s = Stitcher::new(frame(&p, 0, 200));
        assert_eq!(s.add(frame(&p, 0, 200)), 0); // nothing moved
        assert_eq!(s.add(frame(&p, 37, 200)), 37);
        assert_eq!(s.add(frame(&p, 120, 200)), 83);
        assert_eq!(s.add(frame(&p, 300, 200)), 180);
        assert_eq!(s.pic.h, 500);
        // the stitched picture is exactly the page's first 500 rows
        assert_eq!(s.pic.px, frame(&p, 0, 500).px);
    }

    #[test]
    fn blank_frames_and_jumps_are_skipped() {
        let p = page(900);
        let mut s = Stitcher::new(frame(&p, 0, 200));
        let blank = Pic { w: 120, h: 200, px: vec![255; 120 * 200 * 4] };
        assert_eq!(s.add(blank), 0);
        let mut s = Stitcher::new(frame(&p, 0, 200));
        // a jump far past the frame: no overlap, nothing added
        assert_eq!(s.add(frame(&p, 600, 200)), 0);
    }
}
