//! `ultroid-rich` — native rich messages, **bot client only**.
//!
//! Port of `pyUltroid/_core/richmsg.py` + `cards.py` + `buttonkit.py`.
//! The sending path is the grammers 0.12 high-level one, verified in
//! GRAMMERS-RESEARCH.md §9: `InputMessage::rich_message(InputRichMessage)`
//! via `Client::send_message`. `RichSender` holds ONLY a bot-token client
//! (`bot_sign_in`); there is no constructor from a user client, and the
//! user-client `Responder::send_rich` refuses with `Error::RichOnlyBot`.
//! Rich builder forms (the only ones grammers 0.12 has): `html` /
//! `markdown` + `.photo(id, …)` / `.document(id, …)` media binding, and
//! legacy `PageBlock` blocks. Never invent `InputRichBlock` types — they
//! do not exist in grammers 0.12.

use grammers_client::message::{Button, InputMessage, InputRichMessage, ReplyMarkup};
use grammers_client::message::Message;
use grammers_client::session::types::PeerRef;
use grammers_client::{Client, InvocationError, tl};
use grammers_mtsender::RpcError;
use ultroid_core::error::{Error, Result};
use ultroid_core::rich::{RichDoc, RichMediaKind};
use ultroid_core::tg::{ButtonAction, InlineButton};

/// Grammers invocation result (not the core `Result`).
type InvokeResult<T> = std::result::Result<T, InvocationError>;

fn rich_invalid() -> InvocationError {
    InvocationError::Rpc(RpcError {
        code: 400,
        name: "RICH_MESSAGE_INVALID".to_string(),
        value: None,
        caused_by: None,
    })
}

pub mod cards;

/// Convert a core `RichDoc` into a grammers `InputRichMessage`.
/// Exactly one of markdown / html / blocks, as in `InputRichMessage`.
pub fn to_input_rich(doc: &RichDoc) -> Result<InputRichMessage> {
    if doc.is_empty() {
        return Err(Error::Other("cannot send an empty rich message".into()));
    }
    let mut rich = if let Some(md) = &doc.markdown {
        InputRichMessage::markdown(md)
    } else if let Some(html) = &doc.html {
        InputRichMessage::html(html)
    } else {
        // Legacy PageBlock blocks: the Python code's `blocks()` path.
        // Each line becomes a plain text block; never invent 10.2 block types.
        let blocks: Vec<tl::enums::PageBlock> = doc
            .blocks
            .iter()
            .map(|line| {
                tl::enums::PageBlock::Paragraph(tl::types::PageBlockParagraph {
                    text: tl::enums::RichText::TextPlain(tl::types::TextPlain {
                        text: line.clone(),
                    }),
                })
            })
            .collect();
        InputRichMessage::blocks(blocks)
    };
    if doc.rtl {
        rich = rich.rtl();
    }
    if doc.no_autolink {
        rich = rich.noautolink();
    }
    for media in &doc.media {
        let id = media.id.to_string();
        match media.kind {
            RichMediaKind::Photo => {
                rich = rich.photo(
                    id,
                    tl::enums::InputPhoto::Photo(tl::types::InputPhoto {
                        id: media.id,
                        access_hash: media.access_hash,
                        file_reference: media.file_reference.clone(),
                    }),
                );
            }
            RichMediaKind::Video | RichMediaKind::Audio | RichMediaKind::Document => {
                rich = rich.document(
                    id,
                    tl::enums::InputDocument::Document(tl::types::InputDocument {
                        id: media.id,
                        access_hash: media.access_hash,
                        file_reference: media.file_reference.clone(),
                    }),
                );
            }
        }
    }
    Ok(rich)
}

