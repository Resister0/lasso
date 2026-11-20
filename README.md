<p align="center">
  <img src="packaging/icons/hicolor/128x128/apps/link.sonela.Lasso.png" width="96" height="96" alt="Lasso">
</p>

<h1 align="center">Lasso</h1>

<p align="center"><b>Screenshots with every editing tool right on the frozen screen.</b></p>

<p align="center">
  Press a key, the screen freezes in a blink, draw a box and mark it up - then copy, save or pin it.<br>
  Free, no ads. Open source. Windows and Linux (KDE Plasma first).
</p>

<p align="center">
  <a href="https://github.com/Resister0/lasso/releases/latest"><b>Download Lasso</b></a> ·
  <a href="https://sonela.app/lasso">sonela.app/lasso</a>
</p>

## What it does

- **Fast freeze**: area (drag), the window under the mouse (click), or the whole screen (Tab), with a pixel magnifier.
- **Every tool on the frozen screen**: pen, highlighter, line, arrow, rectangle, ellipse, text, numbered steps, blur,
  pixelate, spotlight, colour picker. 8 colours, size, fill, shadow, undo / redo.
- **Copy** to the clipboard, **save** with no dialog (Pictures/Lasso), or **pin** the picture on top of everything.
- **Esc closes at once and nothing is lost**: "Restore last capture" brings the frozen screen back with every shape.
- **Library** of older captures whose shapes stay editable, plus your saved pictures.
- **Full screenshot** with one command, **OCR** (copy the text in a picture) and a one-key blur of emails, numbers and keys.
- Every key can be changed in Settings; global hotkeys are off until you switch them on. Light on memory in the tray.

## Keys

| Key | Does |
|---|---|
| Drag / click / Tab | pick an area / the window under the mouse / switch Area, Window, Screen |
| Enter | the main action (copy, save, both or link - your choice) |
| Ctrl+C / Ctrl+S / Ctrl+Shift+S | copy / save / save as |
| Ctrl+T | pin to the screen |
| Ctrl+Z / Ctrl+Shift+Z | undo / redo |
| 1-8, `[` `]` | colour, size |
| Ctrl+B / Ctrl+I | bold / italic text |
| Ctrl+R | library |
| Ctrl+N | throw this away, pick a new box |
| ? or F1 | the full key sheet |
| Esc | close |

Command line: `lasso --region`, `--window`, `--screen`, `--full`, `--repeat`, `--restore`, `--library`, `--settings`,
`--open FILE`, `--quit`. Bind `lasso --region` to a key in your desktop's shortcut settings if you don't want Lasso's own hotkeys.

## Download

Get the newest files from the [Releases page](https://github.com/Resister0/lasso/releases/latest).

- **Windows**: `Lasso-Setup.exe` (installs for you, no admin) or `Lasso-Windows.zip` (portable).
- **Linux**: `Lasso-x86_64.AppImage` (updates itself) or `Lasso-linux-x86_64.tar.gz`.

`SHA256SUMS` lists the fingerprint of every file. On KDE Plasma, Lasso's first start writes its menu entry; that is what
lets it take screenshots silently and instantly.

## Build

Rust (stable) and the Slint toolchain are all you need.

```bash
cargo build --release
```

On Linux you also need the development packages for fontconfig, freetype, xkbcommon, wayland and dbus
(Ubuntu: `libfontconfig1-dev libfreetype6-dev libxkbcommon-dev libwayland-dev libdbus-1-dev pkg-config`).

```bash
cargo test --release
```

## License

Free software under the [GNU GPL v3 or later](LICENSE). Part of [Sonela](https://sonela.app), a family of small, free apps.
