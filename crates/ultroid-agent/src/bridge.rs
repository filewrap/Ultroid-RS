//! The User-to-Bot bridge that lets the agent ask the user something —
//! port of `pyUltroid/agent/_bridge.py`.
//!
//! The agent runs on the *User* client but Telegram will not deliver a
//! `CallbackQuery` for a message the User account sent itself. So a
//! keyboard the agent draws has to be sent by the *Bot* client, and the
//! answer has to come back to the User side. That round trip is this module.
//!
//! A pending question is an [`AskSession`]: one random token, one
//! oneshot channel, one TTL watchdog. The token goes into the callback
//! data, so a tap resolves exactly the future that asked, and nothing
//! else. The watchdog is what stops a question that is never answered
//! from leaking a future — the thing that would otherwise pin a tool
//! call open forever.
//!
//! Nothing here imports the agent package. This is the transport; the
//! tool that uses it lives in `crate::interactive`.
//!
//! Telegram sending is abstracted behind [`BotSender`]: this module
//! must not depend on grammers directly (that wiring lives in the
//! binary crate later).

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use serde_json::{Value, json};
use tokio::sync::{Mutex, oneshot};
use ultroid_core::error::{Error, Result};

use crate::telegram::{ButtonCell, ButtonSpec, inline_rows, text_of};

/// Callback data is capped at 64 bytes by Telegram, so the prefix is
/// short and the token carries the rest. A per-chat salt is folded in
/// so a token minted for one chat cannot be replayed against another.
const PREFIX: &str = "ask:";
/// Default time-to-live for an unanswered question.
const TTL_SECS: f64 = 120.0;

/// The callback-data prefix identifying this bridge's asks in *chat_id*.
pub fn answer_prefix(chat_id: i64) -> Vec<u8> {
    format!("{PREFIX}{chat_id}:").into_bytes()
}

/// The minimal bot-side send capability the bridge needs. The binary
/// crate implements this against its real Telegram client.
#[async_trait]
pub trait BotSender: Send + Sync {
    /// Send *text* to *chat_id* with an optional inline keyboard.
    /// Returns the sent message id.
    async fn send_message(
        &self,
        chat_id: i64,
        text: &str,
        buttons: Option<Vec<Vec<ButtonSpec>>>,
    ) -> Result<i64>;
}

/// A question that ended without the user answering.
///
/// Returned instead of raising so the calling tool can hand the model a
/// structured "nobody answered" fact and let it decide what to do,
/// instead of the loop dying on a timeout.
#[derive(Debug, Clone)]
pub struct NoAnswer {
    pub reason: String,
    pub waited: f64,
}

impl Default for NoAnswer {
    fn default() -> Self {
        NoAnswer {
            reason: "timeout".to_string(),
            waited: 0.0,
        }
    }
}

impl NoAnswer {
    pub fn as_dict(&self) -> Value {
        json!({
            "answered": false,
            "reason": self.reason,
            "waited": (self.waited * 1000.0).round() / 1000.0,
        })
    }
}

/// The result of one question: answered, or explicitly not answered.
#[derive(Debug, Clone)]
pub struct AskOutcome {
    pub question: String,
    pub value: Option<String>,
    pub timed_out: bool,
    pub reason: String,
    pub waited: f64,
    pub answer_id: i64,
}

impl AskOutcome {
    pub fn answered(&self) -> bool {
        self.value.is_some() && !self.timed_out
    }

    /// The shape the model sees. Mirrors Hermes' `clarify` result.
    pub fn as_dict(&self) -> Value {
        let mut payload = json!({
            "question": self.question,
            "answered": self.answered(),
            "user_response": if self.answered() { self.value.clone().map(Value::String).unwrap_or(Value::Null) } else { Value::Null },
            "waited": (self.waited * 1000.0).round() / 1000.0,
        });
        if !self.answered() {
            payload["timed_out"] = Value::Bool(true);
            payload["reason"] = Value::String(if self.reason.is_empty() {
                "timeout".to_string()
            } else {
                self.reason.clone()
            });
        }
        payload
    }
}