/// Convert core inline-button rows into a grammers `ReplyMarkup`.
/// Only bot accounts can use reply markup (Telegram ignores it from users).
pub fn to_reply_markup(rows: &[Vec<InlineButton>]) -> ReplyMarkup {
    let button_rows: Vec<Vec<Button>> = rows
        .iter()
        .map(|row| {
            row.iter()
                .map(|b| match &b.action {
                    ButtonAction::Data(data) => Button::data(b.text.clone(), data.clone()),
                    ButtonAction::Url(url) => Button::url(b.text.clone(), url.clone()),
                    ButtonAction::SwitchInline(q) => {
                        Button::switch(b.text.clone(), q.clone())
                    }
                    ButtonAction::SwitchInlineElsewhere(q) => {
                        Button::switch_elsewhere(b.text.clone(), q.clone())
                    }
                    ButtonAction::WebApp(url) => Button::webview(b.text.clone(), url.clone()),
                    ButtonAction::Copy(text) => Button::data(b.text.clone(), text.as_bytes().to_vec()),
                })
                .collect()
        })
        .collect();
    ReplyMarkup::from_buttons(&button_rows)
}

/// The bot-only rich sender.
///
/// Construct ONLY from a client logged in with a bot token
/// (`client.bot_sign_in(token, api_hash)`). Passing a user-account client
/// is a caller bug: the server may reject rich from user accounts (and
/// programmatic user rich may require Premium — unverified), so the type
/// carries no user-client constructor at all.
#[derive(Clone)]
pub struct RichSender {
    client: Client,
}

impl RichSender {
    /// Wrap a bot client. The caller guarantees `client` was authorised
    /// via `bot_sign_in`.
    pub fn new_bot(client: Client) -> Self {
        RichSender { client }
    }

    pub fn client(&self) -> &Client {
        &self.client
    }

    /// Send a rich document to `peer` (Bot API `sendRichMessage` equivalent
    /// over MTProto). Returns the sent message.
    pub async fn send_rich<C: Into<PeerRef>>(
        &self,
        peer: C,
        doc: &RichDoc,
    ) -> InvokeResult<Message> {
        let rich = to_input_rich(doc).map_err(|_| rich_invalid())?;
        self.client
            .send_message(peer, InputMessage::new().rich_message(rich))
            .await
    }

    /// Edit a message into a rich document (Bot API `editMessageText` +
    /// rich equivalent over MTProto).
    pub async fn edit_rich<C: Into<PeerRef>>(
        &self,
        peer: C,
        message_id: i32,
        doc: &RichDoc,
    ) -> InvokeResult<()> {
        let rich = to_input_rich(doc).map_err(|_| rich_invalid())?;
        self.client
            .edit_message(peer, message_id, InputMessage::new().rich_message(rich))
            .await?;
        Ok(())
    }

    /// Live rich draft streaming (the ~30 s ephemeral preview).
    /// Hand-built: grammers 0.12 has no high-level draft helper. Uses the
    /// layer-233 TL type `InputSendMessageRichMessageDraftAction` via
    /// `messages.SetTyping`, mirroring the Python draft flow. `random_id`
    /// identifies the stream: the same id animates, a new id replaces.
    pub async fn send_rich_draft<C: Into<PeerRef>>(
        &self,
        peer: C,
        doc: &RichDoc,
        random_id: i64,
        can_stop: bool,
    ) -> InvokeResult<()> {
        let rich = to_input_rich(doc).map_err(|_| rich_invalid())?;
        let peer_input: tl::enums::InputPeer = peer.into().into();
        self.client
            .invoke(&tl::functions::messages::SetTyping {
                peer: peer_input,
                top_msg_id: None,
                action: tl::enums::SendMessageAction::InputSendMessageRichMessageDraftAction(
                    tl::types::InputSendMessageRichMessageDraftAction {
                        can_stop,
                        keep_on_stop: false,
                        random_id,
                        rich_message: rich.raw,
                    },
                ),
            })
            .await?;
        Ok(())
    }

    /// Fresh random id for a draft stream.
    pub fn new_draft_id() -> i64 {
        rand::random::<i64>()
    }
}
