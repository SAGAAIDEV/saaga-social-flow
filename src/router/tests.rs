//! Router tests: chapter numbering and filenames as plain units, plus the
//! `#[ignore]` hardware flows that are the only way to prove a chapter's
//! files are actually encoded, aligned and cropped as configured.
use super::*;
use std::fs;

#[test]
fn test_chapter_numbering() {
    // Simulate the chapter numbering logic without a real router.
    let mut chapter = 1u32;

    // Cut should increment chapter.
    chapter = chapter + 1;
    assert_eq!(chapter, 2);

    // Cut again.
    chapter = chapter + 1;
    assert_eq!(chapter, 3);

    // Retake should NOT increment chapter (stays at 3).
    // (no change to chapter variable)
    assert_eq!(chapter, 3);

    // Cut should increment from 3 to 4.
    chapter = chapter + 1;
    assert_eq!(chapter, 4);
}

/// Full hardware integration test of the chapter flow — the exact path
/// the ⌃⌥C hotkey drives, minus the hotkey. Needs a saved camera+mic in
/// ~/.stream-recorder/config.json, real devices, and ffprobe; run it
/// explicitly with `cargo test -- --ignored chapter_flow`.
///
/// Exists because of a shipped bug this would have caught: chapter
/// writers built with `None` encoder settings (passthrough) produced
/// static instead of audio. The codec assertions below fail on that.
#[test]
#[ignore]
fn chapter_flow_produces_encoded_streams() {
    let cfg = crate::config::load();
    let (camera_uid, audio_uid) = match (cfg.camera_device_uid, cfg.audio_device_uid) {
        (Some(c), Some(a)) => (c, a),
        _ => panic!("needs saved camera+mic defaults; run the record command once first"),
    };

    let session_dir =
        std::env::temp_dir().join(format!("stream-recorder-test-{}", std::process::id()));
    fs::create_dir_all(&session_dir).unwrap();

    let connection = crate::capture::av::Connection::start_capture(&camera_uid, &audio_uid)
        .expect("start_capture");
    connection
        .wait_for_warmup(std::time::Duration::from_secs(5))
        .expect("warmup");
    // The app opens the first chapter from a button press, seconds after
    // warmup; stand in for that delay here so the test records against a
    // settled device the way a real session does.
    std::thread::sleep(std::time::Duration::from_secs(2));
    let mut router = Router::start_at(
        &session_dir,
        &connection.delegate,
        connection.video_settings.clone(),
        connection.audio_settings.clone(),
        connection.sync_clock().expect("session clock"),
        None,
        crate::layouts::Pair::TalkingHead,
        1,
    )
    .expect("router start");

    std::thread::sleep(std::time::Duration::from_secs(2));
    router.cut_chapter().expect("cut chapter");
    std::thread::sleep(std::time::Duration::from_secs(1));
    // Retake chapter 2: the discarded take must land in .discarded/ and
    // the same-numbered replacement must start cleanly (the first retake
    // implementation hit AVError -11823 by creating the new writer while
    // the old file still occupied the path).
    router.retake_chapter().expect("retake chapter");
    std::thread::sleep(std::time::Duration::from_secs(2));
    unsafe { connection.session.stopRunning() };
    router.stop().expect("stop");

    let discarded: Vec<_> = fs::read_dir(session_dir.join(".discarded"))
        .expect(".discarded exists after retake")
        .collect();
    assert_eq!(discarded.len(), 1, "exactly one discarded take");

    for n in 1..=2 {
        let path = session_dir.join(format!("chapter-{n:02}.mp4"));
        let out = std::process::Command::new("ffprobe")
            .args(["-v", "error", "-show_entries", "stream=codec_name"])
            .args(["-of", "csv=p=0"])
            .arg(&path)
            .output()
            .expect("ffprobe runs");
        let codecs = String::from_utf8_lossy(&out.stdout);
        assert!(
            codecs.contains("h264"),
            "chapter {n} missing encoded video, got: {codecs}"
        );
        assert!(
            codecs.contains("aac"),
            "chapter {n} missing encoded audio (passthrough bug?), got: {codecs}"
        );

        // Content check, not just codec check: static/white noise decodes
        // as valid AAC but shows a zero-crossing rate near half the sample
        // rate (~14k/s at 48kHz observed in the field), while room tone /
        // speech sits well under 8k/s. Catches garbage LPCM fed into a
        // healthy encoder — which codec assertions alone cannot.
        let astats = std::process::Command::new("ffmpeg")
            .args(["-i"])
            .arg(&path)
            .args(["-map", "0:a", "-af", "astats", "-f", "null", "-"])
            .output()
            .expect("ffmpeg astats runs");
        let stderr = String::from_utf8_lossy(&astats.stderr);
        let crossings: f64 = stderr
            .lines()
            .find_map(|l| l.split("Zero crossings:").nth(1))
            .expect("astats reports zero crossings")
            .trim()
            .parse()
            .expect("zero crossings parses");
        let duration: f64 = {
            let out = std::process::Command::new("ffprobe")
                .args(["-v", "error", "-show_entries", "format=duration"])
                .args(["-of", "csv=p=0"])
                .arg(&path)
                .output()
                .expect("ffprobe duration");
            String::from_utf8_lossy(&out.stdout)
                .trim()
                .parse()
                .expect("duration parses")
        };
        let zcr = crossings / duration;
        assert!(
            zcr < 8000.0,
            "chapter {n} audio looks like static: {zcr:.0} zero crossings/sec (files kept in {})",
            session_dir.display()
        );
        assert!(
            session_dir.join(format!("chapter-{n:02}.m4a")).exists(),
            "chapter {n} m4a missing"
        );
        assert!(
            session_dir.join(format!("chapter-{n:02}.mp3")).exists(),
            "chapter {n} mp3 missing"
        );
    }

    fs::remove_dir_all(&session_dir).ok();
}

