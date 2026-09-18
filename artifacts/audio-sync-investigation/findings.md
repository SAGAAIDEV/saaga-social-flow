# Audio sync investigation

Investigated the most recently rendered local project, `2026-09-16_03-04-35`, take `v1`. The affected output confirmed here is `render/v1/horizontal/longform.mp4`. No application code, recordings, or existing renders were modified.

## Confirmed cause

The horizontal assembly stream-copies chapters with different video time bases. Chapter 2 uses `1/19200`; the other cut chapters and conformed title cards use `1/12800`. Audio consistently uses `1/48000`.

`src/edit/render.rs:216` conforms title cards to the first chapter but passes every footage chapter through without checking its time base. `src/edit/cut.rs:269` concatenates those inputs with `-c copy`. FFmpeg requires the same time base across concat inputs: https://ffmpeg.org/ffmpeg-formats.html#concat-1

Chapter 2's video timestamp span is multiplied by 19200/12800 = 1.5, while its audio retains normal timing. Following video timestamps then overlap the stretched chapter, and FFmpeg repairs them into nearly identical timestamps. Matching the actual encoded packet hashes between the cut chapters and longform proves this is introduced by assembly, rather than inferred from stream durations.

| Stream | Source timestamp | Timestamp in existing longform |
| --- | ---: | ---: |
| Chapter 2 video | 0.026667 s | 11.812031 s |
| Chapter 2 video | 49.166667 s | 85.522031 s |
| Chapter 2 video | 98.333333 s | 159.272031 s |
| Chapter 2 audio | 49.152000 s | 57.021333 s |
| Chapter 4 video | 0.160000 s | 159.268047 s |
| Chapter 4 video | 45.560000 s | 159.357031 s |

Thus chapter 2 accumulates approximately 49 seconds of additional video delay, and almost the entirety of chapter 4's video is compressed into approximately 0.09 seconds of presentation timestamps. Checking only the total file duration or stream start times misses this defect.

## Why chapter 2 differs

The original recording reports `r_frame_rate=150/1`, but `avg_frame_rate=583800/23353` (approximately 25 fps), with 3,892 frames over 155.686667 seconds. `probe_fps` in `src/edit/cut.rs` reads only `r_frame_rate`, and `cut_segment` uses that value in the `fps` filter. The resulting chapter is encoded at 150 fps and acquires a different video time base. A reported base frame rate is not a reliable choice of delivery frame rate for this recording.

## Repair experiment

Created temporary copies only. Remuxed the full chapter 2 cut using:

```sh
ffmpeg -i chapter-02-horizontal.mp4 -map 0 -c copy -video_track_timescale 12800 chapter-02-timescale.mp4
```

Then assembled the first three retained chapters (1, 2, and 4) with their existing conformed title cards. All encoded video and audio were copied, not re-encoded. Packet hashes were compared against the original cut inputs; repeated hashes were excluded to avoid confusing identical static frames or silence.

| Chapter | Video offset after join | Audio offset after join | Maximum variation in video offset within chapter |
| --- | ---: | ---: | ---: |
| 1 | 0.021016 s | 0.021333 s | 0 s |
| 2 | 7.868958–7.869011 s | 7.869333–7.869354 s | 0.000053 s |
| 4 | 109.223672 s | 109.223979–109.224000 s | 0 s |

This eliminates the large assembly-induced mismatch: audio and video offsets agree to within 0.4 ms. It does not prove perceptual lip sync in the original recording. A few boundary DTS warnings remain due to AAC priming and different video reorder delays; a production fix should address or explicitly validate boundary behavior as well.

Temporary preview:
`/var/folders/33/gmytc3cd4jv67wdgx63jm8dh0000gn/T/saaga-sync-investigation-e94copyw/repaired-first-three-chapters.mp4`

## Recommended implementation

1. Normalize/check every chapter and card before concatenation, including video time base. Remuxing incompatible time bases can preserve existing encoded footage; explicit encoder settings prevent future mismatches.
2. Select a deliberate delivery frame rate instead of blindly treating `r_frame_rate` as the recording cadence. Validate against recorded frame-rate metadata and actual timestamps.
3. Include the normalization policy/version in assembly cache inputs. Currently an unchanged part list and source mtimes can leave an old broken longform marked current after a code fix.
4. Add an FFmpeg integration regression with mixed time bases that checks per-chapter audio/video timestamp offsets and monotonic boundary behavior, rather than merely output existence or total duration.

## Scope

The confirmed defect is in horizontal longform assembly. The latest vertical chapters all render at 30 fps with a common `1/15360` video time base, so this particular mismatch does not explain vertical sync complaints. Vertical perceptual sync and original capture sync have not been established by this investigation. The user has not yet identified a specific affected output; the latest local render was selected for inspection.
