//! The plan, while recording: the teleprompter, the layout New Chapter opens
//! in, and the record of which plan chapter each take was recorded for.
//!
//! The plan the recording follows is [`crate::plan::recording_plan`] — the
//! approved version, else the newest. Recording chapter *n* is plan chapter
//! *n*, and three things follow from that at the moment a chapter opens:
//!
//! - **The teleprompter** (the Record tab's Speaking notes) shows plan
//!   chapter *n*, marked as recording. Idle, it shows the chapter the next
//!   press will record, marked as up next.
//! - **The layout.** The chapter opens in plan chapter *n*'s layout. Idle, the
//!   next chapter's layout is made the current one, so the preview already
//!   shows what will record; mid-take, New Chapter opens the next chapter in
//!   it, through the same reopen a layout picked mid-take already uses. A
//!   layout picked by hand for that chapter wins, and a planned Split with no
//!   screen selected is not forced — the line says so instead.
//! - **The binding**, `chapter-NN.plan.json` beside the take (see
//!   [`crate::plan::binding`]), which is how the render gives HyperFrames the
//!   plan chapter's title, knows which chapter is the call to action, and
//!   hints the outline's points.
//!
//! Without a plan, none of this does anything: the deck or the placeholder
//! shows as before and the layout is whatever was picked.

use super::App;
use crate::layouts::{Layout, Pair};
use crate::plan::Plan;

/// What the plan says about a chapter's layout.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Planned {
    /// Record in this layout.
    Use(Pair),
    /// The plan wants this layout, but it needs a screen and none is selected.
    NoScreen(Pair),
}

/// The plan's layout for chapter `n`, judged against what can record now.
///
/// `None` when there is nothing to do: no plan, a chapter past its end, a
/// chapter with no layout, or one already chosen by hand (`hand` is the
/// chapter a hand-picked layout is for).
pub(super) fn planned(
    plan: Option<&Plan>,
    n: u32,
    hand: Option<u32>,
    orientation: crate::layouts::Orientation,
    has_screen: bool,
) -> Option<Planned> {
    if hand == Some(n) {
        return None;
    }
    let pair = plan?.body.chapter(n)?.layout?;
    if Layout::get(pair, orientation).needs_screen() && !has_screen {
        return Some(Planned::NoScreen(pair));
    }
    Some(Planned::Use(pair))
}

impl App {
    fn recording_plan(&self) -> Option<Plan> {
        crate::plan::recording_plan(&crate::plan::dir(&self.session))
    }

    /// The plan's layout for chapter `n` — see [`planned`].
    pub(super) fn planned_pair(&self, n: u32) -> Option<Planned> {
        planned(
            self.recording_plan().as_ref(),
            n,
            self.hand_layout,
            self.orientation,
            self.screen_uid.is_some(),
        )
    }

    /// The chapter the teleprompter should be on, and whether it is recording.
    fn prompter_chapter(&self) -> (u32, bool) {
        match self.router.as_ref() {
            Some(router) => (router.current_chapter_number(), true),
            None => (self.next_chapter, false),
        }
    }

    /// Draw the Record tab's teleprompter: the recording plan if there is one,
    /// on the chapter recording or up next. `false` when there is no plan, and
    /// the caller shows the deck instead.
    pub(super) fn show_plan_teleprompter(&self) -> bool {
        let Some(live) = self.live.as_ref() else {
            return false;
        };
        let Some(plan) = self.recording_plan() else {
            return false;
        };
        let (n, recording) = self.prompter_chapter();
        live.notes
            .show_page(&crate::ui::planning::teleprompter_page(&plan, n, recording));
        true
    }

    /// The plan changed — built, refined, edited, selected, approved. Redraw
    /// the teleprompter and, idle, take up the next chapter's planned layout.
    pub(super) fn recording_plan_changed(&mut self) {
        self.reload_deck();
        self.preselect_planned_layout();
    }

    /// Idle, the chapter the next press records changed — a Stop, the chapter
    /// menu, a version or project switch. Move the teleprompter to it and take
    /// up its planned layout.
    pub(super) fn next_chapter_changed(&mut self) {
        if self.router.is_some() {
            return;
        }
        if let Some(live) = self.live.as_ref() {
            live.notes.go_to_chapter(self.next_chapter, false);
        }
        self.preselect_planned_layout();
    }

