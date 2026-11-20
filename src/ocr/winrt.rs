//! Windows: the OCR engine built into Windows 10 / 11 (Windows.Media.Ocr, offline, no install) through Windows
//! PowerShell 5 (always there), so Lasso needs no WinRT bindings. ⚠ untested on a real Windows (type-checks only).
//! The picture goes in as a grey PNG in the temp folder; one "W line x y w h text" row per word comes back (UTF-8).
#![cfg_attr(not(windows), allow(dead_code))]

use super::prep;
use super::Word;
use crate::shapes::R;

/// OcrEngine.MaxImageDimension: Windows refuses pictures with a longer side
pub const OPTS: prep::Opts = prep::Opts { max_pixels: 12e6, max_side: 2600, max_scale: 3.0, pad: 16 };

const NO_LANG: &str = "Copy text needs a Windows language with text recognition: Settings > Time & language > Language & region";

const SCRIPT: &str = r#"param([string]$img)
$ErrorActionPreference = 'Stop'
[Console]::OutputEncoding = New-Object System.Text.UTF8Encoding $false
try {
  Add-Type -AssemblyName System.Runtime.WindowsRuntime
  $null = [Windows.Storage.StorageFile,Windows.Storage,ContentType=WindowsRuntime]
  $null = [Windows.Media.Ocr.OcrEngine,Windows.Foundation,ContentType=WindowsRuntime]
  $null = [Windows.Graphics.Imaging.BitmapDecoder,Windows.Graphics,ContentType=WindowsRuntime]
  $asTask = ([System.WindowsRuntimeSystemExtensions].GetMethods() | Where-Object { $_.Name -eq 'AsTask' -and $_.GetParameters().Count -eq 1 -and $_.GetParameters()[0].ParameterType.Name -eq 'IAsyncOperation`1' })[0]
  function Await($op, $t) { $task = $asTask.MakeGenericMethod($t).Invoke($null, @($op)); $null = $task.Wait(-1); $task.Result }
  $e = [Windows.Media.Ocr.OcrEngine]::TryCreateFromUserProfileLanguages()
  if ($null -eq $e) {
    foreach ($l in [Windows.Media.Ocr.OcrEngine]::AvailableRecognizerLanguages) {
      $e = [Windows.Media.Ocr.OcrEngine]::TryCreateFromLanguage($l)
      if ($null -ne $e) { break }
    }
  }
  if ($null -eq $e) { 'NOLANG'; exit 3 }
  $f = Await ([Windows.Storage.StorageFile]::GetFileFromPathAsync($img)) ([Windows.Storage.StorageFile])
  $s = Await ($f.OpenAsync([Windows.Storage.FileAccessMode]::Read)) ([Windows.Storage.Streams.IRandomAccessStream])
  $d = Await ([Windows.Graphics.Imaging.BitmapDecoder]::CreateAsync($s)) ([Windows.Graphics.Imaging.BitmapDecoder])
  $b = Await ($d.GetSoftwareBitmapAsync()) ([Windows.Graphics.Imaging.SoftwareBitmap])
  $b = [Windows.Graphics.Imaging.SoftwareBitmap]::Convert($b, [Windows.Graphics.Imaging.BitmapPixelFormat]::Bgra8)
  $r = Await ($e.RecognizeAsync($b)) ([Windows.Media.Ocr.OcrResult])
  $i = 0
  foreach ($l in $r.Lines) {
    foreach ($w in $l.Words) { $q = $w.BoundingRect; "W`t$i`t$($q.X)`t$($q.Y)`t$($q.Width)`t$($q.Height)`t$($w.Text)" }
    $i++
  }
} catch {
  "E`t$($_.Exception.Message)"
  exit 1
}
"#;

