//! What has to be on this machine before a render can work, checked at launch.
//!
//! Written after a render died with "HyperFrames library missing at
//! …/../screencast/components" — a path inside a private repo the person
//! running it could not clone. The build was fine, the app started fine, and
//! the failure landed at the end of a recording that had already been made.
//!
//! So the rule here is: anything a later stage will *hard fail* on gets looked
//! at during startup, when the cost of being wrong is a line of text rather
//! than a lost take.
//!
//! ## What this is not
//!
//! Not a gate. Recording works with none of these — no library, no Node, no
//! network — and refusing to launch over a dependency that only the render
//! stage needs would make the app useless for the thing it is best at. Every
//! check reports and returns.
//!
//! ## Cheap at startup, thorough on demand
//!
//! [`check`] runs on every launch, so it only stats files and looks for
//! executables on `PATH`. [`report`] backs `saaga-social-flow doctor` and may
//! spend real time — it is asked for.

use std::fmt::Write as _;
use std::path::PathBuf;

/// How much a missing dependency costs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Impact {
    /// A stage will fail outright.
    Breaks,
    /// Slower or degraded, but it works.
    Degrades,
}

/// One dependency, and what was found.
#[derive(Debug, Clone)]
pub struct Finding {
    pub what: &'static str,
    pub ok: bool,
    pub impact: Impact,
    /// What is wrong, and what to do — only read when `ok` is false.
    pub detail: String,
}

impl Finding {
    fn ok(what: &'static str, detail: impl Into<String>) -> Self {
        Self {
            what,
            ok: true,
            impact: Impact::Breaks,
            detail: detail.into(),
        }
    }

    fn bad(what: &'static str, impact: Impact, detail: impl Into<String>) -> Self {
        Self {
            what,
            ok: false,
            impact,
            detail: detail.into(),
        }
    }
}

/// Is `program` runnable from `PATH`?
///
/// A `PATH` walk rather than spawning it with `--version`: this runs on every
/// launch, and starting a Node process to learn that Node exists is a visible
/// pause on an app whose whole point is to start recording quickly.
fn on_path(program: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|dir| dir.join(program))
        .find(|candidate| candidate.is_file())
}

/// The HyperFrames library, and every file a render copies out of it.
///
/// Checks the individual files rather than just the directory: a partial
/// library is the harder failure to read, because the render gets further in
/// before dying, and the message names an asset instead of the library.
fn library() -> Finding {
    let root = crate::edit::compose::components_root();
    let required = [
        "compositions/chapter-title-card.html",
        "compositions/talking-head-vertical.html",
        "assets/pattern-rings.svg",
        "assets/badge.svg",
        "assets/silence.mp3",
        "assets/fonts/Booton-Regular.woff2",
        "assets/fonts/Booton-Semibold.woff2",
        "assets/fonts/Booton-Bold.woff2",
    ];
    let missing: Vec<&str> = required
        .iter()
        .copied()
        .filter(|rel| !root.join(rel).is_file())
        .collect();

    if missing.is_empty() {
        return Finding::ok("HyperFrames library", format!("{}", root.display()));
    }
    if missing.len() == required.len() {
        return Finding::bad(
            "HyperFrames library",
            Impact::Breaks,
            format!(
                "nothing at {} — the library is vendored in this repo, so a checkout \
                 should already have it. Re-clone, or `git checkout components/`.",
                root.display()
            ),
        );
    }
    Finding::bad(
        "HyperFrames library",
        Impact::Breaks,
        format!(
            "{} of {} files missing under {} (first: {}). Restore with `git checkout components/`.",
            missing.len(),
            required.len(),
            root.display(),
            missing[0]
        ),
    )
}

/// The renderer itself, which is a Node CLI.
///
/// Present in the shared cache is best; `npx` alone still works but pays a
/// ~360 MB download on the first render, which is worth warning about *before*
/// someone is waiting on it rather than during.
fn renderer() -> Finding {
    let cached = std::env::var_os("HOME").map(|home| {
        PathBuf::from(home)
            .join(".screencast/cache/hyperframes")
            .join(crate::edit::render::HF_VERSION)
            .join("cli/node_modules/.bin/hyperframes")
    });
    if let Some(cached) = cached {
        if cached.is_file() {
            return Finding::ok(
                "HyperFrames renderer",
                format!("cached, v{}", crate::edit::render::HF_VERSION),
            );
        }
    }
    match on_path("npx") {
        Some(_) => Finding::bad(
            "HyperFrames renderer",
            Impact::Degrades,
            format!(
                "not cached — the first render fetches hyperframes@{} (~360 MB) through npx. \
                 Run `scripts/setup.sh` to pull it now instead.",
                crate::edit::render::HF_VERSION
            ),
        ),
        None => Finding::bad(
            "HyperFrames renderer",
            Impact::Breaks,
            "no npx on PATH and nothing cached — install Node (brew install node), \
             then run `scripts/setup.sh`."
                .to_string(),
        ),
    }
}

