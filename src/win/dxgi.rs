//! Windows fast freeze: one monitor's picture straight from the GPU.
//! 1. DXGI Desktop Duplication (what Echo records with): a D3D11 device per monitor is made once and kept, the
//!    duplication itself is made per capture and dropped right after (no cost while Lasso waits in the tray).
//! 2. Windows.Graphics.Capture of the monitor when duplication isn't possible (rotated monitor, too many apps
//!    duplicating already - Echo / OBS / Discord -, remote sessions).
//! 3. The caller falls back to GDI (win.rs) when both fail.
//! HDR monitors: asked for half-float scRGB frames and tone-mapped to SDR on the CPU (all cores): the desktop's
//! SDR content stays exact (Windows' "SDR content brightness" = white), brighter HDR parts roll off softly.
//! The code comes from Echo's recorder (src-tauri/src/gpucap.rs), trimmed to single pictures.
//! ⚠ Type-checked from Linux only; never run on a real Windows PC yet.

use crate::shapes::Pic;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};
use windows::core::Interface;
use windows::Win32::Foundation::{HMODULE, RECT};
use windows::Win32::Graphics::Direct3D::{D3D_DRIVER_TYPE_UNKNOWN, D3D_FEATURE_LEVEL_10_0, D3D_FEATURE_LEVEL_10_1, D3D_FEATURE_LEVEL_11_0, D3D_FEATURE_LEVEL_11_1};
use windows::Win32::Graphics::Direct3D11::*;
use windows::Win32::Graphics::Dxgi::Common::*;
use windows::Win32::Graphics::Dxgi::*;

fn e<E: std::fmt::Display>(ctx: &'static str) -> impl FnOnce(E) -> String {
    move |err| format!("{ctx}: {err}")
}

fn wide(s: &[u16]) -> String {
    let n = s.iter().position(|&c| c == 0).unwrap_or(s.len());
    String::from_utf16_lossy(&s[..n])
}

/// One monitor as DXGI sees it
struct Output {
    adapter: IDXGIAdapter1,
    output: IDXGIOutput1,
    /// \\.\DISPLAY1 ...
    name: String,
    rotated: bool,
    hdr: bool,
    hmon: usize,
    rect: RECT,
}

fn outputs() -> Result<Vec<Output>, String> {
    unsafe {
        let factory: IDXGIFactory1 = CreateDXGIFactory1().map_err(e("CreateDXGIFactory1"))?;
        let mut v = vec![];
        let mut a = 0;
        while let Ok(adapter) = factory.EnumAdapters1(a) {
            a += 1;
            let mut o = 0;
            while let Ok(out) = adapter.EnumOutputs(o) {
                o += 1;
                let Ok(desc) = out.GetDesc() else { continue };
                if !desc.AttachedToDesktop.as_bool() {
                    continue;
                }
                let Ok(out1) = out.cast::<IDXGIOutput1>() else { continue };
                let hdr = out.cast::<IDXGIOutput6>().ok().and_then(|o6| o6.GetDesc1().ok()).is_some_and(|d| d.ColorSpace == DXGI_COLOR_SPACE_RGB_FULL_G2084_NONE_P2020);
                let rotated = desc.Rotation != DXGI_MODE_ROTATION_IDENTITY && desc.Rotation != DXGI_MODE_ROTATION_UNSPECIFIED;
                v.push(Output { adapter: adapter.clone(), output: out1, name: wide(&desc.DeviceName), rotated, hdr, hmon: desc.Monitor.0 as usize, rect: desc.DesktopCoordinates });
            }
        }
        Ok(v)
    }
}

// ---------------------------------------------------------------------------------------------------------------
// D3D11 devices: one per monitor (so monitors can be read in parallel - an immediate context isn't thread safe),
// made once (first capture, or warm() at start) and kept.

struct Dev {
    name: String,
    luid: (u32, i32),
    device: ID3D11Device,
    ctx: ID3D11DeviceContext,
}

static DEVS: Mutex<Vec<Dev>> = Mutex::new(Vec::new());

fn luid_of(a: &IDXGIAdapter1) -> (u32, i32) {
    unsafe { a.GetDesc1() }.map(|d| (d.AdapterLuid.LowPart, d.AdapterLuid.HighPart)).unwrap_or((0, 0))
}

