//! The pause between ticking a post and it going out.
//!
//! Approving a post is the decision to send it, and an approved post is sent
//! with `shareNow` — public within seconds, and Buffer cannot take it back. So
//! a tick does not send on the spot: it starts [`GRACE`], every further tick
//! or untick starts it again, and when it runs out whatever is still ticked is
//! sent. A misclick is an untick away from nothing.
//!
//! The countdown belongs to one project. Switching to another while it runs
//! drops it, so ticks made in one project can never send from the next.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// How long a tick waits before it sends.
pub const GRACE: Duration = Duration::from_secs(10);

#[derive(Debug, Default)]
pub struct Countdown {
    armed: Option<(Instant, PathBuf)>,
}

impl Countdown {
    /// Starts, or starts again, the wait for the project at `root`.
    pub fn arm(&mut self, now: Instant, root: &Path) {
        self.armed = Some((now + GRACE, root.to_path_buf()));
    }

    pub fn cancel(&mut self) {
        self.armed = None;
    }

    pub fn is_armed(&self) -> bool {
        self.armed.is_some()
    }

    /// True once, when the wait has run out for the project at `root`. A
    /// countdown armed for another project is dropped, never fired.
    pub fn take_due(&mut self, now: Instant, root: &Path) -> bool {
        match &self.armed {
            Some((_, armed_root)) if armed_root != root => {
                self.armed = None;
                false
            }
            Some((deadline, _)) if now >= *deadline => {
                self.armed = None;
                true
            }
            _ => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn it_fires_once_after_the_grace_and_not_before() {
        let root = Path::new("/rec/2026-10-09_15-00-00");
        let start = Instant::now();
        let mut countdown = Countdown::default();
        assert!(!countdown.take_due(start + GRACE, root), "never armed");
        countdown.arm(start, root);
        assert!(!countdown.take_due(start + GRACE - Duration::from_millis(1), root));
        assert!(countdown.take_due(start + GRACE, root));
        assert!(!countdown.take_due(start + GRACE * 2, root), "only once");
        assert!(!countdown.is_armed());
    }

    /// Every tick restarts the wait, so the last change gets the full grace.
    #[test]
    fn another_tick_starts_the_wait_again() {
        let root = Path::new("/rec/a");
        let start = Instant::now();
        let mut countdown = Countdown::default();
        countdown.arm(start, root);
        countdown.arm(start + Duration::from_secs(8), root);
        assert!(!countdown.take_due(start + GRACE, root));
        assert!(countdown.take_due(start + Duration::from_secs(8) + GRACE, root));
    }

    #[test]
    fn a_cancel_or_a_project_switch_sends_nothing() {
        let start = Instant::now();
        let mut countdown = Countdown::default();
        countdown.arm(start, Path::new("/rec/a"));
        countdown.cancel();
        assert!(!countdown.take_due(start + GRACE, Path::new("/rec/a")));

        countdown.arm(start, Path::new("/rec/a"));
        assert!(!countdown.take_due(start + GRACE, Path::new("/rec/b")));
        assert!(!countdown.is_armed(), "dropped, not kept for later");
        assert!(!countdown.take_due(start + GRACE, Path::new("/rec/a")));
    }
}