/// One outstanding question and the channel that will carry its answer.
pub struct AskSession {
    pub token: String,
    pub question: String,
    pub chat_id: i64,
    pub created: Instant,
    pub ttl: Duration,
    pub watchdog: Option<tokio::task::JoinHandle<()>>,
    pub message_id: i64,
    pub answer_id: i64,
    pub choices: Vec<String>,
    sender: Option<oneshot::Sender<AskOutcome>>,
}

impl AskSession {
    fn new(
        token: String,
        question: String,
        chat_id: i64,
        ttl: Duration,
        choices: Vec<String>,
    ) -> (Self, oneshot::Receiver<AskOutcome>) {
        let (tx, rx) = oneshot::channel();
        (
            AskSession {
                token,
                question,
                chat_id,
                created: Instant::now(),
                ttl,
                watchdog: None,
                message_id: 0,
                answer_id: 0,
                choices,
                sender: Some(tx),
            },
            rx,
        )
    }

    pub fn data_prefix(&self) -> Vec<u8> {
        format!("{PREFIX}{}:{}:", self.chat_id, self.token).into_bytes()
    }

    pub fn elapsed(&self) -> f64 {
        self.created.elapsed().as_secs_f64()
    }

    /// Resolve the future as unanswered, exactly once.
    pub fn expire(&mut self, reason: &str) {
        if let Some(handle) = self.watchdog.take() {
            handle.abort();
        }
        if let Some(tx) = self.sender.take() {
            let _ = tx.send(AskOutcome {
                question: self.question.clone(),
                value: None,
                timed_out: true,
                reason: reason.to_string(),
                waited: self.elapsed(),
                answer_id: self.answer_id,
            });
        }
    }

    /// Resolve the future with *value*. `false` when already settled.
    pub fn resolve(&mut self, value: &str) -> bool {
        if let Some(handle) = self.watchdog.take() {
            handle.abort();
        }
        match self.sender.take() {
            Some(tx) => {
                let _ = tx.send(AskOutcome {
                    question: self.question.clone(),
                    value: Some(value.to_string()),
                    timed_out: false,
                    reason: String::new(),
                    waited: self.elapsed(),
                    answer_id: self.answer_id,
                });
                true
            }
            None => false,
        }
    }

    /// The callback data a button carrying *value* must send.
    pub fn callback_for(&self, value: &str) -> Vec<u8> {
        let mut data = self.data_prefix();
        data.extend_from_slice(format!("{}:{value}", self.answer_id).as_bytes());
        data.truncate(64);
        data
    }

    /// Start the TTL watchdog on a shared session.
    ///
    /// After `ttl` the session is expired unless it was already settled
    /// and retired. (`asyncio.TimerHandle` in Python; here an aborted
    /// task, cancelled by [`AskSession::expire`]/[`AskSession::resolve`]
    /// and by retire.)
    pub async fn arm(session: Arc<Mutex<AskSession>>) {
        let ttl = session.lock().await.ttl;
        let weak = Arc::downgrade(&session);
        let handle = tokio::spawn(async move {
            tokio::time::sleep(ttl).await;
            if let Some(session) = weak.upgrade() {
                let mut guard = session.lock().await;
                guard.expire("timeout");
            }
        });
        session.lock().await.watchdog = Some(handle);
    }
}

/// Sends a question on the Bot, resolves the answer on the User.
///
/// One bridge per chat is enough; the pending map is keyed by token, so
/// concurrent questions in different chats never collide, and two
/// questions in the same chat are distinguished by their token.
pub struct AskBridge {
    bot: Option<Arc<dyn BotSender>>,
    user: Option<Arc<dyn BotSender>>,
    pending: Arc<Mutex<HashMap<String, Arc<Mutex<AskSession>>>>>,
    pub ttl: Duration,
    pub default_style: String,
}

