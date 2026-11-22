//! The sign-in token at rest, like Echo keeps its Echo Cloud token:
//! - Windows: encrypted for the signed-in Windows user (DPAPI, crypt32.dll loaded at run time) -> "enc:<hex>"
//! - Linux: in the desktop keyring (keyring_linux.rs) -> the file only says "ks:"
//! - no keyring / test copies (`LASSO_HOME`) / unit tests: as it is, in a file only this user may read.
//!
//! Test copies never touch the Secret Service (master notes: every call can pop a KDE Wallet dialog on the real
//! screen) - with `LASSO_HOME` set the token stays in the test home.

pub const KS: &str = "ks:";
pub const ENC: &str = "enc:";
/// the keyring item's name
#[cfg(target_os = "linux")]
const NAME: &str = "share-token";

/// test copies and unit tests keep the token in their own (scratch) file
fn plain_only() -> bool {
    cfg!(test) || std::env::var_os("LASSO_HOME").is_some()
}

/// The token -> what goes into account.json
pub fn protect(v: &str) -> String {
    if v.is_empty() || plain_only() {
        return v.to_string();
    }
    #[cfg(target_os = "linux")]
    {
        if super::keyring_linux::available() && super::keyring_linux::store(NAME, v).is_ok() {
            return KS.into();
        }
    }
    #[cfg(windows)]
    {
        if let Some(b) = dpapi::protect(v.as_bytes()) {
            return format!("{ENC}{}", b.iter().map(|x| format!("{x:02x}")).collect::<String>());
        }
    }
    v.to_string()
}

/// account.json's value -> the token ("" = can't be read: keyring locked / file from another PC or user)
pub fn unprotect(s: &str) -> String {
    if s == KS {
        #[cfg(target_os = "linux")]
        {
            if plain_only() {
                return String::new();
            }
            return super::keyring_linux::load(NAME).unwrap_or_default();
        }
        #[cfg(not(target_os = "linux"))]
        return String::new();
    }
    if let Some(h) = s.strip_prefix(ENC) {
        let bytes: Option<Vec<u8>> = (0..h.len() / 2).map(|i| h.get(i * 2..i * 2 + 2).and_then(|x| u8::from_str_radix(x, 16).ok())).collect();
        #[cfg(windows)]
        return bytes.filter(|_| h.len() % 2 == 0).and_then(|b| dpapi::unprotect(&b)).and_then(|b| String::from_utf8(b).ok()).unwrap_or_default();
        #[cfg(not(windows))]
        {
            let _ = bytes;
            return String::new();
        }
    }
    s.to_string()
}

/// Signed out: the keyring item goes too
pub fn forget(s: &str) {
    #[cfg(target_os = "linux")]
    if s == KS && !plain_only() {
        super::keyring_linux::delete(NAME);
    }
    let _ = s;
}

/// Only this user may read the file (Linux; %APPDATA% is per user on Windows)
pub fn private_file(p: &std::path::Path) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(p, std::fs::Permissions::from_mode(0o600));
    }
    #[cfg(not(unix))]
    let _ = p;
}

/// 32 random bytes from the system as hex (the Lasso device key) - /dev/urandom / RtlGenRandom. None = no randomness.
pub fn random_hex32() -> Option<String> {
    let mut b = [0u8; 32];
    #[cfg(unix)]
    {
        use std::io::Read;
        std::fs::File::open("/dev/urandom").ok()?.read_exact(&mut b).ok()?;
    }
    #[cfg(windows)]
    {
        if !dpapi::random(&mut b) {
            return None;
        }
    }
    Some(b.iter().map(|x| format!("{x:02x}")).collect())
}

/// DPAPI through crypt32.dll loaded at run time (no extra windows-sys features). ⚠ untested (Windows)
#[cfg(windows)]
mod dpapi {
    use windows_sys::Win32::System::LibraryLoader::{GetProcAddress, LoadLibraryA};

    #[repr(C)]
    struct Blob {
        cb: u32,
        pb: *mut u8,
    }
    type CryptFn = unsafe extern "system" fn(*const Blob, *const u16, *const Blob, *const core::ffi::c_void, *const core::ffi::c_void, u32, *mut Blob) -> i32;
    type UnprotectFn = unsafe extern "system" fn(*const Blob, *mut *mut u16, *const Blob, *const core::ffi::c_void, *const core::ffi::c_void, u32, *mut Blob) -> i32;
    type FreeFn = unsafe extern "system" fn(*mut core::ffi::c_void) -> *mut core::ffi::c_void;
    const UI_FORBIDDEN: u32 = 0x1;

    unsafe fn sym(dll: &[u8], name: &[u8]) -> Option<unsafe extern "system" fn() -> isize> {
        let m = LoadLibraryA(dll.as_ptr());
        if m.is_null() {
            return None;
        }
        GetProcAddress(m, name.as_ptr())
    }

    unsafe fn take(out: Blob) -> Vec<u8> {
        let v = std::slice::from_raw_parts(out.pb, out.cb as usize).to_vec();
        if let Some(f) = sym(b"kernel32.dll\0", b"LocalFree\0") {
            let free: FreeFn = std::mem::transmute(f);
            free(out.pb as *mut core::ffi::c_void);
        }
        v
    }

    /// RtlGenRandom (advapi32 SystemFunction036), the system's random bytes. ⚠ untested (Windows)
    pub fn random(buf: &mut [u8]) -> bool {
        type GenFn = unsafe extern "system" fn(*mut u8, u32) -> u8;
        unsafe {
            let Some(f) = sym(b"advapi32.dll\0", b"SystemFunction036\0") else { return false };
            let f: GenFn = std::mem::transmute(f);
            f(buf.as_mut_ptr(), buf.len() as u32) != 0
        }
    }

    pub fn protect(data: &[u8]) -> Option<Vec<u8>> {
        unsafe {
            let f: CryptFn = std::mem::transmute(sym(b"crypt32.dll\0", b"CryptProtectData\0")?);
            let inp = Blob { cb: data.len() as u32, pb: data.as_ptr() as *mut u8 };
            let mut out = Blob { cb: 0, pb: std::ptr::null_mut() };
            let desc: Vec<u16> = "Lasso".encode_utf16().chain(std::iter::once(0)).collect();
            if f(&inp, desc.as_ptr(), std::ptr::null(), std::ptr::null(), std::ptr::null(), UI_FORBIDDEN, &mut out) == 0 {
                return None;
            }
            Some(take(out))
        }
    }

    pub fn unprotect(data: &[u8]) -> Option<Vec<u8>> {
        unsafe {
            let f: UnprotectFn = std::mem::transmute(sym(b"crypt32.dll\0", b"CryptUnprotectData\0")?);
            let inp = Blob { cb: data.len() as u32, pb: data.as_ptr() as *mut u8 };
            let mut out = Blob { cb: 0, pb: std::ptr::null_mut() };
            if f(&inp, std::ptr::null_mut(), std::ptr::null(), std::ptr::null(), std::ptr::null(), UI_FORBIDDEN, &mut out) == 0 {
                return None;
            }
            Some(take(out))
        }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn tests_never_reach_the_keyring() {
        // under cfg(test) the token stays as it is - no Secret Service / DPAPI call
        assert_eq!(super::protect("tok_abc"), "tok_abc");
        assert_eq!(super::unprotect("tok_abc"), "tok_abc");
        assert_eq!(super::unprotect(super::KS), "");
        assert_eq!(super::unprotect("enc:zz"), "");
        super::forget(super::KS);
    }
}
