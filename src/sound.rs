//! Optional sounds (Settings > General, off by default): a soft click when the screen freezes, a short chime when a
//! picture is copied / saved. Made in code (no sound files), played by the desktop's own player in the background.

#[derive(Clone, Copy)]
pub enum Snd {
    Freeze,
    Done,
}

pub fn play(s: Snd) {
    if !crate::config::Config::load().on("sounds") {
        return;
    }
    #[cfg(target_os = "linux")]
    {
        let (name, notes): (&str, &[(f32, f32)]) = match s {
            Snd::Freeze => ("lasso-freeze.wav", &[(1400.0, 0.025)]),
            Snd::Done => ("lasso-done.wav", &[(880.0, 0.06), (1320.0, 0.09)]),
        };
        let path = crate::paths::runtime_dir().join(name);
        if !path.exists() {
            let _ = std::fs::write(&path, wav(notes));
        }
        std::thread::spawn(move || {
            for player in ["pw-play", "paplay", "aplay"] {
                if std::process::Command::new(player).arg(&path).stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null()).status().is_ok() {
                    break;
                }
            }
        });
    }
    #[cfg(windows)]
    unsafe {
        use windows_sys::Win32::System::Diagnostics::Debug::MessageBeep;
        use windows_sys::Win32::UI::WindowsAndMessaging::{MB_ICONASTERISK, MB_OK};
        MessageBeep(match s {
            Snd::Freeze => MB_OK,
            Snd::Done => MB_ICONASTERISK,
        });
    }
}

/// 16-bit mono 44.1 kHz WAV of soft sine notes (fade in / out, quiet)
#[cfg_attr(windows, allow(dead_code))]
fn wav(notes: &[(f32, f32)]) -> Vec<u8> {
    let rate = 44100u32;
    let mut pcm: Vec<i16> = vec![];
    for (f, secs) in notes {
        let n = (rate as f32 * secs) as usize;
        for i in 0..n {
            let t = i as f32 / rate as f32;
            let env = (i.min(n - i) as f32 / (rate as f32 * 0.006)).min(1.0);
            pcm.push(((2.0 * std::f32::consts::PI * f * t).sin() * env * 0.18 * i16::MAX as f32) as i16);
        }
    }
    let data_len = (pcm.len() * 2) as u32;
    let mut out = Vec::with_capacity(44 + data_len as usize);
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&(36 + data_len).to_le_bytes());
    out.extend_from_slice(b"WAVEfmt ");
    out.extend_from_slice(&16u32.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&rate.to_le_bytes());
    out.extend_from_slice(&(rate * 2).to_le_bytes());
    out.extend_from_slice(&2u16.to_le_bytes());
    out.extend_from_slice(&16u16.to_le_bytes());
    out.extend_from_slice(b"data");
    out.extend_from_slice(&data_len.to_le_bytes());
    for s in pcm {
        out.extend_from_slice(&s.to_le_bytes());
    }
    out
}

#[cfg(test)]
mod tests {
    #[test]
    fn wav_header() {
        let w = super::wav(&[(1000.0, 0.01)]);
        assert_eq!(&w[0..4], b"RIFF");
        assert_eq!(w.len(), 44 + 441 * 2);
    }
}
