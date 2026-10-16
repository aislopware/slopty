// slopty: Slopty's own file, beside zeron's question panel.
//! What an approval shows and how it may be answered (Claude desktop's rules): a long, unknown
//! or destructive call starts on its deny and takes no key, a sandbox-off command is read
//! through before it can be allowed, every shown string draws hidden characters as visible
//! escapes, an edit shows its change, and a plan's choices stand under "Plan ready".
use gpui::AnyElement;

use super::*;

/// The note under an approval that must be read through before it can be allowed.
pub(crate) const READ_GATE_NOTE: &str = "Scroll through the whole command to allow it.";
/// The title a plan's choices stand under; the plan itself is a card in the transcript.
pub(crate) const PLAN_READY: &str = "Plan ready";
/// The most lines of a proposed change the panel draws.
const PROPOSED_MAX_LINES: usize = 80;
/// The tallest a read-gated body grows before it scrolls.
pub(super) const GATED_BODY_MAX_HEIGHT: f32 = 160.0;

/// A character that would hide or reorder what the person reads: controls (but line breaks and
/// tabs), bidi overrides and isolates, default-ignorables and noncharacters.
fn hidden(c: char) -> bool {
    let u = c as u32;
    (c.is_control() && c != '\n' && c != '\t')
        || matches!(
            u,
            0x00AD
                | 0x034F
                | 0x061C
                | 0x115F
                | 0x1160
                | 0x17B4
                | 0x17B5
                | 0x180B..=0x180F
                | 0x200B..=0x200F
                | 0x202A..=0x202E
                | 0x2060..=0x206F
                | 0x3164
                | 0xFDD0..=0xFDEF
                | 0xFE00..=0xFE0F
                | 0xFEFF
                | 0xFFA0
                | 0xFFF0..=0xFFF8
                | 0xE0000..=0xE0FFF
        )
        || (u & 0xFFFE) == 0xFFFE
}

/// `text` with every hidden character drawn as a visible escape, `[U+202E]`.
pub(crate) fn visible_escapes(text: &str) -> String {
    if !text.chars().any(hidden) {
        return text.to_owned();
    }
    text.chars()
        .map(|c| if hidden(c) { format!("[U+{:04X}]", c as u32) } else { c.to_string() })
        .collect()
}

/// Whether `question` is a plan's choices: the transcript carries the plan itself as a card
/// (`plan-card:<id>`, its text part `plan:<id>`), so the panel asks only for the choice.
pub(crate) fn is_plan(question: &UserInputQuestion, transcript: &[SessionMessageEntry]) -> bool {
    let headed = question.header.trim().eq_ignore_ascii_case("plan");
    headed
        && transcript.iter().any(|entry| {
            entry.id.starts_with("plan-card:")
                || entry.parts.iter().any(|part| {
                    matches!(part, MessagePart::Text { id, .. } if id.starts_with("plan:"))
                })
        })
}

/// How many requests the chat holds open: each unresolved input part of its last answer.
pub(crate) fn open_requests(transcript: &[SessionMessageEntry]) -> usize {
    transcript
        .iter()
        .rev()
        .find(|entry| entry.role == MessageRole::Assistant)
        .map_or(0, |entry| {
            entry
                .parts
                .iter()
                .filter(|part| matches!(part, MessagePart::Input { resolved: false, .. }))
                .count()
        })
}

/// The change an edit's approval would make, as the changes pane draws a file, with a label
/// that says it in words for the accessibility tree.
pub(super) fn proposed_change(diff: &slopty_view_proto::ToolDiff, theme: &Theme) -> AnyElement {
    let mut file = crate::transcript::diff_to_file(diff);
    crate::changes::truncate_file_lines(&mut file, PROPOSED_MAX_LINES);
    let lines: Vec<String> = file
        .hunks
        .iter()
        .flat_map(|hunk| &hunk.lines)
        .filter_map(|line| match line.kind {
            crate::changes::LineKind::Add => Some(format!("+ {}", line.text.trim_end())),
            crate::changes::LineKind::Del => Some(format!("− {}", line.text.trim_end())),
            _ => None,
        })
        .take(24)
        .map(|line| visible_escapes(&line))
        .collect();
    let name = diff.path.rsplit('/').next().unwrap_or(&diff.path).to_owned();
    let label = format!("Proposed change to {}: {}", visible_escapes(&name), lines.join("; "));
    div()
        .id("approval-proposed-change")
        .role(Role::Group)
        .aria_label(SharedString::from(label))
        .mt(px(10.0))
        .rounded(px(8.0))
        .border_1()
        .border_color(crate::theme::hairline(0.08))
        .overflow_hidden()
        .flex()
        .flex_col()
        .child(
            div()
                .h(px(26.0))
                .px(px(10.0))
                .flex()
                .items_center()
                .gap(px(8.0))
                .bg(crate::theme::ink(0.03))
                .text_size(crate::typography::ui_rems(11.5))
                .text_color(theme.text_muted)
                .child(div().min_w_0().truncate().child(SharedString::from(visible_escapes(&diff.path))))
                .child(
                    div()
                        .flex_none()
                        .text_color(theme.success)
                        .child(SharedString::from(format!("+{}", file.additions))),
                )
                .child(
                    div()
                        .flex_none()
                        .text_color(theme.danger)
                        .child(SharedString::from(format!("−{}", file.deletions))),
                ),
        )
        .child(
            div()
                .id("approval-proposed-change-body")
                .max_h(px(220.0))
                .overflow_y_scroll()
                .child(crate::changes::render_file_body_with_syntax(&file, None, theme)),
        )
        .into_any_element()
}