#[cfg(windows)]
pub fn recognize(img: &prep::Gray) -> Result<Vec<Word>, String> {
    use super::run::{self, Fail};
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000; // Lasso has no console: without it a black window flashes up
    let dir = crate::paths::runtime_dir();
    let id = std::process::id();
    let (png, ps1) = (dir.join(format!("lasso-ocr-{id}.png")), dir.join(format!("lasso-ocr-{id}.ps1")));
    let res = (|| {
        std::fs::write(&png, prep::png(img)).map_err(|e| e.to_string())?;
        std::fs::write(&ps1, SCRIPT).map_err(|e| e.to_string())?;
        let root = std::env::var_os("SystemRoot").map(std::path::PathBuf::from).unwrap_or_else(|| "C:\\Windows".into());
        let exe = root.join("System32\\WindowsPowerShell\\v1.0\\powershell.exe");
        let mut cmd = std::process::Command::new(if exe.is_file() { exe } else { "powershell.exe".into() });
        cmd.args(["-NoProfile", "-NonInteractive", "-ExecutionPolicy", "Bypass", "-File"]).arg(&ps1).arg(&png);
        cmd.creation_flags(CREATE_NO_WINDOW);
        match run::run(cmd, None, std::time::Duration::from_secs(40)) {
            Err(Fail::NotFound) => Err("Copy text needs Windows PowerShell (part of Windows)".to_string()),
            Err(Fail::TimedOut) => Err(super::TOO_LONG.to_string()),
            Err(Fail::Io(e)) => Err(format!("Windows OCR: {e}")),
            Ok(d) => parse_out(&String::from_utf8_lossy(&d.out), d.ok, &d.reason()),
        }
    })();
    let _ = std::fs::remove_file(&png);
    let _ = std::fs::remove_file(&ps1);
    res
}

fn parse_out(s: &str, ok: bool, reason: &str) -> Result<Vec<Word>, String> {
    let mut ws = vec![];
    for l in s.trim_start_matches('\u{FEFF}').lines() {
        let l = l.trim_end_matches('\r');
        if l.trim() == "NOLANG" {
            return Err(NO_LANG.into());
        }
        if let Some(e) = l.strip_prefix("E\t") {
            return Err(format!("Windows OCR: {}", e.trim()));
        }
        let Some(rest) = l.strip_prefix("W\t") else { continue };
        let f: Vec<&str> = rest.splitn(6, '\t').collect();
        if f.len() < 6 || f[5].trim().is_empty() {
            continue;
        }
        let n = |i: usize| f[i].trim().replace(',', ".").parse::<f32>().unwrap_or(0.0);
        ws.push(Word { text: f[5].trim().to_string(), r: R::new(n(1), n(2), n(3), n(4)), line: f[0].trim().parse().unwrap_or(0), para: 0, conf: 100.0 });
    }
    if ws.is_empty() && !ok {
        return Err(format!("Windows OCR didn't work: {reason}"));
    }
    super::paras_from_gaps(&mut ws);
    Ok(ws)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn windows_output() {
        let out = "\u{FEFF}W\t0\t10\t20\t50\t14\tHello\r\nW\t0\t64\t20\t40\t14\twörld\r\nW\t1\t10,5\t40\t30\t14\tnext\r\n";
        let ws = parse_out(out, true, "").unwrap();
        assert_eq!(ws.len(), 3);
        assert_eq!(ws[2].r.x, 10.5);
        assert_eq!(super::super::text(&ws), "Hello wörld\nnext");
        assert_eq!(parse_out("NOLANG\r\n", false, "").unwrap_err(), NO_LANG);
        assert_eq!(parse_out("E\tThe file is broken\r\n", false, "").unwrap_err(), "Windows OCR: The file is broken");
        assert!(parse_out("", false, "Access denied").unwrap_err().contains("Access denied"));
        assert!(parse_out("", true, "").unwrap().is_empty());
        // the script uses only ASCII (PowerShell 5 reads a file without BOM as the ANSI code page)
        assert!(SCRIPT.is_ascii());
    }
}
