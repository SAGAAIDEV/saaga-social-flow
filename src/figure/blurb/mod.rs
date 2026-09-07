//! Writing the blurb: what this picture shows, and what to call it.
//!
//! A vision call, because that is the only way to know what is *in* a
//! screenshot — but never a vision call on its own. The model also gets what
//! the author *said about it*: the aside recorded while the take was paused —
//! see [`crate::figure::aside`] — transcribed, which is the whole point of the
//! break. Behind that, when there is one, comes the chapter transcript around
//! the moment it was snipped. A screenshot of a terminal described from pixels
//! alone is "a terminal window"; described with the sentence the author stopped
//! to say over it, it is "the retry storm that took the queue down".
//!
//! The blurb is written on its own once the words arrive — [`auto`] — and the
//! Write Blurbs button is the retry for any figure that has none.
//!
//! ## Not through rig
//!
//! Same reason as [`crate::thumbnail::image`], which is where the OpenRouter
//! image-part shape in [`wire`] comes from: rig's extractor takes a text
//! prompt, and there is no room in it for an attachment. One hand-rolled client
//! against `/chat/completions` sends pictures in and structured JSON out.
//!
//! ## Layout of this module
//!
//! | file | responsibility |
//! |---|---|
//! | `mod.rs` | the job: which figures are left, what context each one gets, what a failure costs |
//! | [`auto`] | the one-figure job that waits for the explanation to transcribe and writes the blurb |
//! | [`wire`] | one figure in, one blurb out — the request shape, the salvage, the length clipping |
//!
//! ## Failures are per figure
//!
//! A pass over ten figures that dies on the third has still earned two blurbs,
//! and they are appended as they land. Nothing is retried automatically: a
//! second press writes only what is still unwritten, so the recovery is the same
//! gesture as the first attempt.

use std::path::Path;
use std::sync::mpsc::Sender;
use std::thread;

use anyhow::{Context, Result};

pub mod auto;
mod wire;
#[cfg(test)]
mod tests;

pub use wire::Blurb;

use super::{Figure, FigureEvent};
use crate::session::Session;

/// How much of the transcript rides along, either side of the moment.
///
/// Twenty-five seconds is about sixty spoken words each way. Enough to carry
/// the point being made, short enough that the model cannot mistake a later
/// topic for what is on screen — a figure captioned from two minutes of
/// transcript describes the argument rather than the picture.
const WINDOW_SECS: f64 = 25.0;

/// The prompt id, so the voice is tunable from the prompt library like every
/// other stage's. See [`crate::agent::prompt`].
pub const PROMPT_ID: &str = crate::agent::prompt::FIGURE;

pub const SYSTEM_PROMPT: &str = r#"You caption figures in a technical article on saagasolve.com.

Each figure is a screenshot the author stopped the recording to take, at a
moment they thought was worth showing. You are given the picture, what the
author said about it while the recording was paused, and — when there is one —
what was being said in the recording around that moment.

caption — one or two sentences, under 200 characters, printed under the figure.
Say what the reader is looking at and why it matters. Name what is actually
visible: the tool, the file, the number, the error. Never "as you can see in this
screenshot" and never address the reader as a viewer — the article stands on its
own and the figure has no video around it.

The author's explanation says why the figure is here; the picture says what is
on screen. Caption from both. Where they disagree about what is visible, the
picture wins — the author may have been describing what they were about to
show. Never state a number, name or outcome that is in neither.

If the picture shows nothing meaningful — a blank desktop, a half-drawn window,
a menu mid-open — say so plainly in the caption rather than inventing
significance for it. A figure that should not be published is more useful than a
confident caption over nothing.

alt — under 125 characters, for a screen reader. Describe the content, not the
significance, and do not begin with "image of" or "screenshot of".
"#;

