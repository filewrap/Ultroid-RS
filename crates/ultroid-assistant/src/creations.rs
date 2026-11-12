//! Reusable native RichMessage creations for assistant examples and
//! commands.
//!
//! Port of `pyUltroid/_core/assistant_creations.py`. The Python module
//! builds Telethon TL `PageBlock` trees (`PageBlockTable`,
//! `PageBlockDetails`, `PageBlockButtonRow`, …) and returns them as an
//! `InputRichMessage(blocks=…)` payload. This crate has no grammers/TL
//! types — the contract forbids them — and `RichDoc::blocks` only carries
//! plain paragraph lines, so each template is rendered into the faithful
//! Markdown equivalent of the same content: bold headings, Markdown
//! tables, checklists, fenced code blocks and links. The two interactive
//! controls in the Python templates degrade as follows, because the
//! `InputRichMessage` forms `grammers` 0.12 exposes carry no button rows:
//!
//! * URL buttons ("Telegram docs") become Markdown links.
//! * Copy buttons become fenced code blocks holding the exact copy text.
//!
//! Details sections render as bold section titles; their open/closed
//! state has no Markdown equivalent and is not preserved.

use ultroid_core::error::{Error, Result};
use ultroid_core::rich::RichDoc;

/// One catalog template: id, title, description — the `CREATIONS` tuple
/// from the Python module, in the same order.
#[derive(Debug, Clone, Copy)]
pub struct Creation {
    pub id: &'static str,
    pub title: &'static str,
    pub description: &'static str,
}

/// The five reusable templates.
pub const CREATIONS: &[Creation] = &[
    Creation {
        id: "work-plan",
        title: "Focused work plan",
        description: "Table, details, URL and copy controls",
    },
    Creation {
        id: "decision-brief",
        title: "Decision brief",
        description: "Comparison table, open details, controls",
    },
    Creation {
        id: "meeting-agenda",
        title: "Meeting agenda",
        description: "Checklist, schedule table, details, controls",
    },
    Creation {
        id: "learning-path",
        title: "Learning path",
        description: "Nested sections, code prompt and controls",
    },
    Creation {
        id: "project-review",
        title: "Project review",
        description: "Review table, checklist, details and controls",
    },
];

/// The "Optional resources" footer shared by all five templates: the
/// Telegram-docs URL control and the copy control, rendered as a link plus
/// a fenced code block holding the exact copy text.
fn resources_section(copy_label: &str, copy_text: &str) -> String {
    format!(
        "**Optional resources**\n\n**Resources and reusable controls**\n\n[Telegram docs](https://telegram.org)\n\n**{copy_label}**\n\n```text\n{copy_text}\n```"
    )
}

fn rich_plan() -> String {
    format!(
        "**Focused work plan**\n\nA reusable template; fill in the blanks before acting.\n\n| Stage | Prompt |\n| --- | --- |\n| Goal | What outcome matters? |\n| Next | What is the smallest useful step? |\n\n**Planning worksheet**\n\nDefine a measurable finish line and a timebox.\n\n```text\nGoal: [outcome]\nNext action: [verb + object]\nReview at: [time]\n```\n\n{}",
        resources_section("Copy plan", "Goal: [outcome]\nNext action: [verb + object]")
    )
}

fn decision_brief() -> String {
    format!(
        "**Decision brief**\n\nA neutral structure for comparing options; no recommendation is prefilled.\n\n| Criterion | Option A | Option B |\n| --- | --- | --- |\n| Benefits | [list] | [list] |\n| Risks | [list] | [list] |\n\n**Evaluation checklist**\n\nCheck reversibility, evidence quality, and who is affected.\n\nWrite what would change your mind.\n\n{}",
        resources_section(
            "Copy checklist",
            "Benefits • Risks • Evidence • Reversibility"
        )
    )
}