/// The two-file path end to end: camera+mic and screen cut together,
/// then checked for the thing the whole anchor design exists to give —
/// a chapter's two files starting at the same instant.
///
/// Runs with a **region set**, not on the whole display, so one test covers
/// both halves of the claim: that a cropped capture reaches the file at the
/// layout's dimensions, and that cropping does not disturb the anchor the
/// two files share.
///
/// Needs saved devices, a real display, screen-recording permission for
/// the test binary, and ffprobe.
/// `cargo test -- --ignored --nocapture chapter_flow_with_screen`
#[test]
#[ignore]
fn chapter_flow_with_screen_produces_aligned_pairs() {
    use crate::capture::screen_stream::ScreenConnection;
    use crate::capture::screen_writer::video_settings;

    let cfg = crate::config::load();
    let (camera_uid, audio_uid) = match (cfg.camera_device_uid, cfg.audio_device_uid) {
        (Some(c), Some(a)) => (c, a),
        _ => panic!("needs saved camera+mic defaults; run the record command once first"),
    };
    let display_uid = crate::capture::screen::list_displays()
        .expect("displays list")
        .first()
        .expect("at least one display")
        .uid
        .clone();

    let session_dir =
        std::env::temp_dir().join(format!("stream-recorder-screen-{}", std::process::id()));
    fs::create_dir_all(&session_dir).unwrap();

    let connection = crate::capture::av::Connection::start_capture(&camera_uid, &audio_uid)
        .expect("start_capture");
    connection
        .wait_for_warmup(std::time::Duration::from_secs(5))
        .expect("warmup");
    // Frame it for Split-Horizontal: a 1.299:1 region, not the display.
    let layout = crate::layouts::Layout::get(
        crate::layouts::Pair::Split,
        crate::layouts::Orientation::Horizontal,
    );
    let geometry =
        crate::capture::screen::display_geometry(&display_uid).expect("display geometry");
    let slot = layout
        .slot_size()
        .expect("split-horizontal has a screen slot");
    let output = crate::region::PixelSize::rounded(slot.0, slot.1);
    let base = crate::region::placement::base_size(output, &geometry);
    let resolved = crate::region::placement::resolve(
        base,
        crate::region::placement::Placement {
            offset: (
                (geometry.points.0 - base.0).max(0.0) / 2.0,
                (geometry.points.1 - base.1).max(0.0) / 2.0,
            ),
            zoom: 1.0,
        },
        None,
        &geometry,
    );
    let capture = crate::capture::screen_stream::Capture {
        region: resolved.rect,
        output,
    };
    let screen =
        ScreenConnection::start_capture(&display_uid, Some(capture)).expect("screen capture");
    println!(
        "screen region {:?} on a {:?}pt / {:?}px display -> {}×{}",
        resolved.rect, geometry.points, geometry.pixels, screen.width, screen.height,
    );
    // An idle display may deliver nothing; the stream is still live.
    let _ = screen.wait_for_warmup(std::time::Duration::from_secs(5));

    let track = ScreenTrack {
        state: screen.delegate.state_arc(),
        delegate: screen.delegate.clone(),
        settings: video_settings(screen.width, screen.height).expect("screen settings"),
        clock: screen.sync_clock().expect("screen clock"),
        width: screen.width,
        height: screen.height,
    };
    let mut router = Router::start_at(
        &session_dir,
        &connection.delegate,
        connection.video_settings.clone(),
        connection.audio_settings.clone(),
        connection.sync_clock().expect("session clock"),
        Some(track),
        crate::layouts::Pair::Split,
        1,
    )
    .expect("router start");

    std::thread::sleep(std::time::Duration::from_secs(3));
    router.cut_chapter().expect("cut chapter");
    std::thread::sleep(std::time::Duration::from_secs(3));
    screen.stop().expect("stop screen");
    unsafe { connection.session.stopRunning() };
    router.stop().expect("stop");

    for n in 1..=2 {
        let camera_path = session_dir.join(format!("chapter-{n:02}.mp4"));
        let screen_path = session_dir.join(format!("chapter-{n:02}-screen.mp4"));
        assert!(screen_path.exists(), "chapter {n} screen file missing");

        let codecs = ffprobe(&screen_path, "stream=codec_name");
        assert!(
            codecs.contains("h264"),
            "chapter {n} screen video is not encoded (passthrough bug?), got: {codecs}"
        );

        // The crop reached the file. Not the display's size, and not the
        // slot's nominal size either — the region's, which is the slot's
        // aspect at whatever the display could give at 1:1.
        let dimensions = ffprobe(&screen_path, "stream=width,height");
        assert!(
            dimensions.contains(&screen.width.to_string())
                && dimensions.contains(&screen.height.to_string()),
            "chapter {n} screen file is {dimensions}, not the {}×{} region that was \
             configured — sourceRect did not reach the encoder",
            screen.width,
            screen.height,
        );
        let aspect = screen.width as f64 / screen.height as f64;
        let want = slot.0 / slot.1;
        assert!(
            (aspect - want).abs() < 1e-2,
            "chapter {n} screen aspect {aspect:.4} does not match {}'s slot {want:.4} — \
             object-fit: cover would trim the edges in the render",
            layout.block,
        );

        // The alignment check. Both writers were anchored to the same
        // instant, so the first frame of each file should sit at nearly
        // the same offset from its own t=0. A large gap here means the
        // anchors were computed on mismatched clocks.
        let camera_start: f64 = ffprobe(&camera_path, "stream=start_time")
            .lines()
            .next()
            .and_then(|l| l.trim().parse().ok())
            .expect("camera start_time");
        let screen_start: f64 = ffprobe(&screen_path, "stream=start_time")
            .lines()
            .next()
            .and_then(|l| l.trim().parse().ok())
            .expect("screen start_time");
        let skew = (camera_start - screen_start).abs();
        println!(
            "chapter {n}: camera starts {camera_start:.4}s, screen {screen_start:.4}s, \
             skew {:.1} ms",
            skew * 1000.0
        );
        assert!(
            skew < 0.5,
            "chapter {n} files are {:.0} ms apart — the shared anchor is not working \
             (files kept in {})",
            skew * 1000.0,
            session_dir.display()
        );
    }

    fs::remove_dir_all(&session_dir).ok();
}

