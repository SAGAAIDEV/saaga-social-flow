//! Frame-accurate keep-list cut via ffmpeg, matching screencast `cutsources`.

use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{bail, Context, Result};

use super::compute::Edit;

const DELIVERABLE_CRF: &str = "18";
const DELIVERABLE_PRESET: &str = "medium";

pub fn cut_file(source: &Path, dest: &Path, edits: &[Edit]) -> Result<()> {
    if edits.is_empty() {
        bail!("no keep-segments to cut from {}", source.display());
    }
    dest.parent()
        .map(std::fs::create_dir_all)
        .transpose()
        .with_context(|| format!("creating {}", dest.display()))?;
    let fps = probe_fps(source)?;
    let audio = has_audio(source)?;
    let tmp = temp_dir(source)?;
    let result = cut_into(&tmp, source, dest, edits, fps, audio);
    let _ = std::fs::remove_dir_all(&tmp);
    result
}

fn cut_into(
    tmp: &Path,
    source: &Path,
    dest: &Path,
    edits: &[Edit],
    fps: f64,
    audio: bool,
) -> Result<()> {
    let mut parts = Vec::with_capacity(edits.len());
    for (i, edit) in edits.iter().enumerate() {
        let part = tmp.join(format!("seg_{i:04}.mp4"));
        cut_segment(source, &part, edit, fps, audio)
            .with_context(|| format!("cutting segment {i} from {}", source.display()))?;
        parts.push(part);
    }
    let list = tmp.join("concat.txt");
    let mut body = String::new();
    for part in &parts {
        body.push_str(&format!("file '{}'\n", part.display()));
    }
    std::fs::write(&list, body).with_context(|| format!("writing {}", list.display()))?;
    let raw = tmp.join("concat_raw.mp4");
    run(
        Command::new("ffmpeg")
            .args(["-y", "-f", "concat", "-safe", "0", "-i"])
            .arg(&list)
            .args(["-c", "copy"])
            .arg(&raw),
        "concat segments",
    )?;
    let mut encode = Command::new("ffmpeg");
    encode
        .args(["-y", "-fflags", "+genpts", "-i"])
        .arg(&raw)
        .args([
            "-c:v",
            "libx264",
            "-profile:v",
            "high",
            "-pix_fmt",
            "yuv420p",
            "-crf",
            DELIVERABLE_CRF,
            "-preset",
            DELIVERABLE_PRESET,
        ]);
    if audio {
        encode.args(["-c:a", "aac", "-b:a", "192k", "-ar", "48000", "-ac", "2"]);
    } else {
        encode.arg("-an");
    }
    encode.args(["-movflags", "+faststart"]).arg(dest);
    run(&mut encode, "encode deliverable")
}

fn cut_segment(source: &Path, dest: &Path, edit: &Edit, fps: f64, audio: bool) -> Result<()> {
    let start = edit.start as f64 / 1000.0;
    let duration = (edit.end - edit.start).max(1) as f64 / 1000.0;
    let n_frames = ((duration * fps).round() as i64).max(1);
    let exact = n_frames as f64 / fps;
    let timescale = (fps.round() as i64).max(1) * 1000;
    let vf = format!("fps={fps},setpts=PTS-STARTPTS");
    let mut cmd = Command::new("ffmpeg");
    cmd.args(["-y", "-ss"])
        .arg(format!("{start:.6}"))
        .arg("-i")
        .arg(source)
        .args(["-vf", &vf, "-frames:v", &n_frames.to_string()])
        .args(["-c:v", "libx264", "-preset", "ultrafast", "-crf", "0"]);
    if audio {
        let af = format!("atrim=end={exact:.6},asetpts=PTS-STARTPTS");
        cmd.args([
            "-af",
            &af,
            "-c:a",
            "pcm_s16le",
            "-ar",
            "48000",
            "-ac",
            "2",
        ]);
    } else {
        cmd.arg("-an");
    }
    cmd.args([
        "-video_track_timescale",
        &timescale.to_string(),
        "-avoid_negative_ts",
        "make_zero",
    ])
    .arg(dest);
    run(&mut cmd, "cut segment")
}

