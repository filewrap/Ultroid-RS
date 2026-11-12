# Ultroid → Rust (grammers) — Design

Source: https://github.com/mahesh953-hub/UltroidRewrite @ `5d09531` (SarahUB,
141,267 LOC Python / 103,811 excl. tests; 102 plugins). Target:
https://github.com/filewrap/Ultroid-RS.git. Stack: grammers-client /
grammers-session / grammers-mtsender **=0.12.0** (TL layer 233), tokio
multi-thread, edition 2024. See `../GRAMMERS-RESEARCH.md` (verified 2026-11-12)
and `../INVENTORY.md`.

## Workspace layout

```
ultroid-rs/                        # Cargo workspace (this repo)
  Cargo.toml                       # workspace + shared deps
  DESIGN.md  README.md  .gitignore
  crates/
    ultroid-rs/          # binary: main.rs — pools, auth, update loop, wiring
    ultroid-core/        # config, error, db, strings, router, ctx, listen, rich data
    ultroid-rich/        # RichDoc -> grammers InputRichMessage; bot-only RichSender
    ultroid-plugins/     # all 102 plugins, one module per Python file
    ultroid-assistant/   # assistant-bot handlers (assistant/*.py)
    ultroid-vc/          # voice chat: grammers phone.* bridge + ntgcalls-direct engine (3.0.7)
    ultroid-agent/       # AI agent providers/tools/mini (pyUltroid/agent/*)
    ultroid-web/         # setup server (pyUltroid/webserver/* + server.py)
```

## Locked rules (user, 2026-11-12)

1. TL layer stays 233: pin `grammers-client` / `grammers-session` /
   `grammers-mtsender` / `grammers-tl-types` `=0.12.0` exactly. Never
   downgrade to 230, never mix 0.11/0.12 stacks.
2. No complete as-is: nothing is copied as-is from the Python repo —
   everything is ported into idiomatic Rust, including assets/config
   (strings are ported to Rust source, not dumped as YAML).
3. New functions first: when a module needs new grammers-native
   functions/helpers, implement those first, then port the callers that
   depend on them. Build order: grammers-native helpers (VC signalling,
   rich sender, client/responder wiring) -> VC engine -> plugins ->
   assistant/agent/web -> binary.
4. Don't make silly mistakes: verify every TL call, function signature,
   and API shape against the grammers 0.12.0 source / generated TL code
   before writing it, and re-check after writing. If a call's shape is
   uncertain, search and verify — never guess it.
5. NO STUBS. Every module shipped is a real port of its Python source.
   If something genuinely cannot work, report exactly which call/fact
   blocks it with source evidence and stop on that piece — never write a
   stub to cover it.

## Locked decisions

- **Config**: same env vars as `pyUltroid/configs.py` (`API_ID`, `API_HASH`,
  `SESSION`, `BOT_TOKEN`, `LOG_CHANNEL`, `SESSION2/3`, `BOT_TOKEN2/3`,
  `VC_SESSION`, `REDIS_URI`, `DATABASE_URL`, `MONGO_URI`, `SETUP_TOKEN`,
  `PORT`, agent keys). dotenv loading via dotenvy; argv 1-5 overrides as in
  Python. No `.env` committed. `SESSION` in Python is a Telethon
  **StringSession string — NOT portable to grammers**; see Sessions below.
- **Sessions**: grammers `SqliteSession` files, one per client
  (`sessions/user.db`, `sessions/asst.db`, `sessions/user2.db`,
  `sessions/user3.db`, `sessions/vc.db`). No fake StringSession decoder.
  README documents the sqlite-file path.