fn device_for(o: &Output) -> Result<(ID3D11Device, ID3D11DeviceContext), String> {
    let luid = luid_of(&o.adapter);
    if let Some(d) = DEVS.lock().unwrap().iter().find(|d| d.name == o.name && d.luid == luid) {
        return Ok((d.device.clone(), d.ctx.clone()));
    }
    let (mut device, mut ctx) = (None, None);
    unsafe {
        D3D11CreateDevice(
            &o.adapter,
            D3D_DRIVER_TYPE_UNKNOWN,
            HMODULE::default(),
            D3D11_CREATE_DEVICE_BGRA_SUPPORT,
            Some(&[D3D_FEATURE_LEVEL_11_1, D3D_FEATURE_LEVEL_11_0, D3D_FEATURE_LEVEL_10_1, D3D_FEATURE_LEVEL_10_0]),
            D3D11_SDK_VERSION,
            Some(&mut device),
            None,
            Some(&mut ctx),
        )
        .map_err(e("D3D11CreateDevice"))?;
    }
    let (device, ctx) = (device.ok_or("no device")?, ctx.ok_or("no context")?);
    DEVS.lock().unwrap().push(Dev { name: o.name.clone(), luid, device: device.clone(), ctx: ctx.clone() });
    Ok((device, ctx))
}

/// After an error the device may be gone (driver update, GPU reset): make a new one next time
fn forget_device(o: &Output) {
    DEVS.lock().unwrap().retain(|d| d.name != o.name);
}

/// At start, in the background: load the GPU driver and make the devices, so the first capture is fast too
pub fn warm() {
    std::thread::spawn(|| {
        let t0 = Instant::now();
        match outputs() {
            Ok(outs) => {
                // monitors that are gone now: their devices go too
                DEVS.lock().unwrap().retain(|d| outs.iter().any(|o| o.name == d.name));
                let n = outs.iter().filter(|o| device_for(o).is_ok()).count();
                let hdr: Vec<&str> = outs.iter().filter(|o| o.hdr).map(|o| o.name.as_str()).collect();
                crate::log::line("capture", &format!("dxgi ready: {n} of {} monitors in {} ms{}", outs.len(), t0.elapsed().as_millis(), if hdr.is_empty() { String::new() } else { format!(", HDR: {}", hdr.join(" ")) }));
            }
            Err(err) => crate::log::line("capture", &format!("dxgi: {err}")),
        }
    });
}

// ---------------------------------------------------------------------------------------------------------------
// One monitor

/// The monitor `hmon` (physical rect `rect`): DXGI, then Windows.Graphics.Capture. Err = use GDI.
pub fn grab(hmon: usize, rect: (i32, i32, i32, i32)) -> Result<(Pic, &'static str), String> {
    let outs = outputs()?;
    let o = outs
        .into_iter()
        .find(|o| o.hmon == hmon || (o.rect.left, o.rect.top, o.rect.right, o.rect.bottom) == rect)
        .ok_or("monitor not found by DXGI")?;
    let mut why = String::new();
    if !o.rotated {
        match duplicate(&o) {
            Ok(p) => return Ok((p, if o.hdr { "dxgi hdr" } else { "dxgi" })),
            Err(err) => {
                forget_device(&o);
                why = err;
            }
        }
    }
    match wgc(&o) {
        Ok(p) => {
            if !why.is_empty() {
                crate::log::line("capture", &format!("{}: duplication failed ({why}), used Windows.Graphics.Capture", o.name));
            }
            Ok((p, if o.hdr { "wgc hdr" } else { "wgc" }))
        }
        Err(err2) => {
            forget_device(&o);
            Err(if why.is_empty() { err2 } else { format!("{why}; {err2}") })
        }
    }
}