/// Re-encodes `source` to the settings a cut deliverable carries, at `fps`.
///
/// For making a HyperFrames render concat-compatible with the cut chapters it sits
/// between. The concat demuxer copies streams rather than decoding them, so every input
/// has to agree on codec, profile, pixel format and frame rate — and HyperFrames writes
/// Constrained Baseline at its own frame rate, where a cut is High at the take's. Left
/// mismatched, the concat either warns and produces a file that stutters at every join
/// or fails outright.
///
/// Cheap by construction: this is only ever pointed at a three-second title card.
pub fn conform(source: &Path, dest: &Path, fps: f64) -> Result<()> {
    dest.parent()
        .map(std::fs::create_dir_all)
        .transpose()
        .with_context(|| format!("creating {}", dest.display()))?;
    let silent = !has_audio(source)?;
    let mut encode = Command::new("ffmpeg");
    encode.args(["-v", "error", "-y", "-i"]).arg(source);
    // Every input first: an option placed after an `-i` binds to the *next* input, so
    // the encoder settings below have to come after the last one.
    if silent {
        // A card with no audio track between two chapters that have one leaves the
        // concat's audio shorter than its video, and everything after the join drifts.
        // So a silent card gets real silence rather than nothing.
        encode.args([
            "-f",
            "lavfi",
            "-i",
            "anullsrc=channel_layout=stereo:sample_rate=48000",
        ]);
    }
    encode.args([
        "-map",
        "0:v:0",
        "-map",
        if silent { "1:a:0" } else { "0:a:0" },
        "-vf",
        &format!("fps={fps}"),
        "-c:v",
        "libx264",
        "-profile:v",
        "high",
        "-pix_fmt",
        "yuv420p",
        "-crf",
        DELIVERABLE_CRF,
        "-preset",
        DELIVERABLE_PRESET,
        "-c:a",
        "aac",
        "-b:a",
        "192k",
        "-ar",
        "48000",
        "-ac",
        "2",
    ]);
    if silent {
        // Without this the generated silence never ends and neither does the encode.
        encode.arg("-shortest");
    }
    // Deliberately no `-video_track_timescale`: the cut's own deliverable encode does not
    // set one either, so leaving it to ffmpeg is what makes both sides land on the same
    // timebase. Pinning a "tidier" value here put the card on 1/25000 against the
    // chapters' 1/12800 and left the concat rescaling timestamps at every join.
    encode.args(["-movflags", "+faststart"]).arg(dest);
    run(&mut encode, "conform to the cut's encoder settings")
}

/// The frame rate of `path`, as the cut reads it.
pub fn probe_frame_rate(path: &Path) -> Result<f64> {
    probe_fps(path)
}

pub fn extract_mp3(source: &Path, dest: &Path) -> Result<()> {
    dest.parent()
        .map(std::fs::create_dir_all)
        .transpose()
        .with_context(|| format!("creating {}", dest.display()))?;
    run(
        Command::new("ffmpeg")
            .args(["-v", "error", "-y", "-i"])
            .arg(source)
            .args(["-vn", "-codec:a", "libmp3lame", "-b:a", "64k"])
            .arg(dest),
        "extract mp3",
    )
}

fn probe_fps(path: &Path) -> Result<f64> {
    let out = Command::new("ffprobe")
        .args([
            "-v",
            "error",
            "-select_streams",
            "v:0",
            "-show_entries",
            "stream=r_frame_rate",
            "-of",
            "csv=p=0",
        ])
        .arg(path)
        .output()
        .context("running ffprobe")?;
    if !out.status.success() {
        bail!(
            "ffprobe failed on {}: {}",
            path.display(),
            String::from_utf8_lossy(&out.stderr)
        );
    }
    parse_rate(std::str::from_utf8(&out.stdout).unwrap_or("").trim())
        .with_context(|| format!("reading frame rate of {}", path.display()))
}

