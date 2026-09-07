# Vendored models

## `blaze_face_short_range.tflite`

MediaPipe's short-range face detector (BlazeFace), fetched once from Google's
model store and checked in rather than downloaded at runtime:

    https://storage.googleapis.com/mediapipe-models/face_detector/blaze_face_short_range/float16/1/blaze_face_short_range.tflite
    sha256 b4578f35940bf5a1a655214a1cce5cab13eba73c1297cd78e1a04c2380b0152f

224 KB, and `include_bytes!`d into the binary by `src/face/detect.rs`, so live
face tracking has no model path to configure, no file to lose beside the app,
and no network call on the path that a recording depends on. "Short range" is
the right half of the pair: it is trained for faces within about two metres of
the lens, which is every talking head this recorder has ever captured. The
full-range model is for crowds at a distance and is both larger and slower.

The *runtime* is a different matter and deliberately not vendored — the
`mediapipe` crate `dlopen`s a ~34 MB `libmediapipe.dylib` and fetches it into
`~/.cache/mediapipe-rs/` on first use. See `src/face/mod.rs` for why that
download is forced to happen off the capture queue.
