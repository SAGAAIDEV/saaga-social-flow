//! `stream-recorder card` — draw one card and exit.
//!
//! The rasteriser is the one piece of this module a `cargo test` cannot reach:
//! it needs an `NSApplication` and a run loop, which the test harness has
//! neither of. So it gets a command instead, and running it is how the
//! offscreen-window path is proved without pressing a button in the app.
//!
//! It is not only a test hook. Drawing a card from a title, a still and nothing
//! else is a reasonable thing to want on its own — for a video recorded before
//! this existed, or for a thumbnail that has nothing to do with a recording.
//!
//! ```
//! stream-recorder card --out /tmp/card.jpg --title "Ship it anyway" \
//!   --description "Why the queue fell over." --still ~/…/still-ab.jpg
//! ```
//!
//! ## Its own run loop
//!
//! `NSApplication` is started here rather than reusing the recorder's, because
//! this command opens no recorder: there is no session, no camera and no window.
//! The loop runs until the snapshot arrives and is then stopped, so the process
//! exits on its own rather than sitting in an event loop nobody is driving.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};
use objc2_app_kit::{NSApplication, NSApplicationActivationPolicy};

use super::{raster, Card};

/// A card is small and local; anything past this is a hang, not slow work.
const TIMEOUT: Duration = Duration::from_secs(30);

/// How long to let the run loop turn between checks for the answer.
const TICK: Duration = Duration::from_millis(10);

pub struct Request {
    pub out: PathBuf,
    pub card: Card,
    pub still: Option<PathBuf>,
    pub size: (u32, u32),
}

pub fn run(request: Request) -> Result<PathBuf> {
    let Some(mtm) = objc2::MainThreadMarker::new() else {
        bail!("the card command must run on the main thread");
    };
    let app = NSApplication::sharedApplication(mtm);
    // Accessory: no Dock icon and no menu bar for a command that draws a file
    // and exits. `finishLaunching` rather than `run` because the loop is turned
    // by hand below — see the module docs.
    app.setActivationPolicy(NSApplicationActivationPolicy::Accessory);
    app.finishLaunching();

    if let Some(still) = request.still.as_deref() {
        if !still.is_file() {
            bail!("no such still: {}", still.display());
        }
    }

    // The page and the photo both have to sit under one directory the web view
    // is allowed to read — see `raster::Raster::draw`. Out of the way of any
    // project, since this command has none.
    let work = std::env::temp_dir().join(format!("stream-recorder-card-{}", std::process::id()));
    std::fs::create_dir_all(&work).with_context(|| format!("creating {}", work.display()))?;
    let still = match request.still.as_deref() {
        Some(path) => {
            let copied = work.join("still.jpg");
            std::fs::copy(path, &copied).with_context(|| format!("copying {}", path.display()))?;
            Some(copied)
        }
        None => None,
    };

    let html = super::render::html(
        Path::new(env!("CARGO_MANIFEST_DIR")),
        &request.card,
        still.as_deref(),
        request.size.0,
        request.size.1,
    )?;
    let page = work.join("card.html");
    std::fs::write(&page, &html).with_context(|| format!("writing {}", page.display()))?;

    let (tx, rx) = std::sync::mpsc::channel();
    let _raster = raster::Raster::draw(
        mtm,
        &page,
        &work,
        request.size,
        request.size.0.max(request.size.1) as f64,
        tx,
    )?;

    let started = Instant::now();
    let jpeg = loop {
        // One pass of the loop per tick: the navigation, the decode and the
        // snapshot all arrive as run-loop work, so a bare `recv_timeout` would
        // wait forever on events nothing is pumping.
        turn(&app);
        match rx.try_recv() {
            Ok(raster::RasterEvent::Drawn { jpeg }) => break jpeg,
            Ok(raster::RasterEvent::Failed(message)) => bail!("{message}"),
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                bail!("the rasteriser went away without answering")
            }
            Err(std::sync::mpsc::TryRecvError::Empty) if started.elapsed() >= TIMEOUT => {
                bail!("the card did not draw within {TIMEOUT:?}")
            }
            Err(std::sync::mpsc::TryRecvError::Empty) => {}
        }
    };

    if let Some(parent) = request.out.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating {}", parent.display()))?;
    }
    std::fs::write(&request.out, &jpeg)
        .with_context(|| format!("writing {}", request.out.display()))?;
    let _ = std::fs::remove_dir_all(&work);
    Ok(request.out)
}

/// Drains whatever the run loop has, without blocking on an empty queue.
fn turn(app: &NSApplication) {
    use objc2_app_kit::NSEventMask;
    use objc2_foundation::{NSDate, NSDefaultRunLoopMode};

    let until = NSDate::dateWithTimeIntervalSinceNow(TICK.as_secs_f64());
    while let Some(event) = unsafe {
        app.nextEventMatchingMask_untilDate_inMode_dequeue(
            NSEventMask::Any,
            Some(&until),
            NSDefaultRunLoopMode,
            true,
        )
    } {
        app.sendEvent(&event);
    }
}

/// Generate the same three-asset manifest as the desktop workflow.
pub fn run_set(request: Request) -> Result<PathBuf> {
    let still = request
        .still
        .as_deref()
        .context("--all-formats requires --still")?;
    let bytes = std::fs::read(still)?;
    crate::thumbnail::still::write_bytes(&request.out, &bytes)?;
    super::save(&request.out, &request.card)?;
    let mut job = super::assets::Job::new(&request.out, request.card)?;
    for _ in super::assets::Kind::ALL {
        // `job.kind()` rather than the loop's own: the job advances on `accept`,
        // and two walks of `Kind::ALL` side by side is one way for the picture
        // and the slot it lands in to come apart.
        let kind = job.kind();
        // Drawn aside and handed over, because the job owns the set directory —
        // including whether these bytes are allowed into it at all.
        let rendered = std::env::temp_dir().join(format!(
            "stream-recorder-set-{}-{}.jpg",
            std::process::id(),
            kind.name()
        ));
        run(Request {
            out: rendered.clone(),
            card: job.design(),
            still: Some(job.photo.clone()),
            size: kind.size(),
        })?;
        job.accept(&std::fs::read(&rendered)?)?;
        let _ = std::fs::remove_file(&rendered);
    }
    job.commit()?;
    Ok(request.out.join(super::assets::MANIFEST))
}