/// The uploader the distribute stage shells out to.
///
/// Still the one piece living in the `screencast` sibling. Degrades rather than
/// breaks: everything up to and including the render works without it.
fn uploader() -> Finding {
    let home = crate::distribute::screencast_home();
    if home
        .join("src/screencast/platforms/upload_cli.py")
        .is_file()
        && on_path("uv").is_some()
    {
        return Finding::ok("S3 uploader", format!("{}", home.display()));
    }
    if on_path("uv").is_none() {
        return Finding::bad(
            "S3 uploader",
            Impact::Degrades,
            "uv is not installed (brew install uv). Upload to S3 will fail; \
             everything before it works."
                .to_string(),
        );
    }
    Finding::bad(
        "S3 uploader",
        Impact::Degrades,
        format!(
            "nothing at {} — upload to S3 will fail; everything up to the render works \
             without it. Set SCREENCAST_HOME to a screencast checkout if you have one. \
             This is the last piece still living outside this repo.",
            home.display()
        ),
    )
}

/// Everything worth knowing before a render, cheapest first.
pub fn check() -> Vec<Finding> {
    vec![library(), renderer(), uploader()]
}

/// The startup line, or nothing at all when the machine is ready.
///
/// Silence on a healthy machine is the point: a banner printed every launch is
/// one nobody reads by the third day, which is exactly when it starts mattering.
pub fn warn_once() {
    let findings = check();
    // Only what will actually fail a render. A degraded dependency — no S3
    // uploader on a machine that never uploads — is a permanent state for some
    // people, and a banner printed at every launch about a permanent state is
    // one that gets skipped past by the third day, taking the real warnings
    // with it. `doctor` is where the full picture lives.
    let breaking: Vec<&Finding> = findings
        .iter()
        .filter(|f| !f.ok && f.impact == Impact::Breaks)
        .collect();
    if breaking.is_empty() {
        return;
    }
    let mut out = String::new();
    for finding in breaking {
        let _ = writeln!(
            out,
            "stream-recorder: {} — {}",
            finding.what, finding.detail
        );
    }
    let _ = write!(
        out,
        "stream-recorder: recording still works; run `saaga-social-flow doctor` for the rest."
    );
    eprintln!("{out}");
}

/// The `doctor` subcommand: every dependency, healthy ones included.
pub fn report(out: &mut impl std::io::Write) -> anyhow::Result<()> {
    writeln!(out, "Render dependencies\n")?;
    let findings = check();
    for finding in &findings {
        let mark = match (finding.ok, finding.impact) {
            (true, _) => "ok     ",
            (false, Impact::Breaks) => "BROKEN ",
            (false, Impact::Degrades) => "warn   ",
        };
        writeln!(out, "  {mark} {:<22} {}", finding.what, finding.detail)?;
    }

    let broken = findings
        .iter()
        .filter(|f| !f.ok && f.impact == Impact::Breaks)
        .count();
    let warned = findings
        .iter()
        .filter(|f| !f.ok && f.impact == Impact::Degrades)
        .count();
    writeln!(out)?;
    match (broken, warned) {
        (0, 0) => writeln!(out, "Everything a render needs is here.")?,
        (0, n) => writeln!(out, "Renders will work; {n} thing(s) degraded.")?,
        (n, _) => writeln!(out, "{n} dependency/dependencies will fail a render.")?,
    }
    writeln!(
        out,
        "\nCredentials are reported separately: `saaga-social-flow credentials`."
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The vendored library is in this repo, so this has to pass here — and it
    /// is the check that would have caught the private-sibling dependency.
    #[test]
    fn the_library_check_passes_in_this_checkout() {
        let found = library();
        assert!(found.ok, "library check failed: {}", found.detail);
    }

    /// A failure is only useful if it says what to do, so every unhealthy
    /// finding has to carry an instruction rather than just a diagnosis.
    #[test]
    fn every_unhealthy_finding_names_an_action() {
        for finding in check().into_iter().filter(|f| !f.ok) {
            // An imperative the reader can act on. Broad on purpose: the point
            // is that a message never stops at the diagnosis, not that it uses
            // one blessed verb.
            let says_how = [
                "Run ",
                "run `",
                "install",
                "brew ",
                "git checkout",
                "setup.sh",
                "Set ",
                "export ",
            ]
            .iter()
            .any(|hint| finding.detail.contains(hint));
            assert!(
                says_how,
                "{} says what is wrong but not what to do: {}",
                finding.what, finding.detail
            );
        }
    }

    /// Startup stays quiet about things that merely degrade — those are
    /// permanent states for some setups, and a banner every launch is one
    /// nobody reads by the time it matters.
    #[test]
    fn startup_only_speaks_up_about_hard_failures() {
        let findings = check();
        let breaking = findings
            .iter()
            .filter(|f| !f.ok && f.impact == Impact::Breaks)
            .count();
        assert_eq!(
            breaking, 0,
            "this checkout has a render-breaking dependency, so startup would warn"
        );
    }

    #[test]
    fn the_report_covers_every_finding() {
        let mut out = Vec::new();
        report(&mut out).expect("report writes");
        let text = String::from_utf8(out).expect("utf8");
        for finding in check() {
            assert!(text.contains(finding.what), "report omits {}", finding.what);
        }
    }
}