- **Dispatcher**: hand-written `match` on grammers `Update`
  (`NewMessage` / `CallbackQuery` / `InlineQuery` / `MessageEdited` /
  `MessageDeleted` / `Raw` + wildcard — `Update` is `#[non_exhaustive]`).
  Router: per-plugin regex, compiled exactly like Python's
  `PatternCompiler.compile` (`pyUltroid/_misc/_decorators.py:94-103`): strip
  a leading `^`/`.`, prepend `\` + handler; in Rust anchored at start with
  `(?s)` (dotall) and registered for both `.` and `/` prefixes (dual mode).
  Gates mirror `ultroid_cmd`: `owner_only`, `groups_only`, `admins_only`,
  `fullsudo`, `only_devs`, `allow_guest`, default `func` = reject `via_bot`
  messages, `silent_denials` default true (silent skip; explicit denial only
  when a command sets it false). `listen()` = per-chat waiters on the update
  stream (`ultroid-core::listen::WaiterHub`).
- **Plugins**: compile-time modules (Rust has no runtime plugin loading).
  One module per Python plugin file, same command names/patterns/help text.
  Dynamic load/unload/reload (`.install`/`.unload`) is a registry
  enable/disable flag, not a dlopen.
- **DB**: trait `Database` (`get/set/del/exists/keys` + JSON helpers) in
  `ultroid-core::db`. Priority Redis > Mongo > Postgres > local JSON file
  (same as Python `pyUltroid/startup/_database.py`). Local JSON always works.
  Redis (RESP over tokio TcpStream, `redis` crate), Mongo (`mongodb` crate),
  Postgres (`tokio-postgres`) are **feature-gated** (`redis`/`mongo`/
  `postgres` features on `ultroid-core`); if a URI is set but its feature is
  not compiled, startup errors naming the feature — it never silently falls
  back to local. Per-feature DB modules mirror `pyUltroid/dB/*.py` as
  namespaced key prefixes in `ultroid-core::db::features`.
- **Rich messages — Bot only**: ONLY the bot-token client (`bot_sign_in`)
  may send rich. `ultroid-rich::RichSender` holds only a bot `grammers`
  `Client`; there is no constructor from a user client. The user-client
  `Responder::send_rich` returns `Error::RichOnlyBot` (a refusal, never a
  silent plain-text fallback at the send site — plain-text fallback is a
  separate explicit call, same as Python's fallback rendering). Rich builder
  uses `InputRichMessage::html` / `::markdown` + `.photo(id, ...)` /
  `.document(id, ...)` media binding only. `blocks()` takes legacy `PageBlock`
  only where the Python code used it; **never invent `InputRichBlock` types**
  (they do not exist in grammers 0.12). Rich draft streaming: TL
  `InputSendMessageRichMessageDraftAction` exists at layer 233; no high-level
  helper — hand-built via raw invoke, best-effort. Limits enforced:
  32,768 chars / 500 blocks / 16 nesting / 20 table cols (Python
  `richmsg.py` constants).
- **Strings**: the 24 YAML locales from `strings/strings/` are PORTED into
  idiomatic Rust — `crates/ultroid-core/src/locales.rs`, generated by
  `tools/gen_locales.py` (values preserved verbatim, format converted to
  compile-time sorted slices + a typed `LanguageMeta`). No `.yml` file is
  carried into the Rust repo. Lookup mirrors Python `StringStore.get`
  (lang -> English -> warning text).
- **Voice chat**: VERIFIED WORKABLE (2026-11-12, see below) — ntgcalls-direct,
  not py-tgcalls. `ultroid-vc` uses the official `ntgcalls` Rust crate
  3.0.7 (`NTgCalls::new()`, `create_call(chat_id) -> offer JSON`,
  `connect(chat_id, answer, false)`, `set_stream_sources(chat_id,
  StreamMode::CAPTURE, media)`, `pause/resume/mute/unmute/stop`,
  `on_stream_end`) plus grammers raw `phone.*` invokes for signalling —
  exactly the flow py-tgcalls runs against Telethon
  (`pytgcalls/mtproto/telethon_client.py`, `methods/internal/connect_call.py`):
  `channels.GetFullChannel` / `messages.GetFullChat` -> active
  `InputGroupCall`; `phone.JoinGroupCall{call, params: DataJSON(offer),
  muted: false, join_as, video_stopped}` -> scan returned `Updates` for
  `UpdateGroupCallConnection.params.data` (the answer JSON); `phone.CreateGroupCall`
  when no call is active; `phone.LeaveGroupCall`, `phone.GetGroupParticipants`,
  `phone.GetGroupCall`, `phone.GetDhConfig`, `phone.GetGroupCallChainBlocks`,
  `phone.EditGroupCallParticipant` as needed. DC routing: invoke on the home
  sender; on a 303 migrate RPC error retry with `invoke_in_dc(dc)`. The
  `ntgcalls-sys` build script downloads the static lib from GitHub releases
  with ureq — that fails behind TLS-MITM proxies, so the repo ships
  `scripts/fetch-ntgcalls.sh` (curl + system CA) and the build uses
  `NTGCALLS_LIB_DIR=$PWD/vendor/ntgcalls-lib/lib`; `vendor/` is git-ignored.
  `cargo check` of the `ntgcalls` crate passes in the sandbox with that env
  set. Queue / track resolver (YouTube/Saavn/Spotify metadata + yt-dlp
  download) / player state machine are ported from `vc/_player.py`,
  `vc/_resolver.py`, `vc/_downloader.py` etc. No stub anywhere: if a live
  join ever fails, the error surfaces with its real cause.
- **Agent**: `ultroid-agent` — provider registry (OpenAI incl. GPT-5/Codex
  Responses, Anthropic, Gemini, Groq, xAI, apmix, Meta/Llama, Ollama,
  Cloudflare, custom OpenAI-compatible) over `reqwest`; tool executor with
  shell timeout clamped to [1, 120]s and destructive ops excluded by
  default; MiniAI intent classifier.
- **Web**: `ultroid-web` — axum setup server, default port 7860, token gate
  (`SETUP_TOKEN`, constant-time compare), atomic `.env` writes
  (temp → fsync → chmod 0600 → rename), SSE logs, no OpenAPI/docs.
- **Exclusions (decided)**: do NOT port the root `startup` shell script's
  hard-coded git identity / HF credential line or its gist download, and do
  NOT port `wgptPrompt` (jailbreak-style persona file). The Rust binary is
  the entry point.
- **Verification**: `cargo check` + `cargo build` in the sandbox; `cargo test`
  for core (config parsing, router matching, rich gate, local DB). No
  login/send is ever performed in the sandbox — no credentials exist there.
  Never present a test pass as live Telegram verification. Live-only checks
  are named in README (pull, configure env, run, `.alive`).

## Python → Rust module mapping

| Python | Rust |
|---|---|
| `pyUltroid/__init__.py`, `ultroid.py`, `version.py` | `ultroid-core::{config,version}` + binary `main.rs` composition root; `VERSION = "14.10.2026[fork]"` |
| `pyUltroid/configs.py` | `ultroid-core::config::Config::from_env` |
| `pyUltroid/exceptions.py` | `ultroid-core::error::{Error, Result}` |
| `pyUltroid/startup/_database.py`, `pyUltroid/dB/*` | `ultroid-core::db::{Database, LocalJsonDb, RedisDb, MongoDb, PostgresDb, features::*}` |
| `pyUltroid/startup/dispatcher.py`, `_misc/_decorators.py` (pattern compiler, gates), `_misc/listener.py`, `startup/listen*.py` | `ultroid-core::router::{Registry, CommandDef, CommandHandler, dispatch}`, `ultroid-core::listen::WaiterHub` |
| `pyUltroid/_core/loader.py`, `manifest.py`, `dynamic.py`, `pipeline.py`, `events.py` | `ultroid-core::loader` (registry enable/disable + manifests), binary update loop |
| `pyUltroid/_core/richmsg.py`, `entities.py`, `cards.py`, `buttonkit.py`, `inlineresults.py`, `instant.py`, `custom/rich.py`, `custom/entities.py` | `ultroid-core::rich::RichDoc` (data) + `ultroid-rich::{RichSender, RichBuilder, Cards, ButtonKit}` |
| `pyUltroid/startup/BaseClient.py`, `connections.py` | binary `main.rs`: one `SenderPool` + `Client` per session file; `bot_sign_in` / login-code flow |
| `pyUltroid/startup/funcs.py`, `patching.py`, `business.py`, `guestmode.py`, `_loops.py`, `utils.py` | `ultroid-core::{startup, business, guest}` + binary |
| `pyUltroid/_misc/*` (registry, methods, wrappers, services) | `ultroid-core::{registry, methods}` |
| `pyUltroid/_core/{queue,toggles,notices,internals,assistant_creations,business_profile,stars,stories,ludo_*,matrix_arcade,webupload,telethon_compat,_cbs}.py` | `ultroid-core::{queue, toggles, notices, stars, stories, games}`; Telethon compat is dropped (no Telethon in Rust) |
| `pyUltroid/fns/*` | `ultroid-core::fns` (+ `ultroid-agent` for ytdl-adjacent bits) |
| `pyUltroid/agent/*` | `ultroid-agent::{providers, tools, mini, sessions, runtime, bridge, ...}` |
| `pyUltroid/shell/*` | `ultroid-agent::shell` (sandboxed executor, same clamps) |
| `pyUltroid/universal/*`, `adapters/*` | `ultroid-core::universal` |
| `pyUltroid/updater/*` | `ultroid-core::updater` (git via `git` CLI, not GitPython) |
| `pyUltroid/tracer/*` | `ultroid-core::tracer` |
| `pyUltroid/imaging/*` | `ultroid-core::imaging` (image crate pipeline; error cards) |
| `pyUltroid/extender/*`, `modules/*`, `b2b.py` | `ultroid-core::{extender, modules}` + `plugins::bot2bot` |
| `pyUltroid/webserver/*`, `server.py` | `ultroid-web` |
| `plugins/*.py` (102) | `ultroid-plugins::<same snake-case name>`; `register_all(&mut Registry)` |
| `assistant/*.py`, `assistant/manager/*` | `ultroid-assistant::{start, callbacks, inline, pmbot, games, agent, manager::*}` |
| `vc/*.py` | `ultroid-vc::{player, resolver, downloader, youtube, saavn, spotify, queue, ...}`; `vc/_pytgcalls.py` → `ultroid-vc::audio::AudioEngine::Unavailable` |
| `custom/*.py` | `ultroid-core::{tg (buttons/keyboards), fmt, entities, icons}` |
| `akipy/*` (vendored Akinator) | `ultroid-plugins::chatbot` uses `ultroid-core::akinator` (real Akinator HTTP API client) |
| `strings/__init__.py` + `strings/strings/*.yml` | `ultroid-core::strings::Strings` + `ultroid-core::locales` (generated Rust, no YAML shipped) |
| `resources/*` (fonts, images, web assets) | Ported meaning only: web UI statics reimplemented in `ultroid-web`; fonts/images are runtime data, fetched by path, never vendored as Python files |

## Core API (frozen for plugin authors)

```rust
// ultroid-core
pub struct Ctx { pub text, args, captures: Vec<String>, command: &'static str,
  pub chat_id: i64, pub chat_type: ChatType, pub sender_id: i64, pub message_id: i64,
  pub reply_to_id: Option<i64>, pub reply_to_text: Option<String>,
  pub is_owner/is_sudo/is_dev/is_guest/via_bot: bool,
  pub db: Arc<dyn Database>, pub config: Arc<Config>, pub http: reqwest::Client,
  pub responder: Arc<dyn Responder>, pub lang: String, pub strings: Arc<Strings>,
  pub client_kind: ClientKind /* User | AsstBot | ... */ }
