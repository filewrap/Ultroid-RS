//! Board-only assistant command and reusable rich creation exports.
//!
//! Port of `assistant/rich_creations.py`, which itself re-exports
//! from `pyUltroid/_core/assistant_creations.py`:
//!
//! * `/chess` (owner-only `asst_cmd(pattern="chess")`) — renders only
//!   the complete starting chess position.
//! * `CREATIONS` — the five reusable rich templates the showcase
//!   indexes: `work-plan`, `decision-brief`, `meeting-agenda`,
//!   `learning-path`, `project-review`.
//! * `chessboard_html()` — the starting position as an eight-row
//!   native HTML chessboard (light `#f0d9b5` / dark `#b58863`
//!   squares, 28px pieces), copied exactly from the core source.
//!
//! The five creation templates are block-level native payloads in
//! Python (`InputRichMessage` blocks). Core `RichDoc` carries
//! markdown / HTML / legacy text blocks, not PageBlock trees, so
//! each template is rendered here as the equivalent HTML document
//! (same headings, tables, checklists and controls as text); the
//! precise native block construction lives with the core rich port,
//! not this assistant module.

use ultroid_core::error::Result;
use ultroid_core::rich::RichDoc;
use ultroid_core::{CommandDef, Ctx, Registry};

use crate::cbs::{def, reg_add};

/// The five reusable creation templates: `(id, label, description)`.
/// (Python `CREATIONS` also carries a builder per entry; the HTML
/// rendering of each is `creation_html` below.)
pub const CREATIONS: &[(&str, &str, &str)] = &[
    (
        "work-plan",
        "Focused work plan",
        "Table, details, URL and copy controls",
    ),
    (
        "decision-brief",
        "Decision brief",
        "Comparison table, open details, controls",
    ),
    (
        "meeting-agenda",
        "Meeting agenda",
        "Checklist, schedule table, details, controls",
    ),
    (
        "learning-path",
        "Learning path",
        "Nested sections, code prompt and controls",
    ),
    (
        "project-review",
        "Project review",
        "Review table, checklist, details and controls",
    ),
];

/// Starting position as an eight-row native HTML chessboard
/// (Python `chessboard_html`, byte-for-byte the same markup).
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
    let mut out = String::from(
        "<table role=\"presentation\" cellpadding=\"8\" cellspacing=\"0\" style=\"border-collapse:collapse;font-size:28px;text-align:center\">",
    );
    for (r, rank) in ranks.iter().enumerate() {
        out.push_str("<tr>");
        for (c, piece) in rank.chars().enumerate() {
            let bg = if (r + c) % 2 == 0 {
                "#f0d9b5"
            } else {
                "#b58863"
            };
            out.push_str(&format!(
                "<td style=\"background:{bg};min-width:32px\">{piece}</td>"
            ));
        }
        out.push_str("</tr>");
    }
    out.push_str("</table>");
    out
}

/// HTML rendering of one creation template (same sections as the
/// Python block builder: heading, table/checklist body, and a
/// resources/details footer).
pub fn creation_html(sample_id: &str) -> Option<String> {
    let (title, body) = match sample_id {
        "work-plan" => (
            "Focused work plan",
            "<p>One goal, three moves, one owner per move.</p><table class=\"bordered compact\"><tr><th>Move</th><th>Owner</th><th>Done</th></tr><tr><td>Define the outcome</td><td>You</td><td>[ ]</td></tr><tr><td>Do the smallest shippable slice</td><td>You</td><td>[ ]</td></tr><tr><td>Review and decide next</td><td>You</td><td>[ ]</td></tr></table>",
        ),
        "decision-brief" => (
            "Decision brief",
            "<p>Options, trade-offs, and the call.</p><table class=\"bordered compact\"><tr><th>Option</th><th>Upside</th><th>Risk</th></tr><tr><td>A — ship now</td><td>Fast feedback</td><td>Rough edges</td></tr><tr><td>B — polish first</td><td>Clean launch</td><td>Slow feedback</td></tr></table><blockquote expandable><b>Decision</b>\nShip the smallest version, then polish what users touch.</blockquote>",
        ),
        "meeting-agenda" => (
            "Meeting agenda",
            "<p>Checklist first, schedule second.</p><p>[x] Read the brief<br>[ ] Bring the numbers<br>[ ] Decide the owner</p><table class=\"bordered compact\"><tr><th>Time</th><th>Item</th></tr><tr><td>00–05</td><td>Check-in</td></tr><tr><td>05–20</td><td>Decisions</td></tr><tr><td>20–30</td><td>Next actions</td></tr></table>",
        ),
        "learning-path" => (
            "Learning path",
            "<p>Nested sections, one prompt per step.</p><blockquote expandable><b>Step 1 — foundations</b>\nRead the core guide, then reproduce the smallest example.</blockquote><blockquote expandable><b>Step 2 — practice</b>\n<code>Build the same thing twice: once guided, once from memory.</code></blockquote>",
        ),
        "project-review" => (
            "Project review",
            "<p>Progress, risk, decision, next action — then resources.</p><table class=\"bordered compact\"><tr><th>Area</th><th>Status</th></tr><tr><td>Progress</td><td>On track</td></tr><tr><td>Risk</td><td>Low</td></tr><tr><td>Decision</td><td>Continue</td></tr></table><p>Next action / owner: [ ]</p>",
        ),
        _ => return None,
    };
    Some(format!("<h1>{title}</h1><hr>{body}"))
}

/// The creation payload as a `RichDoc` (Python `creation_payload`).
pub fn creation_payload(sample_id: &str) -> Option<Result<RichDoc>> {
    creation_html(sample_id).map(RichDoc::html)
}

/// `/chess` — render only the complete starting chess position
/// (owner-only, bot client: rich with plain-text fallback).
pub async fn chess(ctx: &Ctx) -> Result<()> {
    let doc = RichDoc::html(chessboard_html())?;
    ctx.send_rich_or_plain(&doc).await?;
    Ok(())
}

pub fn register(registry: &mut Registry) {
    reg_add(
        registry,
        CommandDef {
            owner_only: true,
            ..def(
                "chess",
                "chess",
                "Render only the complete starting chess position.",
            )
        },
        |ctx| Box::pin(chess(ctx)),
    );
}
