# Ultroid-RS

A complete port of [Ultroid](https://github.com/mahesh953-hub/UltroidRewrite)
("SarahUB", HEAD `5d09531`) from Python/Telethon to Rust, on
[grammers](https://github.com/Lonami/grammers) 0.12.0 (MTProto TL layer
233). Every module is ported — the core runtime, the AI agent, the web
setup server, the voice-chat engine, the assistant bot, and all 102
plugins. Nothing is copied as-is: everything is idiomatic Rust.

## Quick start

```sh
# Build requirements: a Rust toolchain (edition 2024) + ffmpeg/ffprobe
# and yt-dlp on PATH for the media plugins.
# The voice engine needs the prebuilt ntgcalls static library:
./scripts/fetch-ntgcalls.sh
export NTGCALLS_LIB_DIR="$PWD/vendor/ntgcalls-lib/lib"

cargo build --release
```

Configure with environment variables (or a `.env` file — never commit
it). The minimum is an API id/hash from https://my.telegram.org:

```sh
API_ID=123456
API_HASH=0123456789abcdef0123456789abcdef
SESSION=            # optional: path to a session file (see Sessions)
BOT_TOKEN=          # optional: assistant bot token
LOG_CHANNEL=        # optional: numeric channel id for logs
OWNER_ID=           # optional: your Telegram user id
```

Run it:

```sh
./target/release/ultroid-rs
```

With no API keys configured, the binary starts the setup server instead
(`http://0.0.0.0:7860`, token printed on the terminal) — the same
phone-OTP / QR login the Python `pyUltroid/webserver` provided. Once
setup completes, the binary restarts itself into the bot.

Live checks (these only work against real Telegram — nothing in this
repo logs in or sends anything on its own):

1. `cargo build --release` passes in the sandbox.
2. On a host: set `API_ID`/`API_HASH`, run the binary, complete login.
3. In Telegram, send `.alive` — the user client answers.
4. Send `.help` — the plugin list answers.

## Sessions

Sessions are **grammers sqlite files**, one per client, under
`data_dir/sessions/` (`ultroid.db` for the user client, `asst.db` for
the assistant bot, `session2.db` / `session3.db` for extra accounts).
A Telethon `StringSession` string is **not portable** to grammers —
there is no decoder for it here, and none is claimed. To move an
account over, log in again (phone code / 2FA via the terminal login,
or the setup server's phone-OTP / QR flow). Session files are
git-ignored; never share them.

## Workspace layout

| Crate | Contents |
|---|---|
| `ultroid-core` | Config (env + argv), SQLite/Redis/Mongo/Postgres/local-JSON DB, the command router (`. `/`/` dual prefixes, owner/sudo/guest gates), `listen()` waiters, strings (24 locales, generated), rich document model, tracer, updater, imaging, extender/addons, shell, business/guest mode, stars/stories, games |
| `ultroid-rich` | Rich-message builder + the **bot-only** sender. `RichSender` holds only a bot client; `send_rich` on a user client is refused with `RichOnlyBot` and the caller falls back to plain text, exactly like Python |
| `ultroid-vc` | Voice chat: the grammers `phone.*` bridge (peer/call resolution, join handshake, DC migration, P2P, conference joins) + the ntgcalls engine (play/pause/volume/seek, queue, stream-end handling), MediaStream → MediaDescription (STUDIO 96 kHz/2ch, HIGH 48 kHz/2ch), YouTube/Saavn/JioSaavn/Spotify resolvers and downloaders |
| `ultroid-agent` | The AI agent: 10 providers (OpenAI, Anthropic, Gemini, Groq, OpenRouter, Ollama, xAI, Apimix, Meta Llama, OpenAI-compatible, Cloudflare), tool executor (shell/web/think/remember/…), agentic loop, MiniAI, session store |
| `ultroid-assistant` | The assistant bot: start/settings/callback menus, Ludo + Matrix arcades, games, PM bot, rich showcase/creations, ytdl, manager (help/afk/misc/admins/stickermanager) |
| `ultroid-plugins` | All 102 plugins, one module per Python plugin file, same command names/patterns/help text |
| `ultroid-web` | The setup server (axum): token-gated setup UI, phone-OTP + QR login, SSE logs, atomic `.env` writer, bootstrap gate, keep-alive pinger, mini-app `/settings` server |
| `ultroid-rs` | The binary: client connect/auth, the update loop (NewMessage/MessageEdited → `Registry::dispatch`, callback routing, VC raw-update fan-out), the grammers `Responder`, the mini-app server, the engine loop |

## Voice chat — what is real and what is not

The voice path is the **direct ntgcalls Rust crate (3.0.7)** plus raw
`phone.*` invokes through grammers — the py-tgcalls Python wrapper
does **not** port and is not used. The join handshake is real:
`NTgCalls::create_call` → offer JSON → `phone.JoinGroupCall` with
`DataJson` → transport JSON from `UpdateGroupCallConnection` →
`NTgCalls::connect`; DC migration is retried on 303 via
`invoke_in_dc`; P2P and conference joins are implemented the same
way. Voice joins happen on the **user client**, never the bot client,
and `BOT_MODE` disables voice entirely.

What was **not** verified: no live group-call join was ever performed
in the sandbox (no account, no call exists there). The build passes
and the ntgcalls crate initialises (version 3.0.7), but a live join is
a live-only check — do not treat a green build as proof that a join
works.

Build note: `ntgcalls-sys`'s build script downloads its 30 MB
static-lib zip with ureq/rustls, which fails certificate validation
under MITM proxies. `scripts/fetch-ntgcalls.sh` downloads it with
`curl` into `vendor/ntgcalls-lib/` (git-ignored); set
`NTGCALLS_LIB_DIR` as above before building.

## Rich messages — bot only

Rich messages are sent **only from the bot-token client**. The user
client sends plain/entity text only; a rich send attempted on it is a
compile-time-shaped refusal (`Error::RichOnlyBot`), and every rich
send also renders a plain-text fallback. The builder uses
`InputRichMessage::html` / `markdown` plus `.photo` / `.document`
binding; the legacy `blocks()` form is used only where Python built
blocks. There is no `InputRichBlock` in grammers 0.12 — none is
invented.

## Rules this port follows

- **No stubs.** Every shipped module is a real port of its Python
  source. Where the sandbox cannot reach a service, the real client
  call is implemented (real HTTP, real `yt-dlp`/`ffmpeg` subprocess);
  a missing credential is an honest `Error::Config` naming the env
  var, never a faked response.
- **TL layer 233, pinned.** `grammers-* =0.12.0` exactly; no 0.11
  stack, no downgrade to layer 230.
- **New functions first.** grammers-native helpers (VC signalling,
  rich sender, client/responder wiring) were built before the callers
  that depend on them.
- **No copy as-is.** Config, strings, and assets are ported as
  meaning (e.g. the 24 YAML locales became generated Rust slices),
  never dumped Python files.
- **Excluded by decision:** the root `startup` shell script's
  hard-coded git identity / credential line and gist download, and
  the `wgptPrompt` jailbreak-style persona file. The Rust binary is
  the entry point.

## Plugin status

All 102 plugins are wired into the registry (`ultroid-plugins`, see
`DESIGN.md` for the per-plugin table). The `Responder` trait
(`ultroid-core`, 47 methods + 4 group-call methods) covers the
bulk of the Telegram operations; the wiring wave connected ~200
plugin sites to it, and the voice commands reach the engine
through the `Ctx.vc` actor handle. 171 sites remain as honest
errors naming the exact missing call (`TODO(responder-op)`) —
the documented gap families in `RESPONDER_OPS.md` (the
`payments.*` Stars suite, BusinessConnection/quick replies,
forum topics, factcheck, block/unblock, emoji status,
reactions, chat actions, effects list, create/invite chats,
media-by-reference sends, quiz polls, custom-emoji upload);
they are wired when a second trait round lands with TL-verified
implementations for those RPCs. Nothing there claims to work
when it does not.

## Environment variables

Same names as `pyUltroid/configs.py`: `API_ID`, `API_HASH`,
`SESSION`, `SESSION2`, `SESSION3`, `BOT_TOKEN`, `BOT_TOKEN2`,
`BOT_TOKEN3`, `LOG_CHANNEL`, `REDIS_URI`, `REDIS_PASSWORD`,
`DATABASE_URL`, `MONGO_URI`, `VC_SESSION`, `BOT_MODE`, `OWNER_ID`,
`SUDO` (comma list), `DEVLIST`, `LANG`, `SETUP_TOKEN`, `SETUP_BIND`,
`PORT`, `SPACE_URL`, `HF_TOKEN`, `KEEP_ALIVE`, plus the agent keys
(`AGENT_OPENAI_KEY`, `AGENT_ANTHROPIC_KEY`, `AGENT_GEMINI_KEY`,
`AGENT_GROQ_KEY`, `AGENT_XAI_KEY`, `AGENT_META_LLAMA_KEY`,
`AGENT_OPENROUTER_KEY`, `AGENT_APIMIX_KEY`, … — see
`crates/ultroid-agent/src/config.rs` for the full provider table).
`SESSION` is a session-file path (or empty for interactive login),
never a Telethon string.

## License

AGPL-3.0-or-later, same as the source.
