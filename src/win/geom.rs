//! Windows monitor geometry, pure math (no Win32 calls, so the tests also run on Linux: main.rs includes this
//! file in the Linux test build).
//!
//! Windows has no shared logical desktop: each monitor has its own scale (DPI / 96) and the monitor rects are in
//! physical px. Dividing each monitor's origin by its own scale makes the "logical" areas overlap when the scales
//! differ (100 % at 0 and 200 % at 1920 -> the second starts at 960, inside the first). So Lasso's logical layout on
//! Windows is: origin = the PHYSICAL origin, size = physical size / scale. The areas never overlap (size <= physical
//! size), and on one monitor (logical - origin) * scale = physical - origin, exactly.

/// One monitor: physical rect (left, top, right, bottom) and its scale
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Geo {
    pub l: i32,
    pub t: i32,
    pub r: i32,
    pub b: i32,
    pub s: f32,
}

impl Geo {
    /// The logical area (x, y, w, h) app.rs gets as OutInfo
    pub fn logical(&self) -> (i32, i32, i32, i32) {
        let s = if self.s > 0.0 { self.s } else { 1.0 };
        (self.l, self.t, ((self.r - self.l) as f32 / s) as i32, ((self.b - self.t) as f32 / s) as i32)
    }
}

/// A window's physical rect (x, y, w, h) -> its pieces in the logical layout, one per monitor it is on (clipped to
/// that monitor, converted with that monitor's scale). Top-first order of the list is kept by the caller.
pub fn window_pieces(r: (f32, f32, f32, f32), mons: &[Geo]) -> Vec<(f32, f32, f32, f32)> {
    let (x0, y0, x1, y1) = (r.0, r.1, r.0 + r.2, r.1 + r.3);
    mons.iter()
        .filter_map(|m| {
            let (l, t, rr, b) = (x0.max(m.l as f32), y0.max(m.t as f32), x1.min(m.r as f32), y1.min(m.b as f32));
            if rr <= l || b <= t {
                return None;
            }
            let s = if m.s > 0.0 { m.s } else { 1.0 };
            let (ml, mt) = (m.l as f32, m.t as f32);
            Some((ml + (l - ml) / s, mt + (t - mt) / s, (rr - l) / s, (b - t) / s))
        })
        .collect()
}

/// A pin's place: `pos` = logical px from the top-left of monitor `m` -> physical screen px
pub fn pin_origin(m: &Geo, pos: (i32, i32)) -> (i32, i32) {
    let s = if m.s > 0.0 { m.s } else { 1.0 };
    (m.l + (pos.0 as f32 * s).round() as i32, m.t + (pos.1 as f32 * s).round() as i32)
}

