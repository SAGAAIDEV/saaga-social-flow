//! Reading the upload bar the Python uploader writes to stderr.
//!
//! `screencast.platforms.media_host._UploadProgress` prints a boto3 transfer
//! callback as a single line, repainted in place with a carriage return:
//!
//! ```text
//! \r\x1b[K  ██████░░░░░░░░  45.2%  54.3/120.1 MB  chapter-01.mp4
//! ```
//!
//! Two consequences shape everything here. The separator is `\r`, not `\n`, so a
//! line-oriented reader sees *nothing* until the upload finishes and the final
//! newline lands — which is exactly the "no progress until it's done" behaviour
//! this module exists to fix. And the payload carries an ANSI erase-to-end-of-line
//! that has to come off before parsing.
//!
//! Only files over ~1 MB get a bar on the Python side, so small companion files
//! (transcripts) legitimately produce no progress at all.

/// One repaint of the upload bar.
#[derive(Debug, Clone, PartialEq)]
pub struct Progress {
    pub pct: f64,
    pub seen_mb: f64,
    pub total_mb: f64,
    pub label: String,
}

/// Splits a raw stderr chunk into segments on either separator.
///
/// `\r` is what the bar repaints with and `\n` ends the final update and any
/// ordinary log line, so both terminate a segment.
pub fn segments(chunk: &str) -> impl Iterator<Item = &str> {
    chunk
        .split(['\r', '\n'])
        .map(str::trim_end)
        .filter(|part| !part.trim().is_empty())
}

/// Parses one segment, or `None` when it is an ordinary log line rather than a bar.
pub fn parse(segment: &str) -> Option<Progress> {
    let clean = strip_ansi(segment);
    let tokens: Vec<&str> = clean.split_whitespace().collect();
    let at = tokens.iter().position(|t| t.ends_with('%'))?;
    let pct: f64 = tokens[at].trim_end_matches('%').parse().ok()?;
    // "54.3/120.1" followed by "MB" — the shape the callback prints.
    let (seen, total) = tokens.get(at + 1)?.split_once('/')?;
    let seen_mb: f64 = seen.parse().ok()?;
    let total_mb: f64 = total.parse().ok()?;
    if tokens.get(at + 2).copied() != Some("MB") {
        return None;
    }
    let label = tokens[at + 3..].join(" ");
    Some(Progress {
        pct: pct.clamp(0.0, 100.0),
        seen_mb,
        total_mb,
        label,
    })
}

/// Drops ANSI CSI escapes (the bar's erase-to-end-of-line, and any colour codes).
fn strip_ansi(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars();
    while let Some(ch) = chars.next() {
        if ch != '\x1b' {
            out.push(ch);
            continue;
        }
        // CSI: ESC '[' … final byte in @..~
        match chars.next() {
            Some('[') => {}
            // A bare ESC is dropped, but whatever followed it is real text.
            Some(other) => {
                out.push(other);
                continue;
            }
            None => break,
        }
        for next in chars.by_ref() {
            if ('\x40'..='\x7e').contains(&next) {
                break;
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const BAR: &str = "\r\x1b[K  ██████░░░░░░░░  45.2%  54.3/120.1 MB  chapter-01.mp4";

    #[test]
    fn a_repaint_parses_into_percent_bytes_and_label() {
        let segment = segments(BAR).next().expect("one segment");
        let got = parse(segment).expect("a progress line");
        assert_eq!(got.pct, 45.2);
        assert_eq!(got.seen_mb, 54.3);
        assert_eq!(got.total_mb, 120.1);
        assert_eq!(got.label, "chapter-01.mp4");
    }

    /// The whole point: several repaints arrive in one read, separated by `\r`.
    #[test]
    fn one_chunk_can_hold_many_repaints() {
        let chunk = "\r\x1b[K  ██  10.0%  1.0/10.0 MB  a.mp4\
                     \r\x1b[K  ████  50.0%  5.0/10.0 MB  a.mp4\
                     \r\x1b[K  ██████  100.0%  10.0/10.0 MB  a.mp4\n";
        let seen: Vec<f64> = segments(chunk).filter_map(parse).map(|p| p.pct).collect();
        assert_eq!(seen, vec![10.0, 50.0, 100.0]);
    }

    #[test]
    fn ordinary_log_lines_are_not_progress() {
        assert!(parse("Uploading to my-bucket: chapter-01.mp4 (120.1 MB)").is_none());
        assert!(parse("S3_BUCKET unset — set it in screencast/.env").is_none());
        assert!(parse("").is_none());
        // A percentage with no byte counts after it is a log line, not a bar.
        assert!(parse("  done 100% of the work").is_none());
    }

    #[test]
    fn a_label_with_spaces_survives() {
        let got = parse("  ██  12.5%  1.0/8.0 MB  my chapter 01.mp4").expect("progress");
        assert_eq!(got.label, "my chapter 01.mp4");
    }

    #[test]
    fn ansi_escapes_come_off() {
        assert_eq!(strip_ansi("\x1b[K  50.0%"), "  50.0%");
        assert_eq!(strip_ansi("\x1b[1;32mgreen\x1b[0m"), "green");
        assert_eq!(strip_ansi("plain"), "plain");
        // A lone ESC with no CSI must not eat the rest of the string.
        assert_eq!(strip_ansi("a\x1bb"), "ab");
    }

    #[test]
    fn a_percentage_over_a_hundred_is_clamped() {
        // boto3 can overshoot on a multipart retry.
        let got = parse("  ██  103.4%  10.4/10.0 MB  a.mp4").expect("progress");
        assert_eq!(got.pct, 100.0);
        assert_eq!(got.seen_mb, 10.4, "the raw byte counts are left alone");
    }

    #[test]
    fn blank_and_whitespace_segments_are_dropped() {
        assert_eq!(segments("\r\n  \r\n").count(), 0);
        assert_eq!(segments("one\rtwo").count(), 2);
    }
}