fn duplicate(o: &Output) -> Result<Pic, String> {
    let (device, ctx) = device_for(o)?;
    unsafe {
        // HDR: half-float scRGB frames, tone-mapped below (the 8-bit copy Windows would give looks washed out)
        let hdr_dup = if o.hdr { o.output.cast::<IDXGIOutput5>().ok().and_then(|o5| o5.DuplicateOutput1(&device, 0, &[DXGI_FORMAT_R16G16B16A16_FLOAT]).ok()) } else { None };
        let dup = match hdr_dup {
            Some(d) => d,
            None => o.output.DuplicateOutput(&device).map_err(e("DuplicateOutput"))?,
        };
        // The first frame of a new duplication is the whole desktop. A few drivers hand out a black one first:
        // then wait for the next (a still screen may never send one -> GDI decides).
        for attempt in 0..3 {
            let mut info = DXGI_OUTDUPL_FRAME_INFO::default();
            let mut res: Option<IDXGIResource> = None;
            match dup.AcquireNextFrame(if attempt == 0 { 250 } else { 100 }, &mut info, &mut res) {
                Ok(()) => {}
                Err(err) if err.code() == DXGI_ERROR_WAIT_TIMEOUT => return Err("no frame".into()),
                Err(err) => return Err(format!("AcquireNextFrame: {err}")),
            }
            let tex = res.and_then(|r| r.cast::<ID3D11Texture2D>().ok());
            let pic = match tex {
                Some(t) => read_texture(&device, &ctx, &t, &o.name),
                None => Err("no texture".into()),
            };
            let _ = dup.ReleaseFrame();
            let pic = pic?;
            if !all_black(&pic) {
                return Ok(pic);
            }
        }
        Err("black frames".into())
    }
}

fn wgc(o: &Output) -> Result<Pic, String> {
    use windows::Graphics::Capture::{Direct3D11CaptureFramePool, GraphicsCaptureItem, GraphicsCaptureSession};
    use windows::Graphics::DirectX::Direct3D11::IDirect3DDevice;
    use windows::Graphics::DirectX::DirectXPixelFormat;
    use windows::Win32::Graphics::Gdi::HMONITOR;
    use windows::Win32::System::WinRT::Direct3D11::{CreateDirect3D11DeviceFromDXGIDevice, IDirect3DDxgiInterfaceAccess};
    use windows::Win32::System::WinRT::Graphics::Capture::IGraphicsCaptureItemInterop;
    if !GraphicsCaptureSession::IsSupported().unwrap_or(false) {
        return Err("Windows.Graphics.Capture needs Windows 10 1903 or newer".into());
    }
    let (device, ctx) = device_for(o)?;
    unsafe {
        let interop = windows::core::factory::<GraphicsCaptureItem, IGraphicsCaptureItemInterop>().map_err(e("capture interop"))?;
        let item: GraphicsCaptureItem = interop.CreateForMonitor(HMONITOR(o.hmon as _)).map_err(e("CreateForMonitor"))?;
        let dxgi: IDXGIDevice = device.cast().map_err(e("IDXGIDevice"))?;
        let d3d: IDirect3DDevice = CreateDirect3D11DeviceFromDXGIDevice(&dxgi).map_err(e("WinRT device"))?.cast().map_err(e("IDirect3DDevice"))?;
        let size = item.Size().map_err(e("item size"))?;
        let fmt = if o.hdr { DirectXPixelFormat::R16G16B16A16Float } else { DirectXPixelFormat::B8G8R8A8UIntNormalized };
        let pool = Direct3D11CaptureFramePool::CreateFreeThreaded(&d3d, fmt, 1, size).map_err(e("frame pool"))?;
        let session = pool.CreateCaptureSession(&item).map_err(e("capture session"))?;
        // the pointer is drawn by win.rs like for the other ways; no yellow border (Windows 11, ignored before)
        let _ = session.SetIsCursorCaptureEnabled(false);
        let _ = session.SetIsBorderRequired(false);
        session.StartCapture().map_err(e("StartCapture"))?;
        let t0 = Instant::now();
        let frame = loop {
            if let Ok(f) = pool.TryGetNextFrame() {
                break Some(f);
            }
            if t0.elapsed() > Duration::from_millis(700) {
                break None;
            }
            std::thread::sleep(Duration::from_millis(2));
        };
        let res = match &frame {
            Some(f) => f
                .Surface()
                .map_err(e("frame surface"))
                .and_then(|s| s.cast::<IDirect3DDxgiInterfaceAccess>().map_err(e("dxgi access")))
                .and_then(|a| a.GetInterface::<ID3D11Texture2D>().map_err(e("texture")))
                .and_then(|t| read_texture(&device, &ctx, &t, &o.name)),
            None => Err("no frame from Windows.Graphics.Capture".into()),
        };
        if let Some(f) = frame {
            let _ = f.Close();
        }
        let _ = session.Close();
        let _ = pool.Close();
        res
    }
}

