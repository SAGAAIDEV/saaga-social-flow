//! The card's HTML to a JPEG, through a web view nobody sees.
//!
//! WebKit is already linked into this app for the panes, and it is a better
//! rasteriser than anything that could be added: real CSS layout, real system
//! fonts, subpixel-correct text. The alternative — a JavaScript SVG renderer
//! plus a rasteriser, and a font file vendored beside them because such
//! renderers cannot read system fonts — is two dependencies and a worse picture.
//!
//! ## Why there is a window
//!
//! A `WKWebView` with no window has no backing layer, and
//! `takeSnapshotWithConfiguration:` on one answers with a blank image rather
//! than an error. So the view goes into a borderless window sized to the card
//! and positioned far off every screen. The window is never ordered front, so
//! nothing appears on screen and nothing steals focus — but it exists, which is
//! the part WebKit needs.
//!
//! It is also `NSWindowSharingType::None`, like the region and snip overlays: a
//! window drawing a thumbnail must not turn up in a recording even in the moment
//! it is off screen, because "off every screen" is a position, not a promise.
//!
//! ## Why it waits for the navigation
//!
//! Snapshotting before `didFinish` reliably yields a blank card — the page is
//! loaded asynchronously and the photograph is a `file://` image that has to be
//! read and decoded. `afterScreenUpdates` then forces one more update pass
//! before the pixels are taken, which is what covers the gap between "the DOM is
//! ready" and "it has been drawn".
//!
//! ## Panics here abort the process
//!
//! Everything below runs inside a delegate callback or a completion block, where
//! a panic unwinds into Objective-C.

use std::cell::RefCell;
use std::path::Path;
use std::sync::mpsc::Sender;

use anyhow::{Context, Result};
use block2::RcBlock;
use objc2::rc::Retained;
use objc2::runtime::{AnyObject, NSObjectProtocol, ProtocolObject};
use objc2::{define_class, DefinedClass, MainThreadMarker, MainThreadOnly};
use objc2_app_kit::{
    NSBackingStoreType, NSImage, NSWindow, NSWindowSharingType, NSWindowStyleMask,
};
use objc2_foundation::{NSError, NSPoint, NSRect, NSSize, NSString, NSURL};
use objc2_web_kit::{
    WKNavigation, WKNavigationDelegate, WKSnapshotConfiguration, WKWebView,
    WKWebViewConfiguration,
};

/// Far enough off that no display arrangement reaches it. Negative rather than
/// large-positive so it is also clear of a display placed to the right.
const OFFSCREEN: f64 = -50_000.0;

/// What the caller hears when the picture is ready.
pub enum RasterEvent {
    Drawn { jpeg: Vec<u8> },
    Failed(String),
}

pub struct DelegateIvars {
    /// Taken by the first `didFinish`. A page can finish navigating more than
    /// once — an in-page load, a failed subresource retried — and a second
    /// snapshot would send a second event for a job the caller has finished.
    job: RefCell<Option<Job>>,
}

struct Job {
    tx: Sender<RasterEvent>,
    /// Long edge of the JPEG. The page is already laid out at the output size,
    /// so this only bounds a caller asking for something enormous.
    max_edge: f64,
}

define_class!(
    // SAFETY:
    // - `WKNavigationDelegate` is a main-thread protocol, which `MainThreadOnly`
    //   enforces at the type level.
    // - `SnapshotDelegate` does not implement `Drop`.
    #[unsafe(super(objc2_foundation::NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "StreamRecorderCardSnapshot"]
    #[ivars = DelegateIvars]
    pub struct SnapshotDelegate;

    unsafe impl NSObjectProtocol for SnapshotDelegate {}

    unsafe impl WKNavigationDelegate for SnapshotDelegate {
        #[unsafe(method(webView:didFinishNavigation:))]
        fn did_finish(&self, webview: &WKWebView, _navigation: Option<&WKNavigation>) {
            let Ok(mut slot) = self.ivars().job.try_borrow_mut() else {
                return;
            };
            let Some(job) = slot.take() else {
                return;
            };
            snapshot(webview, job);
        }

        #[unsafe(method(webView:didFailNavigation:withError:))]
        fn did_fail(
            &self,
            _webview: &WKWebView,
            _navigation: Option<&WKNavigation>,
            error: &NSError,
        ) {
            self.fail(&error.localizedDescription().to_string());
        }

        /// A page that never starts loading fails here rather than in
        /// `didFail:`, and without both the caller waits forever.
        #[unsafe(method(webView:didFailProvisionalNavigation:withError:))]
        fn did_fail_provisional(
            &self,
            _webview: &WKWebView,
            _navigation: Option<&WKNavigation>,
            error: &NSError,
        ) {
            self.fail(&error.localizedDescription().to_string());
        }
    }
);

impl SnapshotDelegate {
    fn new(mtm: MainThreadMarker, job: Job) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(DelegateIvars {
            job: RefCell::new(Some(job)),
        });
        unsafe { objc2::msg_send![super(this), init] }
    }

    fn fail(&self, message: &str) {
        let Ok(mut slot) = self.ivars().job.try_borrow_mut() else {
            return;
        };
        if let Some(job) = slot.take() {
            let _ = job
                .tx
                .send(RasterEvent::Failed(format!("the card page failed to load: {message}")));
        }
    }
}

