//! The blurb written on its own, once the explanation has transcribed.
//!
//! The break ends, the aside's audio goes to the transcriber, and some seconds
//! later there are words. This waits for them and writes the blurb — nobody
//! should have to remember a button for a caption whose material they just
//! spoke. It waits for the *capture row* too: the shutter answers on its own
//! queue, and a break ended briskly can be over before the picture is filed.
//!
//! One thread per figure rather than one pass over all of them, because each
//! figure's words arrive at their own time and a pass would either wait for the
//! slowest or leave the rest for the button. The button — [`super::spawn`] — is
//! still the retry: this never tries twice, and a figure it fails to caption is
//! exactly the figure "Write Blurbs" is for.
//!
//! An explanation that came back silent, skipped or failed is not an error
//! here. The blurb is written from the picture alone, as it would be for a
//! figure taken with no break, and the status says so — the author should know
//! their words did not land, not lose the figure over it.

use std::sync::mpsc::Sender;
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{bail, Result};

use crate::figure::{self, Figure, FigureEvent};
use crate::session::Session;

const WAIT_EVERY: Duration = Duration::from_secs(2);
/// Generous, because the transcriber's own job polls for up to ten minutes and
/// a wait that gave up first would caption from the picture while the words
/// were about to arrive.
const WAIT_FOR: Duration = Duration::from_secs(660);

/// Caption figure `n` once its explanation has transcribed, on a worker thread.
pub fn spawn(
    session: Session,
    n: u32,
    model: String,
    provider: Option<String>,
    tx: Sender<FigureEvent>,
) {
    let _ = thread::Builder::new()
        .name(format!("figure-{n:02}-blurb"))
        .spawn(move || {
            let outcome = run(&session, n, &model, provider.as_deref(), &tx);
            let _ = tx.send(match outcome {
                Ok(Some(figure)) => FigureEvent::Explained {
                    n,
                    explained: figure.explained(),
                    failure: None,
                },
                // Already captioned by hand while this waited: nothing to say.
                Ok(None) => return,
                Err(err) => FigureEvent::Explained {
                    n,
                    explained: false,
                    failure: Some(format!("{err:#}")),
                },
            });
        });
}

/// `Ok(None)` when the figure was captioned by someone else in the meantime.
fn run(
    session: &Session,
    n: u32,
    model: &str,
    provider: Option<&str>,
    tx: &Sender<FigureEvent>,
) -> Result<Option<Figure>> {
    let figure = wait_for_words(&session.root, n)?;
    if figure.has_blurb() {
        return Ok(None);
    }
    let _ = tx.send(FigureEvent::Status(format!(
        "Writing figure {n:02}'s blurb…"
    )));
    let prompt =
        crate::agent::prompt::resolve(super::PROMPT_ID, super::SYSTEM_PROMPT, Some(&session.root));
    let blurb = super::write_one(session, &figure, &prompt.text, model, provider)?;
    figure::append_blurb(&session.root, &figure.file, &blurb.caption, &blurb.alt)?;
    Ok(Some(figure))
}

/// The figure, once it is in the ledger and its explanation has stopped
/// transcribing — with words or without.
fn wait_for_words(root: &std::path::Path, n: u32) -> Result<Figure> {
    let started = Instant::now();
    loop {
        let figure = figure::load(root).into_iter().find(|figure| figure.n == n);
        match figure {
            Some(figure) if !figure.transcribing => return Ok(figure),
            _ if started.elapsed() > WAIT_FOR => bail!(
                "its explanation did not finish transcribing in {} minutes",
                WAIT_FOR.as_secs() / 60
            ),
            _ => thread::sleep(WAIT_EVERY),
        }
    }
}