/// GPU texture -> CPU picture (RGBA, premultiplied = opaque here). BGRA8 is copied, half-float is tone-mapped.
unsafe fn read_texture(device: &ID3D11Device, ctx: &ID3D11DeviceContext, tex: &ID3D11Texture2D, name: &str) -> Result<Pic, String> {
    let mut d = D3D11_TEXTURE2D_DESC::default();
    tex.GetDesc(&mut d);
    let half = d.Format == DXGI_FORMAT_R16G16B16A16_FLOAT;
    if !half && d.Format != DXGI_FORMAT_B8G8R8A8_UNORM && d.Format != DXGI_FORMAT_B8G8R8A8_UNORM_SRGB {
        return Err(format!("unexpected frame format {}", d.Format.0));
    }
    let sd = D3D11_TEXTURE2D_DESC {
        Width: d.Width,
        Height: d.Height,
        MipLevels: 1,
        ArraySize: 1,
        Format: d.Format,
        SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
        Usage: D3D11_USAGE_STAGING,
        BindFlags: 0,
        CPUAccessFlags: D3D11_CPU_ACCESS_READ.0 as u32,
        MiscFlags: 0,
    };
    let mut staging = None;
    device.CreateTexture2D(&sd, None, Some(&mut staging)).map_err(e("staging texture"))?;
    let staging = staging.ok_or("no staging texture")?;
    ctx.CopyResource(&staging, tex);
    let mut m = D3D11_MAPPED_SUBRESOURCE::default();
    ctx.Map(&staging, 0, D3D11_MAP_READ, 0, Some(&mut m)).map_err(e("Map"))?;
    let (w, h, pitch) = (d.Width as usize, d.Height as usize, m.RowPitch as usize);
    let src = std::slice::from_raw_parts(m.pData as *const u8, pitch * h);
    let mut px = vec![0u8; w * h * 4];
    if half {
        tonemap(src, pitch, w, h, sdr_scale(name), &mut px);
    } else {
        for (y, row) in px.chunks_exact_mut(w * 4).enumerate() {
            let s = &src[y * pitch..y * pitch + w * 4];
            for (dp, sp) in row.chunks_exact_mut(4).zip(s.chunks_exact(4)) {
                dp[0] = sp[2];
                dp[1] = sp[1];
                dp[2] = sp[0];
                dp[3] = 255;
            }
        }
    }
    ctx.Unmap(&staging, 0);
    Ok(Pic { w: w as u32, h: h as u32, px })
}

fn all_black(p: &Pic) -> bool {
    // every 61st pixel is plenty: a real screen is never black in all of them unless it is black
    p.px.chunks_exact(4).step_by(61).all(|q| q[0] == 0 && q[1] == 0 && q[2] == 0)
}

// ---------------------------------------------------------------------------------------------------------------
// HDR -> SDR

/// Windows "SDR content brightness" of an HDR monitor, as a multiplier from scRGB to SDR (1.0 = 80 nits).
fn sdr_scale(device_name: &str) -> f32 {
    use windows::Win32::Devices::Display::*;
    use windows::Win32::Foundation::ERROR_SUCCESS;
    unsafe {
        let (mut np, mut nm) = (0u32, 0u32);
        if GetDisplayConfigBufferSizes(QDC_ONLY_ACTIVE_PATHS, &mut np, &mut nm) != ERROR_SUCCESS {
            return 0.4;
        }
        let mut paths = vec![DISPLAYCONFIG_PATH_INFO::default(); np as usize];
        let mut modes = vec![DISPLAYCONFIG_MODE_INFO::default(); nm as usize];
        if QueryDisplayConfig(QDC_ONLY_ACTIVE_PATHS, &mut np, paths.as_mut_ptr(), &mut nm, modes.as_mut_ptr(), None) != ERROR_SUCCESS {
            return 0.4;
        }
        for p in &paths[..np as usize] {
            let mut src = DISPLAYCONFIG_SOURCE_DEVICE_NAME::default();
            src.header.r#type = DISPLAYCONFIG_DEVICE_INFO_GET_SOURCE_NAME;
            src.header.size = std::mem::size_of::<DISPLAYCONFIG_SOURCE_DEVICE_NAME>() as u32;
            src.header.adapterId = p.sourceInfo.adapterId;
            src.header.id = p.sourceInfo.id;
            if DisplayConfigGetDeviceInfo(&mut src.header) != 0 || !wide(&src.viewGdiDeviceName).eq_ignore_ascii_case(device_name) {
                continue;
            }
            let mut wl = DISPLAYCONFIG_SDR_WHITE_LEVEL::default();
            wl.header.r#type = DISPLAYCONFIG_DEVICE_INFO_GET_SDR_WHITE_LEVEL;
            wl.header.size = std::mem::size_of::<DISPLAYCONFIG_SDR_WHITE_LEVEL>() as u32;
            wl.header.adapterId = p.targetInfo.adapterId;
            wl.header.id = p.targetInfo.id;
            if DisplayConfigGetDeviceInfo(&mut wl.header) == 0 && wl.SDRWhiteLevel > 0 {
                // SDRWhiteLevel: 1000 = 80 nits
                return (1000.0 / wl.SDRWhiteLevel as f32).clamp(0.05, 1.0);
            }
        }
        0.4
    }
}

