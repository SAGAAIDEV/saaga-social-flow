//! One preview slot per orientation. Each letterboxes its own aspect inside
//! a dedicated split pane so resize cannot couple 16:9 to 9:16.

use std::cell::{Cell, RefCell};
use std::ptr::NonNull;
use std::sync::Arc;

use objc2::rc::Retained;
use objc2::runtime::ProtocolObject;
use objc2::{MainThreadMarker, MainThreadOnly};
use objc2_app_kit::{
    NSAutoresizingMaskOptions, NSButton, NSPopUpButton, NSSplitView, NSSplitViewDividerStyle,
    NSTextField, NSView,
};
use objc2_av_foundation::{
    AVLayerVideoGravityResizeAspect, AVQueuedSampleBufferRendering, AVSampleBufferDisplayLayer,
};
use objc2_core_foundation::CFRetained;
use objc2_core_media::{
    kCMSampleAttachmentKey_DisplayImmediately, kCMTimeInvalid, CMSampleBuffer, CMSampleTimingInfo,
    CMVideoFormatDescription, CMVideoFormatDescriptionCreateForImageBuffer,
};
use objc2_core_video::CVImageBuffer;
use objc2_foundation::{NSMutableDictionary, NSNumber, NSPoint, NSRect, NSSize, NSString};
use objc2_quartz_core::{CAAutoresizingMask, CALayer};

use crate::ops::PreviewPort;

const PAD: f64 = 8.0;
const LABEL_H: f64 = 16.0;
const GAP: f64 = 4.0;

const BAR_H: f64 = 26.0;
const LABEL_W: f64 = 52.0;
/// Wide enough for "Track Face — loading…", which is the longest state the
/// switch shows. Sizing to the short title would make the bar reflow the moment
/// a build starts, which reads as a glitch rather than as progress.
const FACE_W: f64 = 150.0;
/// "Track Mouse" has no loading state to name, so it sizes to its own title.
const MOUSE_W: f64 = 110.0;

pub struct PreviewHost {
    wrap: Retained<NSView>,
    layout_label: Retained<NSTextField>,
    layout_popup: Retained<NSPopUpButton>,
    /// Beside the layout popup: all three decide what the recorded frame
    /// contains.
    face_checkbox: Retained<NSButton>,
    mouse_checkbox: Retained<NSButton>,
    split: Retained<NSSplitView>,
    horizontal: PreviewSlot,
    vertical: PreviewSlot,
    /// The raw camera port. No layer, no layout — capture only.
    camera: RefCell<Option<Arc<PreviewPort>>>,
}

struct PreviewSlot {
    name: &'static str,
    aspect: f64,
    pane: Retained<NSView>,
    label: Retained<NSTextField>,
    host: Retained<NSView>,
    layer: Retained<AVSampleBufferDisplayLayer>,
    port: RefCell<Option<Arc<PreviewPort>>>,
    last_pts_value: Cell<i64>,
    last_pts_timescale: Cell<i32>,
}

impl PreviewHost {
    pub fn attach(
        parent: &NSView,
        mtm: MainThreadMarker,
        layout_label: Retained<NSTextField>,
        layout_popup: Retained<NSPopUpButton>,
        face_checkbox: Retained<NSButton>,
        mouse_checkbox: Retained<NSButton>,
    ) -> PreviewHost {
        let wrap = NSView::initWithFrame(
            NSView::alloc(mtm),
            NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(100.0, 100.0)),
        );
        wrap.addSubview(&layout_label);
        wrap.addSubview(&layout_popup);
        wrap.addSubview(&face_checkbox);
        wrap.addSubview(&mouse_checkbox);

