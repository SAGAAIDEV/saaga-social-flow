//! Candidate generation and persistence, separate from UI and brief storage.
use super::*;

pub(super) fn run(
    session: &Session,
    models: &[image::ModelSpec],
    per_model: usize,
    tx: &Sender<ThumbnailEvent>,
) -> Result<(usize, usize)> {
    let status = |msg: String| {
        let _ = tx.send(ThumbnailEvent::Status(msg));
    };

    let stills = still::list(&session.root);
    let Some(still_path) = stills.first() else {
        anyhow::bail!("no camera still yet — press Retake photo first");
    };
    let still_bytes = std::fs::read(still_path)
        .with_context(|| format!("reading {}", still_path.display()))?;
    let still_id = crate::agent::prompt::hash_of_bytes(&still_bytes);

    // Optional by nature: a talking-head layout has no screen to have caught.
    let screen_bytes = still::list_screens(&session.root)
        .first()
        .and_then(|path| std::fs::read(path).ok());
    let screen_id = screen_bytes
        .as_deref()
        .map(crate::agent::prompt::hash_of_bytes);
    if screen_bytes.is_some() {
        status("Drawing from the camera and the screen…".to_string());
    }

    let saved =
        load_brief_or_default(session, crate::config::load().thumbnail.brief).unwrap_or_default();
    if saved.brief.is_empty() {
        anyhow::bail!("nothing to draw from — write a title or a description first");
    }

    let library = references::load(&references::library_root()?);
    let refs_id = library.active_hash();
    // Kept apart from the still, because they are asking for different things:
    // one is who the picture is of, the others are only how it should look.
    let mut style_refs = Vec::new();
    for reference in library.active() {
        match std::fs::read(&reference.path) {
            Ok(bytes) => style_refs.push(bytes),
            Err(err) => status(format!("skipping reference {}: {err}", reference.name)),
        }
    }
    if !style_refs.is_empty() {
        status(format!("{} style reference(s) in play…", style_refs.len()));
    }

    // Hashed from the finished prompt, so editing either field invalidates the
    // pictures it produced. Two boxes, one identity.
    let format = crate::card::load(&session.root).format;
    let prompt = saved.brief.render_for(format);
    let brief_hash = crate::agent::prompt::hash_of(&prompt);
    let key = std::env::var("OPENROUTER_API_KEY")
        .ok()
        .filter(|key| !key.trim().is_empty())
        .context("OPENROUTER_API_KEY unset")?;

    let mut rows = schema::load(&session.root);
    // Retain earlier formats, local cards and the active selection. Each press
    // adds fresh variants; a provider failure must not erase finished work.
    let mut made = 0usize;
    // Every refusal, kept rather than flashed past. A model that 404s used to
    // land in a status line the next repaint overwrote, and the run then reported
    // "0 new candidate(s)" — which reads as "nothing to do" when what happened is
    // "the request was rejected". A whole run of those said nothing at all.
    let mut refused: Vec<String> = Vec::new();
    for model in models {
        for nth in 0..per_model.max(1) {
            let mut variant = nth;
            let id = loop {
                let id = schema::candidate_id(
                    &model.id, &brief_hash, &still_id,
                    screen_id.as_deref().unwrap_or_default(), &refs_id, variant,
                );
                if !schema::already_generated(&rows, &id) { break id; }
                variant += 1;
            };
            status(format!("{} — candidate {}…", model.label, nth + 1));
            match image::generate(
                &key,
                &model.id,
                &prompt,
                &still_bytes,
                screen_bytes.as_deref(),
                &style_refs,
                format,
            ) {
                Ok(candidate) => {
                    let row = write_candidate(
                        session,
                        &id,
                        &candidate,
                        &brief_hash,
                        &still_id,
                        screen_id.as_deref(),
                        &refs_id,
                    )?;
                    rows.push(schema::Row::Candidate(row));
                    made += 1;
                }
                // One model failing must not lose the other's work.
                Err(err) => {
                    eprintln!("stream-recorder: {} failed: {err:#}", model.label);
                    status(format!("{} failed: {err:#}", model.label));
                    refused.push(format!("{}: {err:#}", model.label));
                }
            }
        }
    }
    // Drew nothing and was refused every time: that is a failed run, not a quiet
    // one, and it has to say so where "nothing new to draw" would be a lie.
    if made == 0 && !refused.is_empty() {
        anyhow::bail!("{}", refused.join("; "));
    }
    Ok((made, schema::candidates(&rows).len()))
}

fn write_candidate(
    session: &Session,
    id: &str,
    candidate: &image::Candidate,
    brief_hash: &str,
    still: &str,
    screen: Option<&str>,
    refs: &str,
) -> Result<schema::Candidate> {
    let dir = session.root.join(schema::CANDIDATES_DIR);
    std::fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;
    let file = format!("{}/{id}.jpg", schema::CANDIDATES_DIR);
    let path = session.root.join(&file);
    std::fs::write(&path, &candidate.jpeg)
        .with_context(|| format!("writing {}", path.display()))?;

    let row = schema::Candidate {
        id: id.to_string(),
        model: candidate.model.clone(),
        file,
        created_at: crate::schedule::ledger::now_rfc3339(),
        brief_hash: brief_hash.to_string(),
        still: still.to_string(),
        screen: screen.map(str::to_string),
        refs: refs.to_string(),
    };
    schema::append(&session.root, &schema::Row::Candidate(row.clone()))?;
    Ok(row)
}

