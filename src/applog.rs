//! Everything the app prints, kept in a file per launch.
//!
//! The app reports almost everything — a transcript that failed, a chapter
//! that did not close, a team file that did not decrypt — as a line on stderr.
//! Launched from a terminal that scrolls away; launched any other way it goes
//! nowhere at all, which left "it got stuck on chapter 02" with nothing to read
//! afterwards. So at launch stdout and stderr are pointed at a pipe, and a
//! thread copies what comes through it to the original stderr *and* to
//! `~/.stream-recorder/logs/<utc time>.log`, each line stamped with the time.
//!
//! At the file-descriptor level rather than a `tracing` writer, because most
//! of the output is `eprintln!`, and because the child processes — ffmpeg,
//! sops, the renderer — inherit the descriptors and are captured with it.
//!
//! Best effort, like the ledgers: if any step fails the app carries on with
//! stderr as it was.

use std::fs::File;
use std::io::{Read, Write};
use std::os::fd::FromRawFd;
use std::path::PathBuf;

/// Launch logs kept; the oldest beyond this are removed at launch.
const KEEP: usize = 30;

/// Start copying stdout and stderr into this launch's log. Returns its path.
pub fn start() -> Option<PathBuf> {
    let dir = PathBuf::from(std::env::var_os("HOME")?).join(".stream-recorder/logs");
    std::fs::create_dir_all(&dir).ok()?;
    prune(&dir);
    let path = dir.join(format!(
        "{}.log",
        chrono::Utc::now().format("%Y-%m-%d_%H-%M-%S")
    ));
    let mut log = File::create(&path).ok()?;

    let mut fds = [0; 2];
    // SAFETY: plain descriptor calls on descriptors this function owns, made
    // at the top of `main` before any other thread exists. Each result is
    // checked; on a failure nothing has been redirected yet, or what was is
    // put back.
    let (read_end, terminal) = unsafe {
        if libc::pipe(fds.as_mut_ptr()) != 0 {
            return None;
        }
        let terminal = libc::dup(2);
        if terminal < 0 {
            libc::close(fds[0]);
            libc::close(fds[1]);
            return None;
        }
        if libc::dup2(fds[1], 1) < 0 || libc::dup2(fds[1], 2) < 0 {
            libc::dup2(terminal, 2);
            libc::close(fds[0]);
            libc::close(fds[1]);
            libc::close(terminal);
            return None;
        }
        libc::close(fds[1]);
        (File::from_raw_fd(fds[0]), File::from_raw_fd(terminal))
    };

    let _ = writeln!(
        log,
        "{} saaga-social-flow {} launched",
        stamp(),
        env!("CARGO_PKG_VERSION")
    );
    let copier = std::thread::Builder::new()
        .name("applog".into())
        .spawn(move || copy(read_end, terminal, log));
    if copier.is_err() {
        // Nothing would drain the pipe, and a full pipe blocks every print.
        // SAFETY: `dup(2)` above is gone with the closure, so reopen the tty
        // the only way left: point both back at /dev/tty if there is one.
        unsafe {
            let tty = libc::open(c"/dev/tty".as_ptr(), libc::O_WRONLY);
            if tty >= 0 {
                libc::dup2(tty, 1);
                libc::dup2(tty, 2);
                libc::close(tty);
            }
        }
        return None;
    }
    eprintln!("stream-recorder: logging to {}", path.display());
    Some(path)
}

/// Copy the pipe to the terminal and to the log until every writer is gone.
///
/// Write errors are ignored rather than ending the loop: a closed terminal is
/// normal for an app, and a thread that stopped reading would leave the pipe
/// to fill and every later print — the app's and its children's — to block.
fn copy(mut from: File, mut terminal: File, mut log: File) {
    let mut buf = [0u8; 8192];
    let mut line_start = true;
    loop {
        let n = match from.read(&mut buf) {
            Ok(0) => return,
            Ok(n) => n,
            Err(err) if err.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(_) => return,
        };
        let chunk = &buf[..n];
        let _ = terminal.write_all(chunk);
        let mut stamped = Vec::with_capacity(n + 64);
        for &byte in chunk {
            if line_start {
                stamped.extend_from_slice(stamp().as_bytes());
                stamped.push(b' ');
            }
            stamped.push(byte);
            line_start = byte == b'\n';
        }
        let _ = log.write_all(&stamped);
    }
}

fn stamp() -> String {
    chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string()
}

/// Keep the newest [`KEEP`] logs. The names sort by time, so by name is enough.
fn prune(dir: &std::path::Path) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let mut logs: Vec<PathBuf> = entries
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|x| x == "log"))
        .collect();
    logs.sort();
    let excess = logs.len().saturating_sub(KEEP - 1);
    for old in &logs[..excess] {
        let _ = std::fs::remove_file(old);
    }
}
