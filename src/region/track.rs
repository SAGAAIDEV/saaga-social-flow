//! The sub-rect of the captured screen a frame shows while it is punched in.
//!
//! Everything else about mouse tracking — reading the pointer, reading the key,
//! smoothing the result — produces three numbers: where the pointer is, how far
//! punched in the frame should be, and where it may travel. This turns those
//! into a rect, and it is the only place that is allowed to.
//!
//! ## Two motions, one interpolation
//!
//! A punched-in frame does two things at once: it *tightens*, and it *moves* to
//! where the pointer is — which usually means leaving the region it was framed
//! in, because the whole point is to show something the resting frame does not
//! cover. Those look like separate features and are not worth implementing as
//! two: the frame at rest and the frame fully punched in are just two rects,
//! and every state between them is one interpolation.
//!
//! That formulation buys three things for free. The aspect is exact, because
//! both endpoints carry the layout slot's aspect and interpolating two rects of
//! equal aspect preserves it. Containment is automatic, because each edge of
//! the result is an interpolation of two edges that are already inside the
//! envelope. And releasing the key is not a special case — the punch eases to
//! zero and the frame lands back on the authored rect exactly, having zoomed
//! out and travelled home as one move.
//!
//! ## Why the travel envelope is the capture and not the region
//!
//! `rest` fixes the frame's size and aspect. What bounds its *travel* is the
//! captured buffer, which on Split is the union of both authored regions — and
//! in the usual framing, where the vertical sits inside the horizontal, that
//! union simply *is* the horizontal. So "the vertical box slides left and right
//! within the horizontal one" is what falls out, without the horizontal being
//! named anywhere in here.
//!
//! It is also the reason this is safe to do per frame. `App::union_capture`
//! sizes the whole `SCStream` from those authored rects, and re-aiming the
//! stream blocks the calling thread on a ten-second completion handler and
//! resizes an `AVAssetWriterInput` that has already locked its dimensions.
//! Travelling *inside* the buffer touches none of it: one crop rect changes, on
//! the GPU, and nothing upstream notices. A frame allowed to travel outside the
//! capture would have to move the capture, and this feature would stop being
//! free.
//!
//! ## The floor, and the display where there is no room
//!
//! The punch-in stops when one buffer pixel is one output pixel. Below that it
//! would be upscaling a screenshot of text, which is the one thing a screencast
//! cannot afford, and the stop is exact rather than tuned: `floor` is the
//! layout's own slot size, which is the same number
//! [`base_size`](super::placement::base_size) derives `rest` from.
//!
//! On a display too small to give the region its 1:1 size, `resolve` has
//! already shrunk `rest` below that floor — a 1× 1080p display cannot fit the
//! vertical region's 1280 points of height — so the floor is clamped to `rest`
//! before it is applied and the punch degrades to "no tightening". The travel
//! still works there; only the zoom does not.

/// A rect in the captured buffer: `(x, y, w, h)`, top-left origin, buffer
/// pixels. The same shape `Composite` hands to `crop_rect_into_slot`.
pub type BufferRect = (f64, f64, f64, f64);

/// How much punch counts as none.
///
/// The ramp that drives this is exponential and approaches its ends
/// asymptotically, so "released" is never exactly zero. Without a floor the
/// frame would sit a fraction of a pixel off its resting rect forever, the
/// overlay would draw its punched-in outline permanently, and the "switching
/// tracking on changes nothing" property would quietly stop being true.
const SETTLED: f64 = 1e-3;