pub fn probe_duration_seconds(path: &Path) -> Result<f64> {
    let out = Command::new("ffprobe")
        .args([
            "-v",
            "error",
            "-show_entries",
            "format=duration",
            "-of",
            "csv=p=0",
        ])
        .arg(path)
        .output()
        .context("running ffprobe for duration")?;
    if !out.status.success() {
        bail!(
            "ffprobe duration failed on {}: {}",
            path.display(),
            String::from_utf8_lossy(&out.stderr)
        );
    }
    let raw = std::str::from_utf8(&out.stdout).unwrap_or("").trim();
    raw.parse::<f64>()
        .with_context(|| format!("duration of {}: {raw:?}", path.display()))
}

pub fn concat_videos(inputs: &[PathBuf], dest: &Path) -> Result<()> {
    if inputs.is_empty() {
        bail!("no videos to concatenate");
    }
    dest.parent()
        .map(std::fs::create_dir_all)
        .transpose()
        .with_context(|| format!("creating {}", dest.display()))?;
    let tmp = temp_dir(dest)?;
    let list = tmp.join("concat.txt");
    let mut body = String::new();
    for path in inputs {
        body.push_str(&format!("file '{}'\n", path.display()));
    }
    std::fs::write(&list, body).with_context(|| format!("writing {}", list.display()))?;
    let result = run(
        Command::new("ffmpeg")
            .args(["-y", "-f", "concat", "-safe", "0", "-i"])
            .arg(&list)
            .args(["-c", "copy", "-movflags", "+faststart"])
            .arg(dest),
        "concat publish",
    )
    .and_then(|()| align_tracks(dest));
    let _ = std::fs::remove_dir_all(&tmp);
    result
}

/// Pulls the joined video track back to where the audio starts.
///
/// The black frame at the head of every longform lives here. Both inputs to the
/// concat are clean — video and audio each declare a normal priming edit — but
/// the concat demuxer trims the audio's priming to start it at zero and leaves
/// the video a fraction of a frame behind it. The mp4 muxer records that lead as
/// an *empty edit* (`media time: -1`), which means "display nothing here", and
/// every player that honours edit lists — QuickTime, Safari, the Video details pane's
/// WKWebView, and the thumbnailers the social platforms run — draws it black.
///
/// It is invisible to the obvious checks, which is why it survived: `blackdetect`
/// decodes frames and never sees the container, `-ss 0` seeks past the gap, and
/// `ffprobe` on the file reports `start_time=0` because it silently *applies* the
/// edit list it is being asked about.
///
/// Both streams are copied, so nothing is re-encoded and no frame or sample is
/// dropped — only the video's presentation offset moves, and it moves toward the
/// audio it was lagging. This closes a desync rather than creating one.
fn align_tracks(path: &Path) -> Result<()> {
    let offset = probe_video_start_seconds(path)?;
    // Under a millisecond there is nothing to close, and a second pass would
    // rewrite the whole longform to change nothing.
    if offset <= 0.001 || !has_audio(path)? {
        return Ok(());
    }
    let aligned = path.with_extension("aligned.mp4");
    run(
        Command::new("ffmpeg")
            .args(["-y", "-v", "error", "-itsoffset", &format!("-{offset:.6}"), "-i"])
            .arg(path)
            // The same file again, un-offset, so the audio keeps its own timing:
            // `-itsoffset` binds to the next input only.
            .arg("-i")
            .arg(path)
            .args(["-map", "0:v:0", "-map", "1:a:0", "-c", "copy"])
            .args(["-movflags", "+faststart"])
            .arg(&aligned),
        "aligning the longform's tracks",
    )?;
    std::fs::rename(&aligned, path)
        .with_context(|| format!("replacing {} with the aligned copy", path.display()))
}