    /// Idle only: make the next chapter's planned layout the current one, so
    /// the preview shows what the press will record. A planned layout that
    /// cannot record is reported on the Record tab's line and left alone.
    pub(super) fn preselect_planned_layout(&mut self) {
        if self.router.is_some() {
            return;
        }
        let n = self.next_chapter;
        match self.planned_pair(n) {
            Some(Planned::Use(pair)) if pair != self.pair => {
                self.pending_pair = None;
                self.pair = pair;
                println!(
                    "stream-recorder: chapter {n} is planned as {} — switching to it",
                    pair.as_str()
                );
                self.apply_layout_change();
            }
            Some(Planned::NoScreen(pair)) if pair != self.pair => {
                self.set_render_status(&no_screen_message(n, pair, self.pair));
            }
            _ => {}
        }
    }

    /// Mid-take: the layout New Chapter should open chapter `next` in, when
    /// the plan says one and nothing was picked by hand — `start_or_cut` hands
    /// it to the same reopen a layout picked mid-take uses.
    pub(super) fn planned_layout_for_cut(&self, next: u32) -> Option<Pair> {
        if self.pending_pair.is_some() {
            return None;
        }
        match self.planned_pair(next)? {
            Planned::Use(pair) if pair != self.pair => Some(pair),
            Planned::NoScreen(pair) if pair != self.pair => {
                self.set_render_status(&no_screen_message(next, pair, self.pair));
                None
            }
            _ => None,
        }
    }

    /// Chapter `n` just opened: record which plan chapter it is, and put that
    /// chapter on the teleprompter as recording. The hand-picked layout, if it
    /// was for this chapter, has done its job.
    pub(super) fn chapter_opened(&mut self, n: u32) {
        let plan = self.recording_plan();
        if let Err(err) = crate::plan::binding::bind(&self.session.dir, n, plan.as_ref()) {
            eprintln!("stream-recorder: could not record chapter {n:02}'s plan chapter: {err:#}");
        }
        if self.hand_layout.is_some_and(|hand| hand <= n) {
            self.hand_layout = None;
        }
        if let Some(live) = self.live.as_ref() {
            live.notes.go_to_chapter(n, true);
        }
    }
}

fn no_screen_message(n: u32, planned: Pair, current: Pair) -> String {
    format!(
        "Chapter {n} is planned as {}, which needs a screen — none is selected, so it \
         records as {}. Pick a screen to follow the plan.",
        planned.as_str(),
        current.as_str()
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::layouts::Orientation;
    use crate::plan::schema::{ChapterKind, PlanBody, PlanChapter};

    fn plan(layouts: &[Option<Pair>]) -> Plan {
        Plan {
            body: PlanBody {
                chapters: layouts
                    .iter()
                    .map(|&layout| PlanChapter {
                        kind: ChapterKind::Body,
                        title: "A chapter".into(),
                        goal: String::new(),
                        points: Vec::new(),
                        verbatim: None,
                        cues: Vec::new(),
                        show: String::new(),
                        layout,
                        est_seconds: None,
                        card: true,
                    })
                    .collect(),
                ..PlanBody::default()
            },
            ..Plan::default()
        }
    }

    const H: Orientation = Orientation::Horizontal;

    #[test]
    fn a_chapter_opens_in_its_planned_layout() {
        let plan = plan(&[Some(Pair::TalkingHead), Some(Pair::Outline)]);
        assert_eq!(
            planned(Some(&plan), 1, None, H, false),
            Some(Planned::Use(Pair::TalkingHead))
        );
        assert_eq!(
            planned(Some(&plan), 2, None, H, false),
            Some(Planned::Use(Pair::Outline)),
            "an outline chapter needs no screen"
        );
    }

    #[test]
    fn no_plan_no_layout_or_past_the_plan_changes_nothing() {
        let plan = plan(&[Some(Pair::Split), None]);
        assert_eq!(planned(None, 1, None, H, true), None);
        assert_eq!(planned(Some(&plan), 2, None, H, true), None);
        assert_eq!(planned(Some(&plan), 3, None, H, true), None);
    }

    #[test]
    fn a_layout_picked_by_hand_for_the_chapter_wins() {
        let plan = plan(&[Some(Pair::Split), Some(Pair::Split)]);
        assert_eq!(planned(Some(&plan), 2, Some(2), H, true), None);
        assert_eq!(
            planned(Some(&plan), 2, Some(1), H, true),
            Some(Planned::Use(Pair::Split)),
            "a hand pick for another chapter does not"
        );
    }

    #[test]
    fn a_planned_split_without_a_screen_is_reported_not_forced() {
        let plan = plan(&[Some(Pair::Split)]);
        assert_eq!(
            planned(Some(&plan), 1, None, H, false),
            Some(Planned::NoScreen(Pair::Split))
        );
        let message = no_screen_message(1, Pair::Split, Pair::TalkingHead);
        assert!(message.contains("planned as Split"), "{message}");
        assert!(message.contains("records as Talking Head"), "{message}");
    }
}