impl Default for AskBridge {
    fn default() -> Self {
        Self::new()
    }
}

impl AskBridge {
    pub fn new() -> Self {
        AskBridge {
            bot: None,
            user: None,
            pending: Arc::new(Mutex::new(HashMap::new())),
            ttl: Duration::from_secs_f64(TTL_SECS),
            default_style: "primary".to_string(),
        }
    }

    pub fn with_ttl(mut self, ttl: Duration) -> Self {
        self.ttl = ttl;
        self
    }

    // ── wiring ──

    /// The bot client, once attached. `None` until `attach()` runs —
    /// there is no global client registry inside this crate for the
    /// lazy `_first_client` lookup to read.
    pub fn bot(&self) -> Option<Arc<dyn BotSender>> {
        self.bot.clone()
    }

    pub fn user(&self) -> Option<Arc<dyn BotSender>> {
        self.user.clone()
    }

    /// How many questions are outstanding. Diagnostics and leak checks.
    pub async fn pending(&self) -> usize {
        self.pending.lock().await.len()
    }

    /// Bind the clients explicitly, once they exist.
    pub fn attach(&mut self, bot: Option<Arc<dyn BotSender>>, user: Option<Arc<dyn BotSender>>) {
        if bot.is_some() {
            self.bot = bot;
        }
        if user.is_some() {
            self.user = user;
        }
    }

    // ── asking ──

    /// Put *question* to the user and wait for the answer.
    ///
    /// Returns an [`AskOutcome`]; on expiry `answered` is `false`
    /// rather than an exception, because a silent user is a normal
    /// outcome the model must be able to reason about. A free-text
    /// reply is handled by [`AskBridge::submit_text`] — the caller
    /// forwards the next DM in the chat, so the tool can offer buttons
    /// *and* accept typing.
    #[allow(clippy::too_many_arguments)]
    pub async fn ask(
        &self,
        chat_id: i64,
        question: &str,
        choices: Vec<String>,
        note: &str,
        ttl: Option<Duration>,
        style: Option<&str>,
        icon: Option<&str>,
        columns: usize,
        send: bool,
    ) -> Result<AskOutcome> {
        let choices: Vec<String> = choices.into_iter().map(|c| c.to_string()).collect();
        let (session, rx) = self.new_session(chat_id, question, choices, ttl);
        let token = session.token.clone();
        let session = Arc::new(Mutex::new(session));
        self.pending
            .lock()
            .await
            .insert(token.clone(), Arc::clone(&session));

        let result = async {
            if send {
                self.send_keyboard(&session, note, style, icon, columns)
                    .await?;
            }
            AskSession::arm(Arc::clone(&session)).await;
            match rx.await {
                Ok(outcome) => Ok(outcome),
                Err(_) => {
                    // The sender was dropped without a value (the
                    // session was retired unanswered): treat it as a
                    // cancellation, mirroring Python's CancelledError path.
                    let waited = session.lock().await.elapsed();
                    Ok(AskOutcome {
                        question: question.to_string(),
                        value: None,
                        timed_out: true,
                        reason: "cancelled".to_string(),
                        waited,
                        answer_id: 0,
                    })
                }
            }
        }
        .await;

        self.retire_by_token(&token).await;
        result
    }

    fn new_session(
        &self,
        chat_id: i64,
        question: &str,
        choices: Vec<String>,
        ttl: Option<Duration>,
    ) -> (AskSession, oneshot::Receiver<AskOutcome>) {
        // `secrets.token_urlsafe(9)` in Python: 9 random bytes, urlsafe
        // base64, no padding (12 characters). The token space makes a
        // collision astronomically rare; the pending map is keyed by
        // token, so a duplicate would simply overwrite (Python retries
        // in a loop, which an async map cannot do synchronously here).
        let bytes: [u8; 9] = rand::random();
        let token = base64_url_nopad(&bytes);
        AskSession::new(
            token,
            question.to_string(),
            chat_id,
            ttl.unwrap_or(self.ttl),
            choices,
        )
    }