fn half_to_f32(h: u16) -> f32 {
    let sign = if h & 0x8000 != 0 { -1.0 } else { 1.0 };
    let exp = ((h >> 10) & 0x1f) as i32;
    let man = (h & 0x3ff) as f32;
    match exp {
        0 => sign * man * (1.0 / 16_777_216.0),
        31 => {
            if man == 0.0 {
                sign * f32::INFINITY
            } else {
                f32::NAN
            }
        }
        _ => sign * (1.0 + man / 1024.0) * 2f32.powi(exp - 15),
    }
}

/// half -> f32 for every possible half (256 KB, made once)
fn half_lut() -> &'static [f32] {
    static L: OnceLock<Vec<f32>> = OnceLock::new();
    L.get_or_init(|| (0..=u16::MAX).map(half_to_f32).collect())
}

const ENC_N: usize = 16384;

/// linear 0..1 -> sRGB byte
fn srgb_lut() -> &'static [u8] {
    static L: OnceLock<Vec<u8>> = OnceLock::new();
    L.get_or_init(|| {
        (0..ENC_N)
            .map(|i| {
                let c = i as f32 / (ENC_N - 1) as f32;
                let s = if c <= 0.003_130_8 { c * 12.92 } else { 1.055 * c.powf(1.0 / 2.4) - 0.055 };
                (s * 255.0 + 0.5).clamp(0.0, 255.0) as u8
            })
            .collect()
    })
}

/// The tone curve on the brightest channel `m` (SDR white = 1.0): exact up to the knee, then extended Reinhard
/// so that `peak` lands on 1.0. When nothing is brighter than SDR white, no curve at all (desktop shots stay exact).
pub fn curve(m: f32, peak: f32) -> f32 {
    const KNEE: f32 = 0.8;
    if peak <= 1.02 {
        return m.min(1.0);
    }
    if m <= KNEE {
        return m;
    }
    let l = (m - KNEE) / (1.0 - KNEE);
    let lw = (peak - KNEE) / (1.0 - KNEE);
    let r = (l * (1.0 + l / (lw * lw)) / (1.0 + l)).min(1.0);
    KNEE + (1.0 - KNEE) * r
}

fn threads() -> usize {
    std::thread::available_parallelism().map(|n| n.get()).unwrap_or(4).clamp(1, 8)
}

