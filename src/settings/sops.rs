//! The team's shared credentials, decrypted at startup from `dev.sops.env`.
//!
//! That file is committed to this repo with every value encrypted under the
//! saaga dev KMS key — the same arrangement `saaga`, `strapi-cms` and the other
//! repos use, and the reason a new machine needs no credential handover at all:
//! clone, `aws sso login --profile dev`, run.
//!
//! ## Why shelling out
//!
//! `sops` is a Go binary that already knows how to talk to KMS, refresh SSO
//! credentials and pick a store from a file suffix. Linking a Rust KMS client
//! and reimplementing the envelope format would add a dependency tree and a
//! second thing to keep in step with the `.sops.yaml` every other repo shares.
//! The cost is that `sops` has to be on `PATH`, which [`load`] treats as
//! "no shared credentials available" rather than an error — see below.
//!
//! ## Never fatal
//!
//! Every failure here is survivable: someone can be offline, have expired SSO
//! credentials, or not have `sops` installed, and still want to record a video.
//! So [`load`] reports what went wrong on stderr, leaves the environment alone,
//! and lets the Settings tab's own "not set" state carry the message. What it
//! must never do is take the app down at launch over a credential the user may
//! not even need this session.
//!
//! ## Precedence
//!
//! Loaded *last*, after every `.env` — so a personal key in
//! `~/.stream-recorder/.env`, or one exported in the shell, overrides the team
//! value rather than being silently replaced by it. See [`crate::load_dotenv`].

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;

/// The committed file, and the AWS profile whose KMS key it is encrypted under.
///
/// Both come from `.sops.yaml`, which is the authority — this constant only has
/// to agree with the `dev` rule there.
const FILE: &str = "dev.sops.env";
const PROFILE: &str = "dev";

/// What `sops` handed back this run, for the Settings pane to attribute values
/// to the team file rather than to something the reader typed.
///
/// Empty when the decrypt did not happen, which is indistinguishable from "the
/// file had nothing in it" and does not need to be distinguished: either way no
/// live value came from here.
static PROVIDED: OnceLock<BTreeMap<String, String>> = OnceLock::new();

/// The committed team file, if this checkout has one.
///
/// Only a real checkout: a release ships a bare binary, and there is no
/// `dev.sops.env` beside it to decrypt. Those machines fall back to the
/// per-user `.env` the Settings tab writes.
pub fn path() -> Option<PathBuf> {
    let candidates = [
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(FILE),
        std::env::current_dir().ok()?.join(FILE),
    ];
    candidates.into_iter().find(|p| p.is_file())
}

/// Values decrypted from the team file this run.
pub fn provided() -> &'static BTreeMap<String, String> {
    PROVIDED.get_or_init(BTreeMap::new)
}

/// Decrypt `dev.sops.env` and fold it into the process environment.
///
/// Only sets a variable that is not already set, so everything loaded before it
/// wins — that is what makes a personal override possible.
///
/// Returns the number of variables it contributed. Zero covers every reason it
/// might not have: no checkout, no `sops`, no credentials.
pub fn load() -> usize {
    let Some(path) = path() else { return 0 };
    let decrypted = match decrypt(&path) {
        Ok(text) => text,
        Err(reason) => {
            // One line, and specific enough to act on. The app carries on:
            // recording does not need a Strapi token.
            eprintln!(
                "stream-recorder: could not read the team credentials in {} — {reason}\n\
                 stream-recorder: continuing with local settings only.",
                path.display()
            );
            return 0;
        }
    };

    let values = crate::settings::parse(&decrypted);
    let mut applied = 0;
    for (key, value) in &values {
        if value.trim().is_empty() {
            continue;
        }
        if std::env::var_os(key).is_none() {
            // SAFETY: called from `load_dotenv` at the top of `main`, before any
            // thread that reads the environment has been spawned.
            unsafe { std::env::set_var(key, value) };
            applied += 1;
        }
    }
    let _ = PROVIDED.set(values);
    applied
}

/// Run `sops --decrypt`, turning a failure into a sentence worth reading.
///
/// No `--input-type`: the `.env` suffix is what tells sops to use the dotenv
/// store, which is the convention the other saaga repos rely on too.
fn decrypt(path: &Path) -> Result<String, String> {
    let output = Command::new("sops")
        .arg("--decrypt")
        .arg(path)
        // `.sops.yaml` names the profile per rule, but sops only consults that
        // when *creating* a file. Decrypting reads the key arn baked into the
        // file itself and resolves credentials the ordinary way, so the profile
        // has to be handed over here or an unset AWS_PROFILE picks `default`.
        .env(
            "AWS_PROFILE",
            std::env::var("AWS_PROFILE").as_deref().unwrap_or(PROFILE),
        )
        .output()
        .map_err(|err| match err.kind() {
            std::io::ErrorKind::NotFound => "sops is not installed (brew install sops)".to_string(),
            _ => format!("could not run sops: {err}"),
        })?;

    if output.status.success() {
        return String::from_utf8(output.stdout)
            .map_err(|_| "sops returned something that is not text".to_string());
    }

    let stderr = String::from_utf8_lossy(&output.stderr);
    Err(explain(&stderr))
}

