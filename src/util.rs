//! Small helpers.

#[cfg_attr(windows, allow(dead_code))]
pub fn url_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len() {
            if let Ok(v) = u8::from_str_radix(&s[i + 1..i + 3], 16) {
                out.push(v);
                i += 3;
                continue;
            }
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// A path for a file:// URI: everything but unreserved characters and "/" percent-encoded
#[cfg_attr(windows, allow(dead_code))]
pub fn url_encode_path(p: &str) -> String {
    let mut out = String::new();
    for b in p.bytes() {
        if b.is_ascii_alphanumeric() || b"/-._~".contains(&b) {
            out.push(b as char);
        } else {
            out += &format!("%{b:02X}");
        }
    }
    out
}

/// Home folder -> "~" in log lines (no user names in logs)
pub fn scrub_home(s: &str) -> String {
    match std::env::var("HOME").or_else(|_| std::env::var("USERPROFILE")) {
        Ok(h) if h.len() > 3 => s.replace(&h, "~"),
        _ => s.to_string(),
    }
}

/// Local time parts (y, mo, d, h, mi, s) - Linux: localtime_r; Windows: GetLocalTime
pub fn local_now() -> (i32, u32, u32, u32, u32, u32) {
    local_from_unix(std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0))
}

#[cfg(unix)]
pub fn local_from_unix(t: i64) -> (i32, u32, u32, u32, u32, u32) {
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    let tt = t as libc::time_t;
    unsafe { libc::localtime_r(&tt, &mut tm) };
    (tm.tm_year + 1900, tm.tm_mon as u32 + 1, tm.tm_mday as u32, tm.tm_hour as u32, tm.tm_min as u32, tm.tm_sec as u32)
}

#[cfg(windows)]
pub fn local_from_unix(t: i64) -> (i32, u32, u32, u32, u32, u32) {
    // UTC fallback with the current local offset from Windows
    use windows_sys::Win32::System::Time::{GetTimeZoneInformation, TIME_ZONE_INFORMATION};
    let mut tz: TIME_ZONE_INFORMATION = unsafe { std::mem::zeroed() };
    let r = unsafe { GetTimeZoneInformation(&mut tz) };
    let bias = tz.Bias + if r == 2 { tz.DaylightBias } else { 0 };
    civil(t - bias as i64 * 60)
}

/// UTC seconds -> civil date/time
#[cfg_attr(not(windows), allow(dead_code))]
pub fn civil(t: i64) -> (i32, u32, u32, u32, u32, u32) {
    let days = t.div_euclid(86400);
    let secs = t.rem_euclid(86400);
    let z = days + 719468;
    let era = z.div_euclid(146097);
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let y = if m <= 2 { y + 1 } else { y } as i32;
    (y, m, d, (secs / 3600) as u32, ((secs / 60) % 60) as u32, (secs % 60) as u32)
}

pub fn unix_now() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0)
}

pub const DEFAULT_PATTERN: &str = "Lasso {yyyy}-{MM}-{dd} {HH}.{mm}.{ss}";

/// File name from the pattern: {yyyy} {MM} {dd} {HH} {mm} {ss} {app}
pub fn name_from_pattern(pat: &str, app: &str) -> String {
    let (y, mo, d, h, mi, s) = local_now();
    let n = pat
        .replace("{yyyy}", &format!("{y:04}"))
        .replace("{MM}", &format!("{mo:02}"))
        .replace("{dd}", &format!("{d:02}"))
        .replace("{HH}", &format!("{h:02}"))
        .replace("{mm}", &format!("{mi:02}"))
        .replace("{ss}", &format!("{s:02}"))
        .replace("{app}", app);
    let n = n.chars().map(|c| if "/\\:*?\"<>|".contains(c) { '-' } else { c }).collect::<String>().trim().trim_start_matches('.').trim().to_string();
    if n.is_empty() && pat != DEFAULT_PATTERN {
        // an empty pattern (or only dots / spaces) would save hidden files like ".png"
        return name_from_pattern(DEFAULT_PATTERN, app);
    }
    n
}

#[cfg(test)]
mod tests {
    #[test]
    fn civil_dates() {
        assert_eq!(super::civil(0), (1970, 1, 1, 0, 0, 0));
        assert_eq!(super::civil(1_700_000_000), (2023, 11, 14, 22, 13, 20));
    }
    #[test]
    fn names_and_uris() {
        assert!(super::name_from_pattern("", "X").starts_with("Lasso 20"));
        assert!(super::name_from_pattern("  ..", "X").starts_with("Lasso 20"));
        assert_eq!(super::name_from_pattern("{app} shot", "Dolphin"), "Dolphin shot");
        assert_eq!(super::url_encode_path("/a b/50%#?.png"), "/a%20b/50%25%23%3F.png");
    }

    #[test]
    fn decode() {
        assert_eq!(super::url_decode("/a%20b"), "/a b");
    }
}