/// The sub-rect of the capture this frame shows.
///
/// - `rest` is the operator's authored region: the frame's size and aspect at
///   full width, and exactly what it returns to.
/// - `bounds` is where the frame may travel — the captured buffer.
/// - `pointer` is where the pointer is, in the same space as `rest` and
///   `bounds`. Not normalized, because the two callers work in different units
///   (buffer pixels and display points) and a normalized argument would need
///   two different denominators to mean the same thing.
/// - `punch` is how far in, normalized to the room actually available: `0.0`
///   is the authored rect, `1.0` is as tight as the floor allows and as far
///   across as `bounds` permits.
/// - `floor` is the smallest the frame may get, in the same units again.
///
/// Returns `rest` unchanged for any input it cannot make sense of, rather than
/// a rect that is merely arithmetically defensible. This runs per frame on a
/// capture queue, where the cost of a bad answer is a ruined take and the cost
/// of no answer is the framing the operator chose.
pub fn tracked_crop(
    rest: BufferRect,
    bounds: BufferRect,
    pointer: (f64, f64),
    punch: f64,
    floor: (f64, f64),
) -> BufferRect {
    let (x, y, w, h) = rest;
    // Comparisons rather than `<= 0.0`, so a NaN side reads as no area too.
    let has_area = w > 0.0 && h > 0.0;
    if !has_area || !x.is_finite() || !y.is_finite() {
        return rest;
    }
    if !pointer.0.is_finite() || !pointer.1.is_finite() || !punch.is_finite() {
        return rest;
    }
    let punch = punch.clamp(0.0, 1.0);
    if punch < SETTLED {
        return rest;
    }

    // The tightest single factor that still leaves `floor` on both axes.
    // `min(1.0)` handles a `rest` already smaller than the slot: there is no
    // room to tighten, so the punched frame is the same size as the resting
    // one and only its position moves.
    let tightest = (floor.0 / w).max(floor.1 / h).min(1.0);
    let tight = (w * tightest, h * tightest);

    // The envelope is the capture *unioned with* the authored rect. Normally
    // the region is inside the capture and this is just the capture; taking
    // the union anyway guarantees both ends of the interpolation are inside
    // it, which is what makes the containment argument hold rather than
    // usually hold.
    let envelope = union(bounds, rest);

    // Fully punched in: centred on the pointer, held inside the envelope.
    // Clamping the origin rather than the centre is what makes a pointer at
    // the very edge show the edge, instead of half a frame of nothing.
    let target = (
        clamp_span(pointer.0 - tight.0 / 2.0, envelope.0, envelope.2 - tight.0),
        clamp_span(pointer.1 - tight.1 / 2.0, envelope.1, envelope.3 - tight.1),
    );

    let lerp = |from: f64, to: f64| from + (to - from) * punch;
    let size = (lerp(w, tight.0), lerp(h, tight.1));
    let origin = (lerp(x, target.0), lerp(y, target.1));

    // Snap to whole pixels, then hold it inside the envelope in case rounding
    // pushed it off the end.
    //
    // The origin only. Rounding the *size* too would be the obvious tidy-up
    // and it is wrong: it breaks the exact aspect the shared-factor derivation
    // guarantees, and it buys nothing, because a fractional size is a constant
    // resample for a given punch while a fractional origin is a *different*
    // subpixel phase every frame. That per-frame phase change is what makes
    // text crawl while the frame moves, and it is the only part worth snapping.
    let snapped = (
        clamp_span(origin.0.round(), envelope.0, envelope.2 - size.0),
        clamp_span(origin.1.round(), envelope.1, envelope.3 - size.1),
    );

    (snapped.0, snapped.1, size.0, size.1)
}

/// `want`, held to `[low, low + span]`. A negative span — an envelope smaller
/// than the frame — pins to `low` rather than panicking inside `clamp`.
fn clamp_span(want: f64, low: f64, span: f64) -> f64 {
    want.clamp(low, low + span.max(0.0))
}

/// The smallest rect containing both.
fn union(a: BufferRect, b: BufferRect) -> BufferRect {
    let x = a.0.min(b.0);
    let y = a.1.min(b.1);
    let right = (a.0 + a.2).max(b.0 + b.2);
    let bottom = (a.1 + a.3).max(b.1 + b.3);
    (x, y, (right - x).max(0.0), (bottom - y).max(0.0))
}

#[cfg(test)]
#[path = "track_tests.rs"]
mod tests;