        let split = NSSplitView::initWithFrame(
            NSSplitView::alloc(mtm),
            NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(100.0, 80.0)),
        );
        split.setVertical(true);
        split.setDividerStyle(NSSplitViewDividerStyle::Thin);
        split.setAutosaveName(Some(&NSString::from_str("stream-recorder-preview-split")));

        let horizontal = PreviewSlot::attach(mtm, "horizontal", "Horizontal", 16.0 / 9.0);
        let vertical = PreviewSlot::attach(mtm, "vertical", "Vertical", 9.0 / 16.0);
        split.addSubview(&horizontal.pane);
        split.addSubview(&vertical.pane);
        wrap.addSubview(&split);
        parent.addSubview(&wrap);

        PreviewHost {
            wrap,
            layout_label,
            layout_popup,
            face_checkbox,
            mouse_checkbox,
            split,
            horizontal,
            vertical,
            camera: RefCell::new(None),
        }
    }

    pub fn set_frame(&self, frame: NSRect) {
        self.wrap.setFrame(frame);
        let w = frame.size.width;
        let h = frame.size.height;
        self.layout_label.setFrame(NSRect::new(
            NSPoint::new(0.0, (h - BAR_H).max(0.0)),
            NSSize::new(LABEL_W, BAR_H),
        ));
        // The popup takes what the switches leave, floored so a very narrow
        // window shrinks the popup rather than pushing a switch off the
        // edge — both have to stay reachable at any width.
        let popup_w = (w - LABEL_W - GAP - FACE_W - GAP - MOUSE_W - GAP).max(80.0);
        self.layout_popup.setFrame(NSRect::new(
            NSPoint::new(LABEL_W + GAP, (h - BAR_H).max(0.0)),
            NSSize::new(popup_w, BAR_H),
        ));
        let face_x = LABEL_W + GAP + popup_w + GAP;
        self.face_checkbox.setFrame(NSRect::new(
            NSPoint::new(face_x, (h - BAR_H).max(0.0)),
            NSSize::new(FACE_W, BAR_H),
        ));
        self.mouse_checkbox.setFrame(NSRect::new(
            NSPoint::new(face_x + FACE_W + GAP, (h - BAR_H).max(0.0)),
            NSSize::new(MOUSE_W, BAR_H),
        ));
        let split_h = (h - BAR_H - GAP).max(40.0);
        self.split
            .setFrame(NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(w, split_h)));
        self.horizontal.fit();
        self.vertical.fit();
    }

    pub fn bind(&self, ports: &[Arc<PreviewPort>]) {
        self.horizontal.bind(ports);
        self.vertical.bind(ports);
        // Held but never displayed — it exists so a thumbnail still can be the
        // camera alone. See `ops::graphs::CAMERA_PREVIEW`.
        *self.camera.borrow_mut() = ports
            .iter()
            .find(|port| port.spec().name() == crate::ops::graphs::CAMERA_PREVIEW)
            .cloned();
    }

    /// The newest **camera-only** frame, for a still capture.
    ///
    /// Deliberately not the horizontal slot: that is a composite in any layout
    /// with a screen slot, so a thumbnail taken from it would have the screen
    /// share baked in. This port hangs off the graph's source node.
    pub fn latest_camera_frame(&self) -> Option<crate::ops::preview::PreviewFrame> {
        let port = self.camera.borrow().clone()?;
        port.latest()
    }

    pub fn pump(&self) {
        self.horizontal.fit();
        self.vertical.fit();
        self.horizontal.present();
        self.vertical.present();
    }
}

impl PreviewSlot {
    fn attach(mtm: MainThreadMarker, name: &'static str, title: &str, aspect: f64) -> PreviewSlot {
        let pane = NSView::initWithFrame(
            NSView::alloc(mtm),
            NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(80.0, 80.0)),
        );
        pane.setAutoresizingMask(
            NSAutoresizingMaskOptions::ViewWidthSizable
                | NSAutoresizingMaskOptions::ViewHeightSizable,
        );

        let label = NSTextField::labelWithString(&NSString::from_str(title), mtm);
        pane.addSubview(&label);

        let host = NSView::initWithFrame(
            NSView::alloc(mtm),
            NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(16.0, 9.0)),
        );
        host.setWantsLayer(true);
        host.setClipsToBounds(true);
        let layer = unsafe { AVSampleBufferDisplayLayer::new() };
        if let Some(gravity) = unsafe { AVLayerVideoGravityResizeAspect } {
            unsafe { layer.setVideoGravity(gravity) };
        }
        let as_layer: &CALayer = layer.as_ref();
        as_layer.setAutoresizingMask(
            CAAutoresizingMask::LayerWidthSizable | CAAutoresizingMask::LayerHeightSizable,
        );
        if let Some(backing) = host.layer() {
            as_layer.setFrame(host.bounds());
            backing.addSublayer(as_layer);
        }
        pane.addSubview(&host);

        PreviewSlot {
            name,
            aspect,
            pane,
            label,
            host,
            layer,
            port: RefCell::new(None),
            last_pts_value: Cell::new(0),
            last_pts_timescale: Cell::new(0),
        }
    }

    fn bind(&self, ports: &[Arc<PreviewPort>]) {
        let port = ports
            .iter()
            .find(|port| port.spec().name() == self.name)
            .cloned();
        *self.port.borrow_mut() = port;
        self.last_pts_value.set(0);
        self.last_pts_timescale.set(0);
        let renderer = unsafe { self.layer.sampleBufferRenderer() };
        unsafe { renderer.flushWithRemovalOfDisplayedImage_completionHandler(true, None) };
    }

    fn fit(&self) {
        let bounds = self.pane.bounds();
        let w = bounds.size.width;
        let h = bounds.size.height;
        self.label.setFrame(NSRect::new(
            NSPoint::new(PAD, (h - LABEL_H - 2.0).max(0.0)),
            NSSize::new((w - PAD * 2.0).max(8.0), LABEL_H),
        ));
        let media_top = (h - LABEL_H - GAP - 2.0).max(8.0);
        let avail_w = (w - PAD * 2.0).max(8.0);
        let avail_h = (media_top - PAD).max(8.0);
        let (mw, mh) = fit_aspect(avail_w, avail_h, self.aspect);
        let x = PAD + (avail_w - mw) / 2.0;
        let y = PAD + (avail_h - mh) / 2.0;
        self.host
            .setFrame(NSRect::new(NSPoint::new(x, y), NSSize::new(mw, mh)));
        let as_layer: &CALayer = self.layer.as_ref();
        as_layer.setFrame(self.host.bounds());
    }

    fn present(&self) {
        let Some(port) = self.port.borrow().clone() else {
            return;
        };
        let Some(frame) = port.latest() else {
            return;
        };
        let pts = frame.pts;
        if pts.value == self.last_pts_value.get() && pts.timescale == self.last_pts_timescale.get()
        {
            return;
        }
        self.last_pts_value.set(pts.value);
        self.last_pts_timescale.set(pts.timescale);

        let renderer = unsafe { self.layer.sampleBufferRenderer() };
        if unsafe { renderer.requiresFlushToResumeDecoding() } {
            unsafe { renderer.flushWithRemovalOfDisplayedImage_completionHandler(true, None) };
        }
        let Some(sample) = sample_buffer_for_preview(frame.pixels.get(), pts) else {
            return;
        };
        unsafe { renderer.enqueueSampleBuffer(&sample) };
    }
}

