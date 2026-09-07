//! Audio peaks for the Edit tab's timeline, decoded once and cached.
//!
//! You cannot drag a keep-span out over audio you cannot see. The pane could ask the
//! webview to decode the chapter itself — WebAudio will do it — but that means shipping
//! a hundred-megabyte mp4 through a decode on every repaint, on the main thread, for a
//! picture that never changes once the take is recorded. So it happens here, once, and
//! lands beside the cut as `peaks.json`.
//!
//! # Why the mp3 and not the video
//!
//! Peaks are read from `chapter-NN.mp3`, the same file AssemblyAI transcribed. That
//! keeps the waveform, the word timings and the keep-list in *one* coordinate space —
//! if peaks came from a separately-written composed mp4 and its first audio buffer
//! landed a frame later, the waveform would sit slightly off the words and every hand
//! cut would inherit the error. It is also a 64kbit mono file rather than a 1080p one,
//! so the decode is close to free.

use std::path::Path;
use std::process::{Command, Stdio};

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};

pub const PEAKS_JSON: &str = "peaks.json";

/// Buckets per second. 100 is one peak per 10ms: finer than anyone can drag, and about
/// 30KB of JSON for a five-minute chapter.
pub const PEAKS_HZ: u32 = 100;

/// The rate ffmpeg is asked to resample to. Low on purpose — peak amplitude per 10ms
/// bucket is all the timeline draws, and 8kHz mono decodes in a fraction of the time
/// full-rate stereo would.
const DECODE_HZ: u32 = 8_000;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Peaks {
    pub hz: u32,
    /// Peak amplitude per bucket, 0–255. Bytes rather than floats because this is drawn,
    /// not measured, and a float array is four times the page weight for a picture
    /// nobody can see the difference in.
    pub values: Vec<u8>,
}

impl Peaks {
    pub fn is_empty(&self) -> bool {
        self.values.is_empty()
    }

    /// How long the decoded audio ran, from the bucket count. The authoritative
    /// duration for the timeline is ffprobe's — this is a sanity figure and the
    /// fallback when there is no video to probe.
    pub fn seconds(&self) -> f64 {
        if self.hz == 0 {
            return 0.0;
        }
        self.values.len() as f64 / self.hz as f64
    }
}

/// Peaks for `audio`, from `cache` when it is still current.
///
/// Freshness is the same rule the renderer uses, for the same reason: a cache that
/// outlives its input is worse than no cache, because it is invisible.
pub fn build(audio: &Path, cache: &Path) -> Result<Peaks> {
    if super::render::is_fresh(cache, std::slice::from_ref(&audio.to_path_buf())) {
        if let Some(peaks) = cached(cache) {
            return Ok(peaks);
        }
    }
    let peaks = decode(audio)?;
    if let Some(parent) = cache.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating {}", parent.display()))?;
    }
    let body = serde_json::to_string(&peaks).context("serializing peaks")?;
    std::fs::write(cache, body).with_context(|| format!("writing {}", cache.display()))?;
    Ok(peaks)
}

/// Whatever is already on disk, without decoding anything.
///
/// The rail needs a chapter's length for every row, and a chapter opened once has
/// already paid for its peaks — so this is the cheap answer before reaching for ffprobe.
pub(crate) fn cached(cache: &Path) -> Option<Peaks> {
    let text = std::fs::read_to_string(cache).ok()?;
    let peaks: Peaks = serde_json::from_str(&text).ok()?;
    (!peaks.is_empty() && peaks.hz > 0).then_some(peaks)
}

/// One ffmpeg pass, raw mono PCM on stdout.
fn decode(audio: &Path) -> Result<Peaks> {
    if !audio.is_file() {
        bail!("no audio at {}", audio.display());
    }
    let out = Command::new("ffmpeg")
        .args(["-v", "error", "-i"])
        .arg(audio)
        .args([
            "-vn",
            "-ac",
            "1",
            "-ar",
            &DECODE_HZ.to_string(),
            "-f",
            "s16le",
            "-",
        ])
        .stderr(Stdio::piped())
        .output()
        .with_context(|| format!("running ffmpeg to decode {}", audio.display()))?;
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr);
        bail!("ffmpeg could not decode {}: {}", audio.display(), err.trim());
    }
    if out.stdout.is_empty() {
        bail!("{} decoded to no audio at all", audio.display());
    }
    let samples: Vec<i16> = out
        .stdout
        .chunks_exact(2)
        .map(|pair| i16::from_le_bytes([pair[0], pair[1]]))
        .collect();
    Ok(Peaks {
        hz: PEAKS_HZ,
        values: peaks_from_pcm(&samples, (DECODE_HZ / PEAKS_HZ) as usize),
    })
}

