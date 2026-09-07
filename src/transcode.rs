//! Post-record format conversions via an `ffmpeg` subprocess.
//!
//! Live capture only ever produces natively-encodable formats (AAC/`.m4a`)
//! — `AVAssetWriter` has no MP3 encoder at all, Apple exposes no public API
//! for one. Anything needing MP3 specifically is therefore a derived file
//! made here, after the fact, not part of the capture pipeline itself. The
//! source file is kept: it stays the primary artifact, the mp3 is a smaller
//! derived convenience copy, matching how this pipeline's existing
//! `audio.mp3` is already described as extracted-after-the-fact rather than
//! captured directly.

use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{bail, Context, Result};

/// Stream-copy the audio track from an `.mp4` to an `.m4a` alongside it
/// (same stem, `.m4a` extension). This is a lossless copy, no re-encoding.
pub fn extract_audio_copy(source_mp4: &Path) -> Result<PathBuf> {
    let m4a_path = source_mp4.with_extension("m4a");
    let status = Command::new("ffmpeg")
        .arg("-v")
        .arg("error")
        .arg("-y")
        .arg("-i")
        .arg(source_mp4)
        .args(["-vn", "-codec:a", "copy"])
        .arg(&m4a_path)
        .status()
        .context("failed to run ffmpeg for audio extraction")?;
    if !status.success() {
        bail!("ffmpeg audio extraction exited with {status}");
    }
    Ok(m4a_path)
}

/// Rebuild the MP4 container metadata in place to fix broken duration.
/// AVAssetWriter sometimes sets incorrect duration metadata when multiple
/// writers feed from the same continuous capture stream (chapter switching).
/// This re-muxes the file with corrected metadata (stream-copy, no re-encoding).
pub fn fix_mp4_metadata(mp4_path: &Path) -> Result<()> {
    let temp_path = mp4_path.with_extension("mp4.tmp");
    let status = Command::new("ffmpeg")
        .arg("-v")
        .arg("error")
        .arg("-y")
        .arg("-i")
        .arg(mp4_path)
        // -f mp4 is required: the temp file's .tmp extension defeats ffmpeg's
        // format inference and it refuses to write otherwise.
        .args(["-codec", "copy", "-f", "mp4"])
        .arg(&temp_path)
        .status()
        .context("failed to run ffmpeg for mp4 metadata rebuild")?;
    if !status.success() {
        bail!("ffmpeg mp4 metadata rebuild exited with {status}");
    }
    std::fs::rename(&temp_path, mp4_path)
        .context("failed to replace original mp4 with metadata-fixed version")?;
    Ok(())
}

/// Transcode audio from an `.mp4` to an MP3 alongside it (same stem, `.mp3` extension).
/// Pulls the audio directly from the mp4 file, no intermediate .m4a needed.
///
/// 64kbps mono: AAC is already more bit-efficient than MP3, so transcoding
/// to an MP3 bitrate *higher* than the AAC source's own would grow the file
/// while adding a second lossy pass — the opposite of the point. This stays
/// below the source's own bitrate so the mp3 is smaller, at some further
/// quality cost; it's good enough for spoken word, not archival fidelity.
/// If MP3 specifically isn't required (only smaller files are), the source
/// `.m4a` extracted via `extract_audio_copy` is already the smaller, better-quality option on its own.
pub fn to_mp3(source_mp4: &Path) -> Result<PathBuf> {
    let mp3_path = source_mp4.with_extension("mp3");
    let status = Command::new("ffmpeg")
        .arg("-v")
        .arg("error")
        .arg("-y")
        .arg("-i")
        .arg(source_mp4)
        .args(["-vn", "-codec:a", "libmp3lame", "-b:a", "64k"])
        .arg(&mp3_path)
        .status()
        .context("failed to run ffmpeg for mp3 transcode")?;
    if !status.success() {
        bail!("ffmpeg mp3 transcode exited with {status}");
    }
    Ok(mp3_path)
}

/// The loudest sample in `audio`, in dBFS, from ffmpeg's `volumedetect`.
///
/// What tells a chapter recorded from a muted or virtual device apart from one
/// somebody spoke into, before it is uploaded for a transcript that will come
/// back empty. ffmpeg reports digital silence as -91 dB rather than negative
/// infinity, so the answer is always a number a threshold can read.
pub fn peak_dbfs(audio: &Path) -> Result<f32> {
    let output = Command::new("ffmpeg")
        // The report is printed at info level, which `-v error` would swallow
        // along with the progress noise `-nostats` is here to drop.
        .args(["-hide_banner", "-nostats", "-v", "info", "-i"])
        .arg(audio)
        .args(["-vn", "-af", "volumedetect", "-f", "null", "-"])
        .output()
        .context("failed to run ffmpeg volumedetect")?;
    if !output.status.success() {
        bail!("ffmpeg volumedetect exited with {}", output.status);
    }
    parse_max_volume(&String::from_utf8_lossy(&output.stderr))
        .with_context(|| format!("ffmpeg reported no max_volume for {}", audio.display()))
}

/// Pulls `max_volume: -91.0 dB` out of ffmpeg's log.
fn parse_max_volume(log: &str) -> Option<f32> {
    log.lines().find_map(|line| {
        let (_, value) = line.split_once("max_volume:")?;
        value.trim().trim_end_matches("dB").trim().parse().ok()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn max_volume_is_read_out_of_the_volumedetect_log() {
        let log = "[Parsed_volumedetect_0 @ 0x600003e5c240] n_samples: 1054592\n\
                   [Parsed_volumedetect_0 @ 0x600003e5c240] mean_volume: -91.0 dB\n\
                   [Parsed_volumedetect_0 @ 0x600003e5c240] max_volume: -22.5 dB\n\
                   [Parsed_volumedetect_0 @ 0x600003e5c240] histogram_22db: 3\n";
        assert_eq!(parse_max_volume(log), Some(-22.5));
        assert_eq!(parse_max_volume("no report here"), None);
    }

    /// Runs the real ffmpeg, which nothing in this module works without anyway.
    /// A second of nothing measures at ffmpeg's silence floor and a tone does
    /// not — the two readings a silent-chapter check has to tell apart.
    #[test]
    fn silence_and_a_tone_measure_apart() {
        let dir = std::env::temp_dir().join(format!(
            "stream-recorder-peak-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let silent = dir.join("silent.mp3");
        let tone = dir.join("tone.mp3");
        for (path, source) in [
            (&silent, "anullsrc=r=48000:cl=mono"),
            // lavfi's sine is 1/8 full scale, about -18 dBFS.
            (&tone, "sine=frequency=440:sample_rate=48000"),
        ] {
            let status = Command::new("ffmpeg")
                .args(["-v", "error", "-y", "-f", "lavfi", "-i", source, "-t", "1"])
                .arg(path)
                .status()
                .expect("ffmpeg");
            assert!(status.success(), "generating {}", path.display());
        }
        assert!(peak_dbfs(&silent).unwrap() <= -90.0);
        assert!(peak_dbfs(&tone).unwrap() > -30.0);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