/// Order of the monitor list (= the output keys): the primary first (corner popups go on the first one), the rest
/// as Windows listed them
pub fn primary_first<T>(v: &mut [T], is_primary: impl Fn(&T) -> bool) {
    if let Some(i) = v.iter().position(is_primary) {
        v[..=i].rotate_right(1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // 1920x1080 at 100 % on the left, 3840x2160 at 200 % on the right (the review's case)
    fn mixed() -> Vec<Geo> {
        vec![Geo { l: 0, t: 0, r: 1920, b: 1080, s: 1.0 }, Geo { l: 1920, t: 0, r: 5760, b: 2160, s: 2.0 }]
    }

    fn overlap(a: (i32, i32, i32, i32), b: (i32, i32, i32, i32)) -> bool {
        a.0 < b.0 + b.2 && b.0 < a.0 + a.2 && a.1 < b.1 + b.3 && b.1 < a.1 + a.3
    }

    #[test]
    fn logical_areas_never_overlap() {
        for ms in [mixed(), vec![Geo { l: 0, t: 0, r: 3840, b: 2160, s: 2.0 }, Geo { l: 3840, t: 0, r: 5760, b: 1080, s: 1.0 }], vec![Geo { l: -2560, t: 0, r: 0, b: 1440, s: 1.25 }, Geo { l: 0, t: 0, r: 1920, b: 1080, s: 1.0 }]] {
            let a = ms[0].logical();
            let b = ms[1].logical();
            assert!(!overlap(a, b), "{a:?} {b:?}");
        }
        assert_eq!(mixed()[1].logical(), (1920, 0, 1920, 1080));
        assert_eq!(Geo { l: -2560, t: 0, r: 0, b: 1440, s: 1.25 }.logical(), (-2560, 0, 2048, 1152));
    }

    /// what app.rs set_windows does with a logical rect on one monitor: (x - out.x) * scale
    fn on_monitor(p: (f32, f32, f32, f32), m: &Geo) -> (f32, f32, f32, f32) {
        let (x, y, _, _) = m.logical();
        let s = m.s;
        ((p.0 - x as f32) * s, (p.1 - y as f32) * s, p.2 * s, p.3 * s)
    }

    #[test]
    fn window_on_the_200_percent_monitor() {
        let ms = mixed();
        // physical 2120,100 400x300 = 200,100 on the right monitor
        let p = window_pieces((2120.0, 100.0, 400.0, 300.0), &ms);
        assert_eq!(p.len(), 1);
        assert_eq!(on_monitor(p[0], &ms[1]), (200.0, 100.0, 400.0, 300.0));
        // and it is outside the left monitor's logical area (the old way put it at 1060 there)
        let (lx, _, lw, _) = ms[0].logical();
        assert!(p[0].0 >= (lx + lw) as f32);
    }

    #[test]
    fn window_on_the_100_percent_monitor_stays_there() {
        let ms = mixed();
        let p = window_pieces((1000.0, 50.0, 500.0, 400.0), &ms);
        assert_eq!(p, vec![(1000.0, 50.0, 500.0, 400.0)]);
        let (rx, _, _, _) = ms[1].logical();
        assert!(p[0].0 + p[0].2 <= rx as f32);
    }

    #[test]
    fn window_across_two_monitors_is_split() {
        let ms = mixed();
        // physical 1720..2320: 200 px on the left one, 400 px on the right one
        let p = window_pieces((1720.0, 0.0, 600.0, 200.0), &ms);
        assert_eq!(p.len(), 2);
        assert_eq!(on_monitor(p[0], &ms[0]), (1720.0, 0.0, 200.0, 200.0));
        assert_eq!(on_monitor(p[1], &ms[1]), (0.0, 0.0, 400.0, 200.0));
    }

    #[test]
    fn window_off_every_monitor_is_dropped() {
        assert!(window_pieces((-500.0, -500.0, 100.0, 100.0), &mixed()).is_empty());
    }

    #[test]
    fn monitor_left_of_the_primary_at_125_percent() {
        let ms = vec![Geo { l: -2560, t: 0, r: 0, b: 1440, s: 1.25 }, Geo { l: 0, t: 0, r: 1920, b: 1080, s: 1.0 }];
        let p = window_pieces((-2000.0, 250.0, 1000.0, 500.0), &ms);
        assert_eq!(on_monitor(p[0], &ms[0]), (560.0, 250.0, 1000.0, 500.0));
    }

    #[test]
    fn pins_go_on_their_own_monitor() {
        let ms = mixed();
        // a box at physical 400,200 of the right monitor = logical 200,100 there
        assert_eq!(pin_origin(&ms[1], (200, 100)), (2320, 200));
        assert_eq!(pin_origin(&ms[0], (80, 80)), (80, 80));
        assert_eq!(pin_origin(&Geo { l: -2560, t: 0, r: 0, b: 1440, s: 1.25 }, (100, 40)), (-2435, 50));
    }

    #[test]
    fn primary_goes_first() {
        let mut v = vec![(1, false), (2, false), (3, true), (4, false)];
        primary_first(&mut v, |m| m.1);
        assert_eq!(v.iter().map(|m| m.0).collect::<Vec<_>>(), vec![3, 1, 2, 4]);
        let mut w = vec![(1, true), (2, false)];
        primary_first(&mut w, |m| m.1);
        assert_eq!(w, vec![(1, true), (2, false)]);
    }
}