/// Turn sops' own diagnostics into the action that fixes them.
///
/// sops leads with "Failed to get the data key required to decrypt the SOPS
/// file", which is true of every failure and actionable for none of them; the
/// cause is further down, indented under the key that failed. So this matches
/// against the whole output and only falls back to a line when nothing is
/// recognised.
fn explain(stderr: &str) -> String {
    let lower = stderr.to_lowercase();

    // Ordered by how specific the fix is, not by how common the failure is: an
    // expired session and a missing grant both mention the profile, and telling
    // someone to re-run `aws sso login` when they simply have no access wastes
    // the one instruction they will actually follow.
    if lower.contains("accessdenied") || lower.contains("not authorized") {
        return "your AWS account has no grant on the saaga dev KMS key — \
             ask for kms:Decrypt on it, then retry"
            .to_string();
    }
    if lower.contains("expired") || lower.contains("sso session") || lower.contains("invalid_grant")
    {
        return format!("your AWS session has expired — run `aws sso login --profile {PROFILE}`");
    }
    // What an unconfigured or logged-out machine actually produces:
    // "could not load AWS config: failed to get shared config profile, dev".
    if lower.contains("failed to get shared config profile")
        || lower.contains("could not load aws config")
        || lower.contains("no such profile")
        || lower.contains("could not find profile")
    {
        return format!(
            "no usable `{PROFILE}` AWS profile — run `aws sso login --profile {PROFILE}` \
             (or add the profile if this machine has never had it)"
        );
    }
    if lower.contains("nocredentialproviders") || lower.contains("unable to locate credentials") {
        return format!("no AWS credentials — run `aws sso login --profile {PROFILE}`");
    }

    // Unrecognised. The indented cause lines carry the detail; sops marks them
    // with `|`, the first one prefixed by the `- ` of the key's bullet, so both
    // shapes have to be recognised to reach the actual message.
    let detail = stderr
        .lines()
        .map(str::trim)
        .filter_map(|line| line.strip_prefix("- ").or(Some(line)))
        .filter_map(|line| line.strip_prefix('|'))
        .map(str::trim)
        .find(|line| !line.is_empty());
    if let Some(detail) = detail {
        return detail.to_string();
    }
    stderr
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .unwrap_or("sops failed with no output")
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Verbatim stderr from `sops --decrypt` on a machine with no usable AWS
    /// profile — the case a teammate hits before their first `aws sso login`.
    /// sops' own headline says only "Failed to get the data key", so matching
    /// that alone would send them nowhere.
    const NO_PROFILE: &str = "\
Failed to get the data key required to decrypt the SOPS file.

Group 0: FAILED
  arn:aws:kms:us-east-1:827913618293:key/mrk-5c773050b08947a68267662e3a59fe94||dev: FAILED
    - | could not load AWS config: failed to get shared config
      | profile, dev

Recovery failed because no master key was able to decrypt the file.";

    #[test]
    fn the_logged_out_case_names_the_login_command() {
        let said = explain(NO_PROFILE);
        assert!(said.contains("aws sso login"), "{said}");
        assert!(
            !said.contains("Failed to get the data key"),
            "led with the useless headline: {said}"
        );
    }

    /// The two failures a person actually hits should each name their fix,
    /// because "AccessDeniedException" on its own sends someone to the wrong
    /// place — usually to re-copying a key that was never the problem.
    #[test]
    fn the_common_failures_name_the_command_that_fixes_them() {
        let expired =
            explain("error: ExpiredToken: The security token included in the request is expired");
        assert!(expired.contains("aws sso login"), "{expired}");

        let denied =
            explain("AccessDeniedException: User is not authorized to perform kms:Decrypt");
        assert!(denied.contains("grant"), "{denied}");
        assert!(
            !denied.contains("sso login"),
            "sent them to log in when they lack access: {denied}"
        );

        let missing = explain("NoCredentialProviders: no valid providers in chain");
        assert!(missing.contains("aws sso login"), "{missing}");
    }

    /// An unrecognised failure still has to say something specific rather than
    /// swallowing the reason — the indented cause beats the generic headline.
    #[test]
    fn an_unknown_failure_surfaces_its_cause_line() {
        let odd = explain("Failed to get the data key.\n\n  - | something odd happened\n");
        assert_eq!(odd, "something odd happened");
        assert_eq!(explain(""), "sops failed with no output");
    }

    /// The team file is a normal dotenv once decrypted, so the parser the rest
    /// of settings uses has to handle it — including the `sops_*` metadata rows
    /// that only appear in the encrypted form.
    #[test]
    fn decrypted_output_parses_as_dotenv() {
        let parsed = crate::settings::parse("OPENROUTER_API_KEY=sk-or-v1-x\nS3_BUCKET=media\n");
        assert_eq!(
            parsed.get("OPENROUTER_API_KEY").map(String::as_str),
            Some("sk-or-v1-x")
        );
        assert_eq!(parsed.get("S3_BUCKET").map(String::as_str), Some("media"));
    }
}