fn fit_aspect(avail_w: f64, avail_h: f64, aspect: f64) -> (f64, f64) {
    if avail_w <= 0.0 || avail_h <= 0.0 || aspect <= 0.0 {
        return (1.0, 1.0);
    }
    if avail_w / avail_h > aspect {
        let h = avail_h;
        (h * aspect, h)
    } else {
        let w = avail_w;
        (w, w / aspect)
    }
}

fn sample_buffer_for_preview(
    pixels: &CVImageBuffer,
    pts: objc2_core_media::CMTime,
) -> Option<CFRetained<CMSampleBuffer>> {
    let mut desc: *const CMVideoFormatDescription = std::ptr::null();
    let status = unsafe {
        CMVideoFormatDescriptionCreateForImageBuffer(None, pixels, NonNull::from(&mut desc))
    };
    if status != 0 {
        return None;
    }
    let desc = NonNull::new(desc as *mut CMVideoFormatDescription)?;
    let desc = unsafe { CFRetained::from_raw(desc) };

    let mut timing = CMSampleTimingInfo {
        duration: unsafe { kCMTimeInvalid },
        presentationTimeStamp: pts,
        decodeTimeStamp: unsafe { kCMTimeInvalid },
    };
    let mut out: *mut CMSampleBuffer = std::ptr::null_mut();
    let status = unsafe {
        CMSampleBuffer::create_ready_with_image_buffer(
            None,
            pixels,
            &desc,
            NonNull::from(&mut timing),
            NonNull::from(&mut out),
        )
    };
    if status != 0 {
        return None;
    }
    let sample = NonNull::new(out)?;
    let sample = unsafe { CFRetained::from_raw(sample) };
    mark_display_immediately(&sample);
    Some(sample)
}

fn mark_display_immediately(sample: &CMSampleBuffer) {
    let Some(attachments) = (unsafe { sample.sample_attachments_array(true) }) else {
        return;
    };
    if attachments.count() < 1 {
        return;
    }
    let ptr = unsafe { attachments.value_at_index(0) };
    if ptr.is_null() {
        return;
    }
    let key: &NSString =
        unsafe { &*(kCMSampleAttachmentKey_DisplayImmediately as *const _ as *const NSString) };
    let dict: &NSMutableDictionary<NSString, NSNumber> = unsafe { &*ptr.cast() };
    unsafe {
        dict.setObject_forKey(&NSNumber::new_bool(true), ProtocolObject::from_ref(key));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_wide_box_is_height_limited_for_sixteen_by_nine() {
        let (w, h) = fit_aspect(320.0, 90.0, 16.0 / 9.0);
        assert!((h - 90.0).abs() < 0.01);
        assert!((w - 160.0).abs() < 0.01);
    }

    #[test]
    fn a_tall_box_is_width_limited_for_nine_by_sixteen() {
        let (w, h) = fit_aspect(90.0, 320.0, 9.0 / 16.0);
        assert!((w - 90.0).abs() < 0.01);
        assert!((h - 160.0).abs() < 0.01);
    }
}
