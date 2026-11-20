//! Run an OCR program with input on stdin, collect stdout / stderr, and give up after a time limit (a stuck engine
//! must not leave the frozen screen "busy" forever).

use std::io::{Read, Write};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

#[derive(Debug)]
pub enum Fail {
    NotFound,
    TimedOut,
    Io(String),
}

pub struct Done {
    pub ok: bool,
    pub out: Vec<u8>,
    pub err: Vec<u8>,
}

impl Done {
    /// The last non-empty stderr line (engines put the reason there)
    pub fn reason(&self) -> String {
        let e = String::from_utf8_lossy(&self.err);
        e.lines().map(str::trim).rfind(|l| !l.is_empty()).unwrap_or("failed").to_string()
    }
}

fn drain<R: Read + Send + 'static>(r: Option<R>) -> Option<std::thread::JoinHandle<Vec<u8>>> {
    r.map(|mut r| {
        std::thread::spawn(move || {
            let mut v = vec![];
            let _ = r.read_to_end(&mut v);
            v
        })
    })
}

pub fn run(mut cmd: Command, input: Option<Vec<u8>>, limit: Duration) -> Result<Done, Fail> {
    cmd.stdin(if input.is_some() { Stdio::piped() } else { Stdio::null() }).stdout(Stdio::piped()).stderr(Stdio::piped());
    let mut child = cmd.spawn().map_err(|e| if e.kind() == std::io::ErrorKind::NotFound { Fail::NotFound } else { Fail::Io(e.to_string()) })?;
    if let (Some(data), Some(mut sin)) = (input, child.stdin.take()) {
        // own thread: a big picture fills the pipe before the program starts writing its answer
        std::thread::spawn(move || {
            let _ = sin.write_all(&data);
        });
    }
    let (ho, he) = (drain(child.stdout.take()), drain(child.stderr.take()));
    let t0 = Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(s)) => break s,
            Ok(None) => {}
            Err(e) => return Err(Fail::Io(e.to_string())),
        }
        if t0.elapsed() > limit {
            let _ = child.kill();
            let _ = child.wait();
            return Err(Fail::TimedOut);
        }
        std::thread::sleep(Duration::from_millis(15));
    };
    let join = |h: Option<std::thread::JoinHandle<Vec<u8>>>| h.and_then(|h| h.join().ok()).unwrap_or_default();
    Ok(Done { ok: status.success(), out: join(ho), err: join(he) })
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    #[test]
    fn pipes_times_out_and_not_found() {
        // more than a pipe buffer both ways
        let big: Vec<u8> = (0..300_000u32).map(|i| (i % 251) as u8).collect();
        let d = run(Command::new("cat"), Some(big.clone()), Duration::from_secs(10)).unwrap();
        assert!(d.ok);
        assert_eq!(d.out, big);

        let mut c = Command::new("sh");
        c.args(["-c", "echo first >&2; echo 'Failed loading language xyz' >&2; exit 3"]);
        let d = run(c, None, Duration::from_secs(10)).unwrap();
        assert!(!d.ok);
        assert_eq!(d.reason(), "Failed loading language xyz");

        let mut c = Command::new("sleep");
        c.arg("5");
        let t = Instant::now();
        assert!(matches!(run(c, None, Duration::from_millis(100)), Err(Fail::TimedOut)));
        assert!(t.elapsed() < Duration::from_secs(3));

        assert!(matches!(run(Command::new("lasso-no-such-program-here"), None, Duration::from_secs(1)), Err(Fail::NotFound)));
    }
}
