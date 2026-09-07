//! A line diff, so a proposed prompt is reviewed as a change rather than as a wall.
//!
//! Reviewing a rewrite by reading the whole new preamble is how a subtle deletion
//! gets approved: forty lines look right, and the one rule that quietly vanished
//! is invisible. What matters is the delta.
//!
//! Longest-common-subsequence over lines, which is O(n·m) — fine at prompt scale
//! (tens of lines) and not worth a dependency. Unchanged runs are elided to a few
//! lines of context so a one-line change reads as a one-line change.

/// One line's fate between two versions.
#[derive(Debug, Clone, PartialEq)]
pub enum Change {
    Same(String),
    Added(String),
    Removed(String),
}

/// Unchanged lines kept either side of a change.
const CONTEXT: usize = 2;

/// The line-by-line changes from `before` to `after`.
pub fn changes(before: &str, after: &str) -> Vec<Change> {
    let old: Vec<&str> = before.lines().collect();
    let new: Vec<&str> = after.lines().collect();

    // table[i][j] = length of the LCS of old[i..] and new[j..]
    let mut table = vec![vec![0usize; new.len() + 1]; old.len() + 1];
    for i in (0..old.len()).rev() {
        for j in (0..new.len()).rev() {
            table[i][j] = if old[i] == new[j] {
                table[i + 1][j + 1] + 1
            } else {
                table[i + 1][j].max(table[i][j + 1])
            };
        }
    }

    let mut out = Vec::new();
    let (mut i, mut j) = (0, 0);
    while i < old.len() && j < new.len() {
        if old[i] == new[j] {
            out.push(Change::Same(old[i].to_string()));
            i += 1;
            j += 1;
        } else if table[i + 1][j] >= table[i][j + 1] {
            out.push(Change::Removed(old[i].to_string()));
            i += 1;
        } else {
            out.push(Change::Added(new[j].to_string()));
            j += 1;
        }
    }
    out.extend(old[i..].iter().map(|line| Change::Removed(line.to_string())));
    out.extend(new[j..].iter().map(|line| Change::Added(line.to_string())));
    out
}

/// `(added, removed)` line counts.
pub fn counts(before: &str, after: &str) -> (usize, usize) {
    let changes = changes(before, after);
    (
        changes.iter().filter(|c| matches!(c, Change::Added(_))).count(),
        changes
            .iter()
            .filter(|c| matches!(c, Change::Removed(_)))
            .count(),
    )
}

/// A short label for a row: "+3 −1 lines", or "no change".
pub fn summary(before: &str, after: &str) -> String {
    match counts(before, after) {
        (0, 0) => "no change".to_string(),
        (added, removed) => format!("+{added} −{removed} lines"),
    }
}

/// The diff as text, with unchanged runs elided.
pub fn render(before: &str, after: &str) -> String {
    let changes = changes(before, after);
    if changes.iter().all(|c| matches!(c, Change::Same(_))) {
        return "  (identical to the prompt in force)\n".to_string();
    }

    // Which unchanged lines are near enough a change to be worth showing.
    let keep: Vec<bool> = (0..changes.len())
        .map(|index| {
            let lo = index.saturating_sub(CONTEXT);
            let hi = (index + CONTEXT + 1).min(changes.len());
            changes[lo..hi]
                .iter()
                .any(|change| !matches!(change, Change::Same(_)))
        })
        .collect();

    let mut out = String::new();
    let mut elided = 0usize;
    for (index, change) in changes.iter().enumerate() {
        if matches!(change, Change::Same(_)) && !keep[index] {
            elided += 1;
            continue;
        }
        if elided > 0 {
            out.push_str(&format!("  … {elided} unchanged line(s) …\n"));
            elided = 0;
        }
        match change {
            Change::Same(line) => out.push_str(&format!("  {line}\n")),
            Change::Removed(line) => out.push_str(&format!("- {line}\n")),
            Change::Added(line) => out.push_str(&format!("+ {line}\n")),
        }
    }
    if elided > 0 {
        out.push_str(&format!("  … {elided} unchanged line(s) …\n"));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identical_text_has_no_changes() {
        let text = "one\ntwo\nthree";
        assert_eq!(counts(text, text), (0, 0));
        assert_eq!(summary(text, text), "no change");
        assert!(render(text, text).contains("identical to the prompt in force"));
    }

    #[test]
    fn an_added_line_is_marked_and_the_rest_kept() {
        let out = render("one\ntwo", "one\nmiddle\ntwo");
        assert!(out.contains("+ middle"));
        assert!(out.contains("  one"));
        assert!(out.contains("  two"));
        assert!(!out.contains("- "));
        assert_eq!(counts("one\ntwo", "one\nmiddle\ntwo"), (1, 0));
    }

    /// The case this exists for: a rule quietly disappearing from a long prompt.
    #[test]
    fn a_removed_rule_is_visible_rather_than_buried() {
        let before = "Be concise.\nMax 280 characters on Twitter.\nUse a hook.";
        let after = "Be concise.\nUse a hook.";
        let out = render(before, after);
        assert!(out.contains("- Max 280 characters on Twitter."));
        assert_eq!(counts(before, after), (0, 1));
        assert_eq!(summary(before, after), "+0 −1 lines");
    }

    #[test]
    fn a_replaced_line_reads_as_a_removal_and_an_addition() {
        let out = render("hook: punchy", "hook: curious");
        assert!(out.contains("- hook: punchy"));
        assert!(out.contains("+ hook: curious"));
        assert_eq!(summary("hook: punchy", "hook: curious"), "+1 −1 lines");
    }

    /// A one-line change in a long prompt must read as a one-line change.
    #[test]
    fn distant_unchanged_lines_are_elided() {
        let before: String = (0..40)
            .map(|n| format!("rule {n}\n"))
            .collect();
        let after = before.replace("rule 20", "rule twenty");
        let out = render(&before, &after);
        assert!(out.contains("- rule 20"));
        assert!(out.contains("+ rule twenty"));
        assert!(out.contains("unchanged line(s)"), "the rest is elided");
        // Context either side survives.
        assert!(out.contains("  rule 19"));
        assert!(out.contains("  rule 21"));
        // Far-away lines do not.
        assert!(!out.contains("  rule 0\n"));
        assert!(out.lines().count() < 20, "a wall would be 40+ lines");
    }

    #[test]
    fn writing_a_prompt_from_nothing_is_all_additions() {
        assert_eq!(counts("", "one\ntwo"), (2, 0));
        let out = render("", "one\ntwo");
        assert!(out.contains("+ one"));
        assert!(out.contains("+ two"));
    }

    #[test]
    fn blanking_a_prompt_is_all_removals() {
        assert_eq!(counts("one\ntwo", ""), (0, 2));
    }

    #[test]
    fn changes_preserve_order() {
        let got = changes("a\nb\nc", "a\nx\nc");
        assert_eq!(
            got,
            vec![
                Change::Same("a".into()),
                Change::Removed("b".into()),
                Change::Added("x".into()),
                Change::Same("c".into()),
            ]
        );
    }

    /// A rewrite that only reorders is still a change, and must not read as none.
    #[test]
    fn reordering_shows_as_a_move() {
        let (added, removed) = counts("one\ntwo", "two\none");
        assert!(added > 0 && removed > 0);
        assert_ne!(summary("one\ntwo", "two\none"), "no change");
    }
}