/// Loudest sample in each bucket, scaled to 0–255.
///
/// Peak rather than RMS: this is drawn to be *clicked on*, and a transient — the start
/// of a word, a lip smack — is exactly the landmark someone aims at when they drag a
/// cut. RMS smooths those away.
pub(crate) fn peaks_from_pcm(samples: &[i16], per_bucket: usize) -> Vec<u8> {
    let per_bucket = per_bucket.max(1);
    samples
        .chunks(per_bucket)
        .map(|bucket| {
            let loudest = bucket
                .iter()
                // i16::MIN has no positive counterpart; saturating keeps it at MAX
                // rather than panicking in debug and wrapping to a silent -32768 in
                // release, which would draw the loudest moment in a take as silence.
                .map(|sample| sample.saturating_abs() as i32)
                .max()
                .unwrap_or(0);
            (loudest * 255 / i16::MAX as i32).clamp(0, 255) as u8
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp(tag: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "stream-recorder-peaks-{}-{tag}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn each_bucket_takes_its_loudest_sample() {
        let samples = [0, 100, i16::MAX, 50, -200, -300, 0, 0];
        let peaks = peaks_from_pcm(&samples, 4);
        assert_eq!(peaks.len(), 2);
        assert_eq!(peaks[0], 255, "the full-scale sample fills the bucket");
        assert_eq!(peaks[1], (300 * 255 / i16::MAX as i32) as u8);
    }

    #[test]
    fn silence_is_all_zeros() {
        assert_eq!(peaks_from_pcm(&[0; 320], 80), vec![0; 4]);
    }

    /// A take does not end on a bucket boundary, and the tail is still audio.
    #[test]
    fn a_partial_final_bucket_still_counts() {
        let peaks = peaks_from_pcm(&[0, 0, 0, 0, i16::MAX], 4);
        assert_eq!(peaks.len(), 2);
        assert_eq!(peaks[1], 255);
    }

    /// The loudest possible sample must not read as silence — see the note in
    /// `peaks_from_pcm`.
    #[test]
    fn the_most_negative_sample_is_loud_not_silent() {
        assert_eq!(peaks_from_pcm(&[i16::MIN], 1), vec![255]);
    }

    #[test]
    fn a_zero_bucket_size_is_treated_as_one_rather_than_dividing_by_zero() {
        assert_eq!(peaks_from_pcm(&[i16::MAX, 0], 0).len(), 2);
    }

    #[test]
    fn seconds_comes_from_the_bucket_count() {
        let peaks = Peaks {
            hz: 100,
            values: vec![0; 250],
        };
        assert!((peaks.seconds() - 2.5).abs() < f64::EPSILON);
        assert_eq!(Peaks { hz: 0, values: vec![1] }.seconds(), 0.0);
    }

    #[test]
    fn a_cache_newer_than_its_audio_is_reused_without_ffmpeg() {
        let dir = temp("cache");
        let audio = dir.join("chapter-01.mp3");
        let cache = dir.join(PEAKS_JSON);
        std::fs::write(&audio, b"not really an mp3").unwrap();
        std::thread::sleep(std::time::Duration::from_millis(20));
        std::fs::write(
            &cache,
            serde_json::to_string(&Peaks { hz: 100, values: vec![7, 8, 9] }).unwrap(),
        )
        .unwrap();
        // Unparseable as audio, so reaching ffmpeg at all would fail: proof of reuse.
        let peaks = build(&audio, &cache).unwrap();
        assert_eq!(peaks.values, vec![7, 8, 9]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// An empty cache is not a cache. Left trusted, a chapter that failed to decode
    /// once would show a blank timeline for good.
    #[test]
    fn an_empty_cache_is_not_trusted() {
        let dir = temp("hollow");
        let cache = dir.join(PEAKS_JSON);
        std::fs::write(
            &cache,
            serde_json::to_string(&Peaks { hz: 100, values: Vec::new() }).unwrap(),
        )
        .unwrap();
        assert!(cached(&cache).is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn missing_audio_says_so_rather_than_shelling_out() {
        let dir = temp("absent");
        let err = build(&dir.join("nope.mp3"), &dir.join(PEAKS_JSON))
            .unwrap_err()
            .to_string();
        assert!(err.contains("no audio at"), "{err}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The real thing, end to end on audio ffmpeg generates.
    ///
    /// Asserts the one property the timeline is built on: sound and silence are
    /// distinguishable, and in the right place. A tone followed by silence has to draw
    /// as a block and then nothing — if the halves came out swapped or smeared, every
    /// hand cut would be aimed at the wrong moment.
    #[test]
    fn a_tone_then_silence_decodes_to_loud_then_quiet_peaks() {
        if Command::new("ffmpeg").arg("-version").output().is_err() {
            return;
        }
        let dir = temp("tone");
        let audio = dir.join("tone.mp3");
        let status = Command::new("ffmpeg")
            .args([
                "-v", "error", "-y", "-f", "lavfi", "-i",
                "sine=frequency=440:duration=1",
                "-f", "lavfi", "-i", "anullsrc=duration=1:sample_rate=8000",
                "-filter_complex", "[0:a][1:a]concat=n=2:v=0:a=1",
            ])
            .arg(&audio)
            .status()
            .expect("ffmpeg generate");
        assert!(status.success());
        let peaks = build(&audio, &dir.join(PEAKS_JSON)).unwrap();
        assert_eq!(peaks.hz, PEAKS_HZ);
        // Two seconds at 100Hz buckets, give or take mp3 encoder padding.
        assert!(
            (180..=230).contains(&peaks.values.len()),
            "{} buckets",
            peaks.values.len()
        );
        // The tone is not silent, and it is level across itself — a sine has the same
        // peak in every bucket. (lavfi's `sine` is well below full scale, which is why
        // this checks presence and evenness rather than a threshold near 255.)
        let loud = &peaks.values[10..80];
        assert!(loud.iter().all(|&v| v > 0), "the tone reads as sound");
        let (lo, hi) = (
            *loud.iter().min().unwrap(),
            *loud.iter().max().unwrap(),
        );
        assert!(hi - lo <= 2, "a steady tone draws level: {lo}..{hi}");
        // And the second half is silence.
        let quiet = &peaks.values[120..180];
        assert!(
            quiet.iter().all(|&v| v == 0),
            "silence reads as silence: {:?}",
            &quiet[..8]
        );
        // The cache is on disk for the next repaint.
        assert!(dir.join(PEAKS_JSON).is_file());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