/// Takes the picture and posts it.
fn snapshot(webview: &WKWebView, job: Job) {
    // Reached only from a `WKNavigationDelegate` callback, which AppKit
    // guarantees is the main thread.
    let Some(mtm) = MainThreadMarker::new() else {
        let _ = job.tx.send(RasterEvent::Failed(
            "the snapshot callback arrived off the main thread".into(),
        ));
        return;
    };
    let config = unsafe { WKSnapshotConfiguration::new(mtm) };
    unsafe {
        // The whole view. `setRect` defaults to the visible region, which is the
        // same thing here — set explicitly so a future resize cannot quietly
        // crop the card.
        config.setRect(webview.bounds());
        // One more update pass before the pixels are read: `didFinish` means the
        // navigation completed, not that it has been drawn.
        config.setAfterScreenUpdates(true);
    }
    let max_edge = job.max_edge;
    let tx = job.tx;
    let handler = RcBlock::new(move |image: *mut NSImage, error: *mut NSError| {
        let event = match encode(image, error, max_edge) {
            Ok(jpeg) => RasterEvent::Drawn { jpeg },
            Err(err) => RasterEvent::Failed(format!("{err:#}")),
        };
        let _ = tx.send(event);
    });
    unsafe {
        webview.takeSnapshotWithConfiguration_completionHandler(Some(&config), &handler);
    }
}

/// The snapshot to JPEG bytes.
///
/// Split out so the completion block has no `?` in it: an early return from
/// inside one is a card that silently never appears.
fn encode(image: *mut NSImage, error: *mut NSError, max_edge: f64) -> Result<Vec<u8>> {
    if !error.is_null() {
        let message = unsafe { (*error).localizedDescription() }.to_string();
        anyhow::bail!("webkit refused the snapshot: {message}");
    }
    let image = unsafe { image.as_ref() }.context("webkit returned no snapshot")?;
    let size = image.size();
    let rect = NSRect::new(NSPoint::new(0.0, 0.0), size);
    let cg = unsafe {
        image.CGImageForProposedRect_context_hints(
            &mut { rect } as *mut NSRect,
            None,
            None,
        )
    }
    .context("the snapshot carried no bitmap")?;
    crate::thumbnail::still::encode_cg(&cg, max_edge)
}

/// An offscreen web view, alive for exactly one card.
///
/// Held by the caller until the event arrives: dropping it takes the window and
/// the delegate with it, and `WKWebView` holds its navigation delegate *weakly*
/// — so a dropped handle is a snapshot that never fires and a caller that waits
/// forever.
pub struct Raster {
    window: Retained<NSWindow>,
    webview: Retained<WKWebView>,
    _delegate: Retained<SnapshotDelegate>,
}

impl Raster {
    /// Loads `page` and photographs it. The outcome arrives on `tx`.
    ///
    /// `access` is the directory the page may read from — the project root, so
    /// the `file://` photograph in it resolves. `loadFileURL` is what grants
    /// that; a `loadHTMLString` with a file base URL does not, and the card
    /// would draw with an empty photo column and no error.
    pub fn draw(
        mtm: MainThreadMarker,
        page: &Path,
        access: &Path,
        size: (u32, u32),
        max_edge: f64,
        tx: Sender<RasterEvent>,
    ) -> Result<Raster> {
        let frame = NSRect::new(
            NSPoint::new(OFFSCREEN, OFFSCREEN),
            NSSize::new(size.0 as f64, size.1 as f64),
        );
        let window = unsafe {
            NSWindow::initWithContentRect_styleMask_backing_defer(
                NSWindow::alloc(mtm),
                frame,
                NSWindowStyleMask::Borderless,
                NSBackingStoreType::Buffered,
                false,
            )
        };
        // Same reason as every other window in this crate: winit closes every
        // window in `[NSApp windows]` as the loop returns, and the default YES
        // here makes that `close` release a reference AppKit never took.
        unsafe { window.setReleasedWhenClosed(false) };
        // See the module docs: off screen is a position, not a promise.
        window.setSharingType(NSWindowSharingType::None);

        let config = unsafe { WKWebViewConfiguration::new(mtm) };
        let webview = unsafe {
            WKWebView::initWithFrame_configuration(
                WKWebView::alloc(mtm),
                NSRect::new(
                    NSPoint::new(0.0, 0.0),
                    NSSize::new(size.0 as f64, size.1 as f64),
                ),
                &config,
            )
        };

        let delegate = SnapshotDelegate::new(mtm, Job { tx, max_edge });
        unsafe {
            webview.setNavigationDelegate(Some(ProtocolObject::from_ref(&*delegate)));
        }
        window.setContentView(Some(&webview));
        // Ordered in, but off screen. Without this the view has no backing layer
        // and every snapshot comes back blank.
        window.orderBack(None::<&AnyObject>);

        let url = file_url(page).context("the card page has a path WebKit cannot take")?;
        let access = file_url(access).context("the project root has a path WebKit cannot take")?;
        unsafe { webview.loadFileURL_allowingReadAccessToURL(&url, &access) };

        Ok(Raster {
            window,
            webview,
            _delegate: delegate,
        })
    }
}

impl Drop for Raster {
    fn drop(&mut self) {
        // The delegate is held weakly by the view, so clear it before either can
        // outlive the other and a late callback messages freed memory.
        unsafe { self.webview.setNavigationDelegate(None) };
        self.window.orderOut(None::<&AnyObject>);
        self.window.close();
    }
}

fn file_url(path: &Path) -> Option<Retained<NSURL>> {
    Some(NSURL::fileURLWithPath(&NSString::from_str(path.to_str()?)))
}