    async fn send_keyboard(
        &self,
        session: &Arc<Mutex<AskSession>>,
        note: &str,
        style: Option<&str>,
        icon: Option<&str>,
        columns: usize,
    ) -> Result<()> {
        let Some(bot) = self.bot.clone() else {
            return Err(Error::Other(
                "AskBridge has no bot client; call attach() first".to_string(),
            ));
        };
        let (chat_id, question, choices) = {
            let s = session.lock().await;
            (s.chat_id, s.question.clone(), s.choices.clone())
        };
        let body = if note.is_empty() {
            question
        } else {
            format!("{question}\n\n{note}")
        };
        let buttons: Option<Vec<Vec<ButtonSpec>>> = if choices.is_empty() {
            None
        } else {
            let cols = columns.max(1);
            let mut cells: Vec<Vec<ButtonCell>> = Vec::new();
            for chunk in choices.chunks(cols) {
                let mut row: Vec<ButtonCell> = Vec::new();
                for label in chunk {
                    let data = session.lock().await.callback_for(label);
                    row.push(ButtonCell::Spec(ButtonSpec {
                        text: label.clone(),
                        data: Some(data),
                        url: None,
                        style: None,
                        icon: None,
                    }));
                }
                cells.push(row);
            }
            Some(inline_rows(
                cells,
                style.or(Some(&self.default_style)),
                icon,
            )?)
        };
        let sent_id = bot.send_message(chat_id, &body, buttons).await?;
        session.lock().await.message_id = sent_id;
        Ok(())
    }

    // ── answering ──

    /// Resolve the session named by a button's callback data.
    ///
    /// Returns the outcome when this bridge owned the tap, `None` when
    /// the data belongs to something else — which is the common case,
    /// since every other inline button in the bot shares the same
    /// dispatcher.
    pub async fn resolve_callback(&self, data: &[u8], edit: bool) -> Option<AskOutcome> {
        let _ = edit; // Accepted for call-site parity; unused in Python too.
        let session = self.session_for(data).await?;
        let outcome = {
            let mut guard = session.lock().await;
            let value = String::from_utf8_lossy(data)
                .rsplit(':')
                .next()
                .unwrap_or("")
                .to_string();
            guard.resolve(&value);
            AskOutcome {
                question: guard.question.clone(),
                value: Some(value),
                timed_out: false,
                reason: String::new(),
                waited: guard.elapsed(),
                answer_id: guard.answer_id,
            }
        };
        let token = session.lock().await.token.clone();
        self.retire_by_token(&token).await;
        Some(outcome)
    }

    /// Resolve the oldest open question in *chat_id* with typed text.
    ///
    /// Button taps arrive in order through
    /// [`AskBridge::resolve_callback`]; a typed reply has no token, so
    /// the oldest question in that chat is the one it answers. Returns
    /// `None` when the chat has no open question.
    pub async fn submit_text(&self, chat_id: i64, text: &Value) -> Option<AskOutcome> {
        let session = self.oldest_in(chat_id).await?;
        let outcome = {
            let mut guard = session.lock().await;
            let value = text_of(text).trim().to_string();
            guard.resolve(&value);
            AskOutcome {
                question: guard.question.clone(),
                value: Some(value),
                timed_out: false,
                reason: String::new(),
                waited: guard.elapsed(),
                answer_id: guard.answer_id,
            }
        };
        let token = session.lock().await.token.clone();
        self.retire_by_token(&token).await;
        Some(outcome)
    }