/// Write blurbs for every figure that has none, on a worker thread.
pub fn spawn(
    session: Session,
    model: String,
    provider: Option<String>,
    tx: Sender<FigureEvent>,
) {
    let _ = thread::Builder::new()
        .name("figure-blurbs".into())
        .spawn(move || {
            let figures = super::unwritten(&session.root);
            if figures.is_empty() {
                let _ = tx.send(FigureEvent::Blurbs {
                    written: 0,
                    failures: Vec::new(),
                });
                return;
            }
            let prompt = crate::agent::prompt::resolve(
                PROMPT_ID,
                SYSTEM_PROMPT,
                Some(&session.root),
            );
            let total = figures.len();
            let mut written = 0usize;
            let mut failures = Vec::new();
            for (index, figure) in figures.iter().enumerate() {
                // Its words are on the way; a blurb written now would be from
                // the picture alone and would stand, because the row exists.
                if figure.transcribing {
                    failures.push(format!(
                        "figure {:02}: its explanation is still transcribing — press again when it has",
                        figure.n
                    ));
                    continue;
                }
                let _ = tx.send(FigureEvent::Status(format!(
                    "Writing blurb {} of {total} — figure {:02}…",
                    index + 1,
                    figure.n,
                )));
                match write_one(&session, figure, &prompt.text, &model, provider.as_deref()) {
                    // Appended as it lands rather than batched at the end: a
                    // pass that dies halfway has still earned the blurbs before
                    // the failure, and losing them would mean paying twice.
                    Ok(blurb) => {
                        match super::append_blurb(
                            &session.root,
                            &figure.file,
                            &blurb.caption,
                            &blurb.alt,
                        ) {
                            Ok(()) => written += 1,
                            Err(err) => {
                                failures.push(format!("figure {:02}: {err:#}", figure.n))
                            }
                        }
                    }
                    Err(err) => failures.push(format!("figure {:02}: {err:#}", figure.n)),
                }
            }
            let _ = tx.send(FigureEvent::Blurbs { written, failures });
        });
}

pub(super) fn write_one(
    session: &Session,
    figure: &Figure,
    preamble: &str,
    model: &str,
    provider: Option<&str>,
) -> Result<Blurb> {
    let jpeg = std::fs::read(&figure.file)
        .with_context(|| format!("reading {}", figure.file.display()))?;
    let context = context_for(&session.root, figure, session.title());
    wire::ask(model, provider, preamble, &context, &jpeg)
}

/// Everything about the moment that is not the picture: the author's
/// explanation first, then what the recording was saying around the moment.
pub fn context_for(root: &Path, figure: &Figure, title: String) -> String {
    let mut lines = vec![
        format!("Video: {title}"),
        format!("Taken at: {}", figure.moment()),
    ];
    let said = figure.said.trim();
    if !said.is_empty() {
        lines.push(String::new());
        lines.push("The author paused the recording to explain this figure. They said:".to_string());
        lines.push(said.to_string());
    }
    match spoken_around(root, figure) {
        Some(words) => {
            lines.push(String::new());
            lines.push("Said in the recording around that moment:".to_string());
            lines.push(words);
        }
        // Said explicitly rather than left off. A model given no words and no
        // note about it reads the silence as "there was nothing worth saying
        // here" and captions apologetically.
        None if said.is_empty() => lines.push(
            "\nThere is no explanation or transcript for this moment — describe the picture alone."
                .to_string(),
        ),
        None => {}
    }
    lines.join("\n")
}

/// The words spoken within [`WINDOW_SECS`] either side of the figure.
///
/// `None` when nothing was recording, the chapter never transcribed, or the
/// window is silent — all three mean the same thing to the caller.
fn spoken_around(root: &Path, figure: &Figure) -> Option<String> {
    let chapter = figure.chapter?;
    let offset = figure.offset?;
    let transcript = crate::notes::load_transcript(root, chapter)?;
    // AssemblyAI counts in milliseconds; a figure's offset is in seconds.
    let from = ((offset - WINDOW_SECS).max(0.0) * 1000.0) as i64;
    let to = ((offset + WINDOW_SECS) * 1000.0) as i64;
    let words: Vec<&str> = transcript
        .words
        .iter()
        .filter(|word| word.start >= from && word.start <= to)
        .map(|word| word.text.as_str())
        .collect();
    // Fall back to the whole chapter's text when there are no timings at all:
    // a transcript with `text` but no `words` is what a provider that returned
    // only a paragraph leaves behind, and the whole chapter is worse context
    // than a window but far better than none.
    if words.is_empty() {
        let text = transcript.text.trim();
        return (!text.is_empty()).then(|| text.to_string());
    }
    Some(words.join(" "))
}