/// Pins `sourceRect`'s coordinate convention against a real display, by
/// recording the left half and then the right half of the same screen.
///
/// This is the one claim in the region path that the headers state only as
/// "the display's logical coordinate system" — everything else is
/// arithmetic and is covered without hardware in [`crate::region`]. What
/// the assertions can pin is that the crop is applied at all and that
/// moving its origin does not change its size. **Which half is which is a
/// human check**: no assertion can tell left from right without knowing
/// what was on the screen, so the test prints both paths and says which is
/// which, and a flipped or mirrored convention shows up the moment they are
/// opened. Saying that out loud beats an assertion that looks stronger than
/// it is.
///
/// Files are deliberately **not** cleaned up, for that reason.
///
/// `cargo test -- --ignored --nocapture region_reaches_the_file`
#[test]
#[ignore]
fn region_reaches_the_file_at_the_configured_origin() {
    use crate::capture::screen_stream::Capture;
    use crate::capture::screen_stream::ScreenConnection;
    use crate::capture::screen_writer::video_settings;
    use crate::region::PointRect;

    let cfg = crate::config::load();
    let (camera_uid, audio_uid) = match (cfg.camera_device_uid, cfg.audio_device_uid) {
        (Some(c), Some(a)) => (c, a),
        _ => panic!("needs saved camera+mic defaults; run the record command once first"),
    };
    let display_uid = crate::capture::screen::list_displays()
        .expect("displays list")
        .first()
        .expect("at least one display")
        .uid
        .clone();
    let geometry =
        crate::capture::screen::display_geometry(&display_uid).expect("display geometry");

    let session_dir =
        std::env::temp_dir().join(format!("stream-recorder-region-{}", std::process::id()));
    fs::create_dir_all(&session_dir).unwrap();

    let half = PointRect {
        x: 0.0,
        y: 0.0,
        w: geometry.points.0 / 2.0,
        h: geometry.points.1,
    };
    let right = PointRect {
        x: geometry.points.0 / 2.0,
        ..half
    };

    let connection = crate::capture::av::Connection::start_capture(&camera_uid, &audio_uid)
        .expect("start_capture");
    connection
        .wait_for_warmup(std::time::Duration::from_secs(5))
        .expect("warmup");
    // Output pinned to the region's own 1:1 pixel count here, not to a layout
    // slot: this test is about where sourceRect lands, so anything that
    // resampled would blur the very edge it is checking.
    let expected = half.pixels(geometry.scale());
    let screen = ScreenConnection::start_capture(
        &display_uid,
        Some(Capture {
            region: half,
            output: expected,
        }),
    )
    .expect("screen capture");
    let _ = screen.wait_for_warmup(std::time::Duration::from_secs(5));

    assert_eq!(
        (screen.width, screen.height),
        (expected.w, expected.h),
        "the left-half region was not configured at its derived pixel size",
    );

    let track = ScreenTrack {
        state: screen.delegate.state_arc(),
        delegate: screen.delegate.clone(),
        settings: video_settings(screen.width, screen.height).expect("screen settings"),
        clock: screen.sync_clock().expect("screen clock"),
        width: screen.width,
        height: screen.height,
    };
    let mut router = Router::start_at(
        &session_dir,
        &connection.delegate,
        connection.video_settings.clone(),
        connection.audio_settings.clone(),
        connection.sync_clock().expect("session clock"),
        Some(track),
        crate::layouts::Pair::Split,
        1,
    )
    .expect("router start");

    std::thread::sleep(std::time::Duration::from_secs(3));
    screen.stop().expect("stop screen");
    unsafe { connection.session.stopRunning() };
    router.stop().expect("stop");

    let path = session_dir.join("chapter-01-screen.mp4");
    let dimensions = ffprobe(&path, "stream=width,height");
    assert!(
        dimensions.contains(&expected.w.to_string()),
        "the file is {dimensions}, not the {}×{} left half — sourceRect did not reach \
         the encoder",
        expected.w,
        expected.h,
    );

    // Same size at a different origin: proves the crop moves rather than
    // just being a resize in disguise.
    let moved = ScreenConnection::start_capture(
        &display_uid,
        Some(Capture {
            region: right,
            output: expected,
        }),
    )
    .expect("right-half capture");
    assert_eq!(
        (moved.width, moved.height),
        (screen.width, screen.height),
        "moving the region changed its size",
    );
    moved.stop().ok();

    println!(
        "\nOPEN THIS AND LOOK: {}\n  It must show the LEFT half of the display — \
         {:.0}×{:.0} points at display-local origin (0, 0), captured as {}×{} pixels.\n  \
         The right half, or a vertically mirrored image, means sourceRect's origin is not \
         top-left display-local and `PointRect` needs a flip.\n",
        path.display(),
        half.w,
        half.h,
        expected.w,
        expected.h,
    );
}

fn ffprobe(path: &Path, entries: &str) -> String {
    let out = std::process::Command::new("ffprobe")
        .args(["-v", "error", "-select_streams", "v:0"])
        .args(["-show_entries", entries, "-of", "csv=p=0"])
        .arg(path)
        .output()
        .expect("ffprobe runs");
    String::from_utf8_lossy(&out.stdout).to_string()
}

#[test]
fn test_chapter_filename_format() {
    // Verify chapter filename format.
    for ch_num in 1..=10 {
        let filename = format!("chapter-{:02}.mp4", ch_num);
        match ch_num {
            1 => assert_eq!(filename, "chapter-01.mp4"),
            9 => assert_eq!(filename, "chapter-09.mp4"),
            10 => assert_eq!(filename, "chapter-10.mp4"),
            _ => {}
        }
    }
}