/// R16G16B16A16 float (scRGB, linear, 1.0 = 80 nits) -> RGBA8 sRGB
fn tonemap(src: &[u8], pitch: usize, w: usize, h: usize, scale: f32, out: &mut [u8]) {
    if w == 0 || h == 0 {
        return;
    }
    let lut = half_lut();
    let enc = srgb_lut();
    let px = |row: &[u8], x: usize| -> [f32; 3] {
        let q = &row[x * 8..x * 8 + 6];
        let f = |i: usize| {
            let v = lut[u16::from_le_bytes([q[i], q[i + 1]]) as usize] * scale;
            if v > 0.0 {
                v
            } else {
                0.0 // negatives (out of sRGB gamut) and NaN
            }
        };
        [f(0), f(2), f(4)]
    };
    let n = threads();
    let rows_per = h.div_ceil(n).max(1);
    // pass 1: how bright the brightest parts are (99.9th percentile of the brightest channel, ignoring a few hot pixels)
    const BINS: usize = 512;
    const BIN_MAX: f32 = 16.0;
    let hist: Vec<[u32; BINS]> = std::thread::scope(|s| {
        let hs: Vec<_> = (0..n)
            .map(|t| {
                s.spawn(move || {
                    let mut hst = [0u32; BINS];
                    for y in (t * rows_per..((t + 1) * rows_per).min(h)).step_by(2) {
                        let row = &src[y * pitch..];
                        for x in (0..w).step_by(2) {
                            let c = px(row, x);
                            let m = c[0].max(c[1]).max(c[2]);
                            hst[((m / BIN_MAX * BINS as f32) as usize).min(BINS - 1)] += 1;
                        }
                    }
                    hst
                })
            })
            .collect();
        hs.into_iter().map(|j| j.join().unwrap_or([0; BINS])).collect()
    });
    let mut all = [0u64; BINS];
    for hh in &hist {
        for (a, b) in all.iter_mut().zip(hh.iter()) {
            *a += *b as u64;
        }
    }
    let total: u64 = all.iter().sum();
    let mut acc = 0u64;
    let mut peak = BIN_MAX;
    for (i, c) in all.iter().enumerate() {
        acc += c;
        if acc as f64 >= total as f64 * 0.999 {
            peak = (i + 1) as f32 * BIN_MAX / BINS as f32;
            break;
        }
    }
    // pass 2
    std::thread::scope(|s| {
        for (t, chunk) in out.chunks_mut(rows_per * w * 4).enumerate() {
            s.spawn(move || {
                for (r, orow) in chunk.chunks_exact_mut(w * 4).enumerate() {
                    let y = t * rows_per + r;
                    let row = &src[y * pitch..];
                    for (x, o) in orow.chunks_exact_mut(4).enumerate() {
                        let mut c = px(row, x);
                        let m = c[0].max(c[1]).max(c[2]);
                        if m > 0.0 {
                            let k = curve(m, peak) / m;
                            if k < 1.0 {
                                c = [c[0] * k, c[1] * k, c[2] * k];
                            }
                        }
                        for i in 0..3 {
                            o[i] = enc[((c[i].min(1.0) * (ENC_N - 1) as f32) + 0.5) as usize];
                        }
                        o[3] = 255;
                    }
                }
            });
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn halves() {
        assert_eq!(half_to_f32(0x3c00), 1.0);
        assert_eq!(half_to_f32(0x4000), 2.0);
        assert_eq!(half_to_f32(0xc000), -2.0);
        assert_eq!(half_to_f32(0x0000), 0.0);
        assert!((half_to_f32(0x3555) - 0.333).abs() < 0.001);
        assert!((half_to_f32(0x0001) - 5.96e-8).abs() < 1e-9);
    }

    #[test]
    fn sdr_desktop_stays_exact() {
        // nothing brighter than SDR white: white is 255, mid grey untouched
        assert_eq!(curve(1.0, 1.0), 1.0);
        assert_eq!(curve(0.5, 1.0), 0.5);
        // HDR highlights present: below the knee untouched, the peak reaches white, smooth and rising
        assert_eq!(curve(0.5, 6.0), 0.5);
        assert!((curve(6.0, 6.0) - 1.0).abs() < 1e-4);
        let mut last = 0.0;
        for i in 0..200 {
            let v = curve(i as f32 * 0.05, 6.0);
            assert!(v >= last && v <= 1.0);
            last = v;
        }
    }

    #[test]
    fn tonemap_white_and_black() {
        // 2x1: SDR white at scale 1 (scRGB 1.0) and black
        let one = 0x3c00u16.to_le_bytes();
        let mut src = vec![];
        for _ in 0..3 {
            src.extend_from_slice(&one);
        }
        src.extend_from_slice(&one);
        src.extend_from_slice(&[0u8; 8]);
        let mut out = vec![0u8; 8];
        tonemap(&src, 16, 2, 1, 1.0, &mut out);
        assert_eq!(out, vec![255, 255, 255, 255, 0, 0, 0, 255]);
    }
}