impl Composer {
    /// Mark the open approval read through once its gated body has been scrolled to its end (or
    /// fits whole); then its allow takes a click.
    pub(super) fn note_read_through(&mut self) -> bool {
        let scroll = &self.extras.wizard_scroll;
        if scroll.bounds().size.height <= px(0.0) {
            return false;
        }
        let max = scroll.max_offset().y;
        let at = -scroll.offset().y;
        let fits = max <= px(1.0);
        if let Some(wizard) = self.wizard.as_mut()
            && !wizard.read_through
            && (fits || at >= max - px(2.0))
        {
            wizard.read_through = true;
        }
        true
    }

    /// A gated body is measured only once drawn: look again a frame later, so a body that fits
    /// whole needs no scroll.
    pub(super) fn recheck_read_through(&mut self, cx: &mut Context<Self>) {
        if self.note_read_through() || self.extras.gate_task.is_some() {
            return;
        }
        self.extras.gate_task = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(Duration::from_millis(32)).await;
            this.update(cx, |composer, cx| {
                composer.extras.gate_task = None;
                cx.notify();
            })
            .ok();
        }));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn approval(len: usize) -> UserInputQuestion {
        UserInputQuestion {
            id: "choice".into(),
            header: "Approval".into(),
            question: format!("Allow Claude to run {}?", "x".repeat(len)),
            options: vec!["Allow once".into(), "Always allow".into(), "Deny".into()],
            choices_only: true,
            deny_first: len > 600,
            click_only: len > 600,
            deny_option: Some(2),
            ..Default::default()
        }
    }

    fn labels(step: WizardStep) -> Vec<String> {
        match step {
            WizardStep::Done(answers) => answers.into_iter().flat_map(|a| a.labels).collect(),
            other => panic!("not answered: {other:?}"),
        }
    }

    #[test]
    fn a_long_command_defaults_to_deny_and_ignores_enter() {
        let mut wizard = Wizard::new("req".into(), vec![approval(700)]);
        assert_eq!(wizard.focused, Some(2), "focus starts on Deny");
        assert_eq!(wizard.press_enter(), WizardStep::Stay, "Enter allows nothing");
        assert_eq!(wizard.press_number(1), WizardStep::Stay, "1 allows nothing");
        assert!(!wizard.page_has_pick());
        assert_eq!(labels(wizard.select(0)), ["Allow once"], "a click does");
    }

    #[test]
    fn a_short_command_takes_its_keys_and_enter_picks_the_focused_choice() {
        let mut wizard = Wizard::new("req".into(), vec![approval(20)]);
        assert_eq!(wizard.focused, Some(0));
        wizard.move_focus(1);
        assert_eq!(wizard.focused, Some(1));
        assert_eq!(labels(wizard.press_enter()), ["Always allow"]);
        let mut wizard = Wizard::new("req".into(), vec![approval(20)]);
        assert_eq!(labels(wizard.press_number(3)), ["Deny"]);
    }

    #[test]
    fn a_read_gated_command_is_allowed_only_once_read_through() {
        let mut question = approval(20);
        question.read_gate = true;
        let mut wizard = Wizard::new("req".into(), vec![question.clone()]);
        assert!(wizard.allow_blocked(0));
        assert_eq!(wizard.select(0), WizardStep::Stay, "the allow is disabled");
        assert!(!wizard.allow_blocked(2), "the deny never is");
        wizard.read_through = true;
        assert_eq!(labels(wizard.select(0)), ["Allow once"]);
        let mut wizard = Wizard::new("req".into(), vec![question]);
        assert_eq!(labels(wizard.select(2)), ["Deny"]);
    }

    #[test]
    fn a_click_only_question_takes_no_digit_and_no_enter() {
        let mut question = approval(20);
        question.click_only = true;
        let mut wizard = Wizard::new("req".into(), vec![question]);
        assert_eq!(wizard.press_number(1), WizardStep::Stay);
        assert_eq!(wizard.press_enter(), WizardStep::Stay);
        assert_eq!(labels(wizard.select(1)), ["Always allow"]);
    }

    #[test]
    fn an_approval_shows_bidi_overrides_as_visible_escapes() {
        assert_eq!(visible_escapes("rm -rf /\u{202E}gnp.exe"), "rm -rf /[U+202E]gnp.exe");
        assert_eq!(visible_escapes("a\u{200B}b\u{0007}c\u{FEFF}"), "a[U+200B]b[U+0007]c[U+FEFF]");
        assert_eq!(visible_escapes("two\nlines\tand tab"), "two\nlines\tand tab");
        assert_eq!(visible_escapes("plain é 日本"), "plain é 日本");
    }

    #[test]
    fn a_plan_is_its_card_and_counts_its_open_requests() {
        let mut question = approval(10);
        question.header = "Plan".into();
        let card = SessionMessageEntry {
            id: "plan-card:p1".into(),
            role: MessageRole::Assistant,
            parts: vec![MessagePart::Text { id: "plan:p1".into(), text: "1. Do".into() }],
            created_at: 0,
            device_id: "d".into(),
            status: None,
            continuation_of: None,
            duration_ms: None,
        };
        assert!(!is_plan(&question, &[]), "no card: the panel keeps the plan's words");
        assert!(is_plan(&question, std::slice::from_ref(&card)));
        let mut asks = card.clone();
        asks.id = "a".into();
        asks.parts = (0..2)
            .map(|ix| MessagePart::Input {
                id: format!("ask:{ix}"),
                request_id: ix.to_string(),
                questions: Vec::new(),
                resolved: false,
            })
            .collect();
        assert_eq!(open_requests(&[card, asks]), 2);
    }
}