fn meeting_agenda() -> String {
    format!(
        "**Meeting agenda**\n\nTemplate only — add attendees, timing, and decisions before sharing.\n\n- [ ] State the desired outcome\n- [ ] Review context and constraints\n- [ ] Agree owner and next action\n\n| Minutes | Topic |\n| --- | --- |\n| 5 | Purpose and context |\n| 10 | Discussion |\n| 5 | Decision and actions |\n\n**Notes template**\n\nCapture decision, owner, due date, and unresolved question.\n\n{}",
        resources_section(
            "Copy action format",
            "Action: [task] | Owner: [name] | Due: [date]"
        )
    )
}

fn learning_path() -> String {
    format!(
        "**Learning path**\n\nA practical study template, not a claim about your progress.\n\n**1 · Set a target**\n\nWrite one question you want to answer.\n\n```text\nQuestion: [specific question]\nI will know I understand when: [test]\n```\n\n| Session | Focus |\n| --- | --- |\n| Learn | One concept and source |\n| Practice | One small exercise |\n\n**2 · Learn and explain**\n\nRead one reliable source, then explain the idea in your own words.\n\n**3 · Practice and review**\n\nTry a small exercise and note what remains unclear.\n\n{}",
        resources_section(
            "Copy study prompt",
            "Teach-back: explain [idea] simply, then give one example."
        )
    )
}

fn project_review() -> String {
    format!(
        "**Project review**\n\nA reusable review card. Values below are prompts, not live project state.\n\n| Area | Prompt |\n| --- | --- |\n| Progress | What changed since last review? |\n| Risk | What could block the next milestone? |\n| Decision | What needs an owner or choice? |\n\n**Review checklist**\n\nSummarize evidence, not optimism. Separate confirmed facts from assumptions.\n\n- [ ] Name one next action\n- [ ] Assign an owner and check-in\n\n{}",
        resources_section(
            "Copy review",
            "Progress: [ ]\nRisk: [ ]\nDecision: [ ]\nNext action / owner: [ ]"
        )
    )
}

/// Return one catalog template as a native rich document (Python
/// `creation_payload(sample_id)`). Unknown ids raise the Rust equivalent
/// of Python's `KeyError`: [`Error::NotFound`].
pub fn creation_payload(sample_id: &str) -> Result<RichDoc> {
    let markdown = match sample_id {
        "work-plan" => rich_plan(),
        "decision-brief" => decision_brief(),
        "meeting-agenda" => meeting_agenda(),
        "learning-path" => learning_path(),
        "project-review" => project_review(),
        other => {
            return Err(Error::NotFound(format!(
                "no rich creation with id `{other}`"
            )));
        }
    };
    RichDoc::markdown(markdown)
}

/// The starting position as an eight-row native HTML chessboard (Python
/// `chessboard_html`), byte-for-byte the same markup: a presentation
/// table, `8px` cell padding, `28px` pieces, light squares `#f0d9b5`,
/// dark squares `#b58863`.
pub fn chessboard_html() -> String {
    let ranks = [
        "♜♞♝♛♚♝♞♜",
        "♟♟♟♟♟♟♟♟",
        "········",
        "········",
        "········",
        "········",
        "♙♙♙♙♙♙♙♙",
        "♖♘♗♕♔♗♘♖",
    ];
    let mut board = String::from(
        "<table role=\"presentation\" cellpadding=\"8\" cellspacing=\"0\" style=\"border-collapse:collapse;font-size:28px;text-align:center\">",
    );
    for (r, rank) in ranks.iter().enumerate() {
        board.push_str("<tr>");
        for (c, piece) in rank.chars().enumerate() {
            let bg = if (r + c) % 2 == 0 {
                "#f0d9b5"
            } else {
                "#b58863"
            };
            board.push_str(&format!(
                "<td style=\"background:{bg};min-width:32px\">{piece}</td>"
            ));
        }
        board.push_str("</tr>");
    }
    board.push_str("</table>");
    board
}

/// [`chessboard_html`] as a rich document.
pub fn chessboard_doc() -> Result<RichDoc> {
    RichDoc::html(chessboard_html())
}