    async fn session_for(&self, data: &[u8]) -> Option<Arc<Mutex<AskSession>>> {
        let text = String::from_utf8_lossy(data).to_string();
        let parts: Vec<&str> = text.split(':').collect();
        // ask:<chat>:<token>[:<answer>:<value>]
        if parts.len() < 3 || parts[0] != PREFIX.trim_end_matches(':') {
            return None;
        }
        let token = parts[2];
        let session = self.pending.lock().await.get(token).cloned()?;
        let prefix = session.lock().await.data_prefix();
        if !data.starts_with(&prefix) {
            return None;
        }
        Some(session)
    }

    async fn oldest_in(&self, chat_id: i64) -> Option<Arc<Mutex<AskSession>>> {
        let candidates: Vec<Arc<Mutex<AskSession>>> =
            self.pending.lock().await.values().cloned().collect();
        let mut best: Option<(Instant, Arc<Mutex<AskSession>>)> = None;
        for session in candidates {
            let (cid, created) = {
                let guard = session.lock().await;
                (guard.chat_id, guard.created)
            };
            if cid == chat_id {
                match &best {
                    Some((t, _)) if *t <= created => {}
                    _ => best = Some((created, session)),
                }
            }
        }
        best.map(|(_, s)| s)
    }

    async fn retire_by_token(&self, token: &str) {
        let session = self.pending.lock().await.remove(token);
        if let Some(session) = session {
            let mut guard = session.lock().await;
            if let Some(handle) = guard.watchdog.take() {
                handle.abort();
            }
            // Drop any still-open sender so a blocked `ask()` returns
            // instead of hanging: expiry already resolved it in the
            // normal paths.
            guard.sender.take();
        }
    }

    // ── housekeeping ──

    /// Expire every open question. Returns how many were settled.
    pub async fn cancel_all(&self, reason: &str) -> usize {
        let sessions: Vec<Arc<Mutex<AskSession>>> = {
            let mut pending = self.pending.lock().await;
            let sessions: Vec<_> = pending.values().cloned().collect();
            pending.clear();
            sessions
        };
        let mut count = 0;
        for session in sessions {
            let mut guard = session.lock().await;
            if guard.sender.is_some() {
                guard.expire(reason);
                count += 1;
            }
            if let Some(handle) = guard.watchdog.take() {
                handle.abort();
            }
        }
        count
    }

    /// Whether *data* is one of this bridge's buttons.
    pub async fn owns(&self, data: &[u8]) -> bool {
        self.session_for(data).await.is_some()
    }
}

fn base64_url_nopad(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let mut out = String::new();
    for chunk in bytes.chunks(3) {
        let b = [
            chunk[0],
            *chunk.get(1).unwrap_or(&0),
            *chunk.get(2).unwrap_or(&0),
        ];
        let n = ((b[0] as u32) << 16) | ((b[1] as u32) << 8) | (b[2] as u32);
        out.push(ALPHABET[(n >> 18) as usize & 63] as char);
        out.push(ALPHABET[(n >> 12) as usize & 63] as char);
        if chunk.len() > 1 {
            out.push(ALPHABET[(n >> 6) as usize & 63] as char);
        }
        if chunk.len() > 2 {
            out.push(ALPHABET[n as usize & 63] as char);
        }
    }
    out
}

// ── the process-wide transport ────────────────────────────────────────────
// One bridge, because the half that asks and the half that resolves are in
// different modules: a tool handler on the agent side, a callback handler
// on the assistant side. Two instances would mean two pending maps and a
// tap that answers nothing.
//
// This is shared mutable state, and deliberately so — it is a transport
// keyed by token, the same trade InstantInline's session registry already
// makes, not a service locator hiding configuration. The clients are
// attached explicitly once they exist.

/// The process-wide [`AskBridge`], created on first use.
pub fn shared_bridge() -> Arc<Mutex<AskBridge>> {
    static BRIDGE: std::sync::OnceLock<Arc<Mutex<AskBridge>>> = std::sync::OnceLock::new();
    BRIDGE
        .get_or_init(|| Arc::new(Mutex::new(AskBridge::new())))
        .clone()
}
