//! Test driver for the headless KWin runs (scripts/headless-test.sh): real pointer + keyboard input through
//! KWin's fake-input protocol (allowed there with KWIN_WAYLAND_NO_PERMISSION_CHECKS=1), so the keys go the real
//! way: KWin -> Lasso's layer surface -> xkb -> Lasso's key map. Never run it in the real session.
//!
//! Reads steps from the file given as the first argument, one per line:
//!   abs X Y | down | up | rdown | rup | click X Y | dclick X Y | drag X1 Y1 X2 Y2 [steps] | wheel N
//!   key NAME | combo ctrl+shift+z | hold NAME | release NAME | type text | wait MS | sh COMMAND | # comment

use std::io::BufRead;
use wayland_client::globals::{registry_queue_init, GlobalListContents};
use wayland_client::protocol::wl_registry::{self, WlRegistry};
use wayland_client::{Connection, Dispatch, QueueHandle};
use wayland_protocols_plasma::fake_input::client::org_kde_kwin_fake_input::OrgKdeKwinFakeInput;

struct St;
impl Dispatch<WlRegistry, GlobalListContents> for St {
    fn event(_: &mut Self, _: &WlRegistry, _: wl_registry::Event, _: &GlobalListContents, _: &Connection, _: &QueueHandle<Self>) {}
}
impl Dispatch<OrgKdeKwinFakeInput, ()> for St {
    fn event(_: &mut Self, _: &OrgKdeKwinFakeInput, _: <OrgKdeKwinFakeInput as wayland_client::Proxy>::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {}
}

fn code(name: &str) -> Option<u32> {
    let n = name.to_lowercase();
    let letters = "qwertyuiop";
    let row2 = "asdfghjkl";
    let row3 = "zxcvbnm";
    if n.len() == 1 {
        let c = n.chars().next().unwrap();
        if let Some(i) = letters.find(c) {
            return Some(16 + i as u32);
        }
        if let Some(i) = row2.find(c) {
            return Some(30 + i as u32);
        }
        if let Some(i) = row3.find(c) {
            return Some(44 + i as u32);
        }
        if let Some(d) = c.to_digit(10) {
            return Some(if d == 0 { 11 } else { 1 + d });
        }
        return match c {
            '[' => Some(26),
            ']' => Some(27),
            '/' => Some(53),
            ' ' => Some(57),
            '-' => Some(12),
            '=' => Some(13),
            '.' => Some(52),
            ',' => Some(51),
            _ => None,
        };
    }
    Some(match n.as_str() {
        "ctrl" | "control" => 29,
        "shift" => 42,
        "alt" => 56,
        "esc" | "escape" => 1,
        "enter" | "return" => 28,
        "tab" => 15,
        "space" => 57,
        "backspace" => 14,
        "delete" | "del" => 111,
        "left" => 105,
        "right" => 106,
        "up" => 103,
        "down" => 108,
        "f1" => 59,
        "print" => 99,
        _ => return None,
    })
}

fn main() {
    let file = std::env::args().nth(1).expect("steps file");
    let conn = Connection::connect_to_env().expect("wayland");
    let (globals, mut q) = registry_queue_init::<St>(&conn).expect("registry");
    let qh = q.handle();
    let fi: OrgKdeKwinFakeInput = globals.bind(&qh, 4..=5, ()).expect("org_kde_kwin_fake_input (run KWin with KWIN_WAYLAND_NO_PERMISSION_CHECKS=1)");
    fi.authenticate("lasso-test".into(), "headless test".into());
    q.roundtrip(&mut St).unwrap();
    let flush = |q: &mut wayland_client::EventQueue<St>| {
        let _ = q.roundtrip(&mut St);
    };
    let sleep = |ms: u64| std::thread::sleep(std::time::Duration::from_millis(ms));
    let key = |q: &mut wayland_client::EventQueue<St>, c: u32, down: bool| {
        fi.keyboard_key(c, if down { 1 } else { 0 });
        flush(q);
    };
    let lines: Vec<String> = std::io::BufReader::new(std::fs::File::open(file).unwrap()).lines().map_while(Result::ok).collect();
    for l in lines {
        let l = l.trim();
        if l.is_empty() || l.starts_with('#') {
            continue;
        }
        let (cmd, rest) = l.split_once(' ').unwrap_or((l, ""));
        let nums: Vec<f64> = rest.split_whitespace().filter_map(|x| x.parse().ok()).collect();
        eprintln!("drive: {l}");
        match cmd {
            "abs" => {
                fi.pointer_motion_absolute(nums[0], nums[1]);
                flush(&mut q);
                sleep(30);
            }
            "down" | "up" | "rdown" | "rup" => {
                let b = if cmd.starts_with('r') { 0x111 } else { 0x110 };
                fi.button(b, if cmd.ends_with("down") { 1 } else { 0 });
                flush(&mut q);
                sleep(40);
            }
            "click" | "dclick" => {
                fi.pointer_motion_absolute(nums[0], nums[1]);
                flush(&mut q);
                sleep(40);
                for _ in 0..if cmd == "dclick" { 2 } else { 1 } {
                    fi.button(0x110, 1);
                    flush(&mut q);
                    sleep(30);
                    fi.button(0x110, 0);
                    flush(&mut q);
                    sleep(60);
                }
            }
            "drag" => {
                let steps = nums.get(4).copied().unwrap_or(12.0) as i32;
                fi.pointer_motion_absolute(nums[0], nums[1]);
                flush(&mut q);
                sleep(40);
                fi.button(0x110, 1);
                flush(&mut q);
                sleep(40);
                for i in 1..=steps {
                    let t = i as f64 / steps as f64;
                    fi.pointer_motion_absolute(nums[0] + (nums[2] - nums[0]) * t, nums[1] + (nums[3] - nums[1]) * t);
                    flush(&mut q);
                    sleep(16);
                }
                fi.button(0x110, 0);
                flush(&mut q);
                sleep(60);
            }
            "wheel" => {
                fi.axis(0, nums[0]);
                flush(&mut q);
                sleep(40);
            }
            "key" => {
                let c = code(rest).unwrap_or_else(|| panic!("key {rest}"));
                key(&mut q, c, true);
                sleep(20);
                key(&mut q, c, false);
                sleep(60);
            }
            "hold" => {
                key(&mut q, code(rest).expect("key"), true);
                sleep(30);
            }
            "release" => {
                key(&mut q, code(rest).expect("key"), false);
                sleep(30);
            }
            "combo" => {
                let parts: Vec<u32> = rest.split('+').map(|p| code(p).unwrap_or_else(|| panic!("key {p}"))).collect();
                for &c in &parts {
                    key(&mut q, c, true);
                    sleep(15);
                }
                for &c in parts.iter().rev() {
                    key(&mut q, c, false);
                    sleep(15);
                }
                sleep(60);
            }
            "type" => {
                for ch in rest.chars() {
                    let upper = ch.is_ascii_uppercase();
                    let c = code(&ch.to_ascii_lowercase().to_string()).unwrap_or(57);
                    if upper {
                        key(&mut q, 42, true);
                    }
                    key(&mut q, c, true);
                    key(&mut q, c, false);
                    if upper {
                        key(&mut q, 42, false);
                    }
                    sleep(25);
                }
                sleep(60);
            }
            "wait" => sleep(nums[0] as u64),
            "sh" => {
                let st = std::process::Command::new("bash").arg("-c").arg(rest).status();
                eprintln!("drive: sh -> {st:?}");
            }
            _ => panic!("unknown step {l}"),
        }
    }
}