/// Where the video track says it starts, in seconds. `0.0` when it does not say.
fn probe_video_start_seconds(path: &Path) -> Result<f64> {
    let out = Command::new("ffprobe")
        .args([
            "-v",
            "error",
            "-select_streams",
            "v:0",
            "-show_entries",
            "stream=start_time",
            "-of",
            "csv=p=0",
        ])
        .arg(path)
        .output()
        .context("running ffprobe for the video start time")?;
    if !out.status.success() {
        bail!(
            "ffprobe could not read the start time of {}: {}",
            path.display(),
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    // "N/A" for a stream that declares none, which is not an error — it means
    // there is no offset to close.
    Ok(std::str::from_utf8(&out.stdout)
        .unwrap_or("")
        .trim()
        .parse::<f64>()
        .unwrap_or(0.0)
        .max(0.0))
}

fn parse_rate(raw: &str) -> Result<f64> {
    let raw = raw.trim();
    if let Some((n, d)) = raw.split_once('/') {
        let n: f64 = n.parse().context("fps numerator")?;
        let d: f64 = d.parse().context("fps denominator")?;
        if d == 0.0 {
            bail!("fps denominator is 0");
        }
        return Ok(n / d);
    }
    raw.parse::<f64>().context("fps")
}

fn has_audio(path: &Path) -> Result<bool> {
    let out = Command::new("ffprobe")
        .args([
            "-v",
            "error",
            "-select_streams",
            "a",
            "-show_entries",
            "stream=index",
            "-of",
            "csv=p=0",
        ])
        .arg(path)
        .output()
        .context("running ffprobe for audio")?;
    Ok(out.status.success() && !out.stdout.is_empty())
}

fn run(cmd: &mut Command, what: &str) -> Result<()> {
    let out = cmd
        .output()
        .with_context(|| format!("running ffmpeg ({what})"))?;
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr);
        let tail: String = err.chars().rev().take(800).collect::<String>().chars().rev().collect();
        bail!("ffmpeg {what} failed: {tail}");
    }
    Ok(())
}

fn temp_dir(source: &Path) -> Result<PathBuf> {
    let stem = source
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("cut");
    let dir = std::env::temp_dir().join(format!(
        "stream-recorder-cut-{}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis())
            .unwrap_or(0),
        stem
    ));
    std::fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;
    Ok(dir)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::edit::compute::Edit;

    fn ffmpeg_ok() -> bool {
        Command::new("ffmpeg").arg("-version").output().is_ok()
            && Command::new("ffprobe").arg("-version").output().is_ok()
    }

    fn edit(start: i64, end: i64) -> Edit {
        Edit {
            index: 0,
            start,
            end,
            duration: end - start,
            text: String::new(),
            start_word_idx: 0,
            end_word_idx: 0,
            disfluency_group: 0,
            extended_silence: 0,
        }
    }

    #[test]
    fn parse_rate_handles_ratio_and_float() {
        assert!((parse_rate("30/1").unwrap() - 30.0).abs() < f64::EPSILON);
        assert!((parse_rate("30000/1001").unwrap() - 29.970).abs() < 0.01);
        assert!((parse_rate("24").unwrap() - 24.0).abs() < f64::EPSILON);
    }

    #[test]
    fn cut_keeps_two_halves_of_a_generated_clip() {
        if !ffmpeg_ok() {
            return;
        }
        let dir = std::env::temp_dir().join(format!(
            "stream-recorder-cut-test-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let src = dir.join("src.mp4");
        let dest = dir.join("out.mp4");
        let status = Command::new("ffmpeg")
            .args([
                "-y",
                "-f",
                "lavfi",
                "-i",
                "testsrc=duration=2:size=320x240:rate=30",
                "-f",
                "lavfi",
                "-i",
                "sine=frequency=440:duration=2",
                "-c:v",
                "libx264",
                "-pix_fmt",
                "yuv420p",
                "-c:a",
                "aac",
                "-shortest",
            ])
            .arg(&src)
            .status()
            .expect("ffmpeg generate");
        assert!(status.success());
        cut_file(&src, &dest, &[edit(0, 500), edit(1000, 1500)]).expect("cut");
        assert!(dest.exists());
        assert!(dest.metadata().unwrap().len() > 0);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