impl Ctx { async fn reply(&self, text: &str) -> Result<i64>;  // + edit/delete/send_file/reply_html/send_rich
           fn get_str(&self, key: &str) -> String; }           // i18n lookup (lang fallback en)

#[async_trait] pub trait Responder: Send + Sync {
  async fn reply(&self, text: &str) -> Result<i64>;
  async fn reply_html(&self, html: &str) -> Result<i64>;
  async fn edit(&self, msg_id: i64, text: &str) -> Result<()>;
  async fn delete(&self, msg_id: i64) -> Result<()>;
  async fn send_file(&self, path: &Path, caption: &str) -> Result<i64>;
  async fn send_rich(&self, doc: &RichDoc) -> Result<i64>; // user client -> Err(RichOnlyBot)
}
#[async_trait] pub trait CommandHandler: Send + Sync {
  async fn handle(&self, ctx: &Ctx) -> Result<()>;
}
pub struct CommandDef { pub name: &'static str, pub pattern: &'static str, pub help: &'static str,
  pub owner_only/sudo_allowed/groups_only/admins_only/fullsudo/only_devs/allow_guest/silent_denials: bool }
impl Default for CommandDef { /* all false except silent_denials: true, sudo_allowed: true */ }
pub struct Registry; impl Registry {
  pub fn add(&mut self, def: CommandDef, handler: Arc<dyn CommandHandler>);
  pub async fn dispatch(&self, ctx: &mut Ctx) -> Result<DispatchOutcome>; // Matched | NoMatch | Denied
}
```

One struct per plugin implements `CommandHandler`, matching on
`ctx.command`; each plugin exposes `pub fn register(reg: &mut Registry)`.
Handlers do real work: real HTTP calls via `ctx.http`, real DB via
`ctx.db`, real Telegram sends via `ctx.responder`. No faked responses.
Where an external service needs a key the sandbox lacks, the handler
returns a clear `Error::Config` naming the env var — it does not invent
data.

## Build status (2026-11-13)

Committed (local, not yet pushed): `4d436fe` core+rich+vc,
`e4f4b14` agent, `d2c7886` web, `8c23758` assistant,
`3b56e60` plugins wave B (15 small-media plugins),
`5600ca9` binary crate. VC resolver/downloader modules
(`resolver`, `youtube`, `api`, `downloader`, `dl_helper`, `httpx`,
`saavn`, `jiosaavn`, `spotify`, `yt`) and plugin waves A (20),
C (8), D (8), G (29) are written, wired, and committed; waves
E (AI/chat, 6) and F (bot core, 16) are written and wired (all
102 plugins on the registry) and workspace `cargo check` is clean
— commit pending in this turn. All check clean; `cargo test`
passes for core/vc/web.

Remaining before push (integration pass): inline-query routing to
the `plugins::inline` engine, the `TODO(responder-op)` extension
pass (370 sites), `_help.py` vs `command_names()`, `cargo build` +
`cargo test`, README counts finalised.

Binary crate (`ultroid-rs/`): `main.rs` (setup gate → config → DB →
registry → clients → VC engine → mini-app server → update loops),
`clients.rs` (SenderPool connect + dialog warm-up + bot sign-in),
`login.rs` (interactive phone-code/2FA), `responder.rs`
(GrammersResponder; user client refuses rich with `RichOnlyBot`).
Callback routing tries the 10 assistant `handle_callback` handlers
in order, then answers unknown data with exactly "🖐️" (Phase-10
ruling). VC raw updates fan out through `Bridge::handle_raw_update`
with chat ids resolved from `Update::GroupCall.peer` or
`Bridge::chat_id_for_call` (reverse lookup by call id).

VC threading: ntgcalls is `Send` but NOT `Sync`, so `Arc<Engine>`
can never cross a thread boundary — the engine is built and run
inside its own OS thread on a current-thread runtime; only `Send`
handles (the `Bridge`, the DB) move across. Plugin VC commands
cannot reach the engine through `Shared` for the same reason; they
need an actor-style handle in the responder-op pass.

Known gaps to close before push (integration pass, after waves land):
1. InlineQuery/InlineSend/GuestChatQuery routing to the
   `plugins::inline` engine (wave C defines its binary hook).
2. `TODO(responder-op)` sites in plugins: extend the `Responder`
   trait with the raw-MTProto operations plugins need (admin actions,
   message/media fetch, chat info), implement them in the binary's
   responder, and wire each site. Deferred deps decided then too:
   `imageproc` + `ab_glyph` (text-on-image), `rqrr` (QR decode).
3. `_help.py` plugin vs `Registry::command_names()` (help listing).
4. `cargo build` (full workspace) + `cargo test`, README counts
   finalised, then push to filewrap/Ultroid-RS with ls-remote verify.
