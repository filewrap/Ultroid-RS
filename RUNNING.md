# Running Ultroid-RS

Ultroid-RS is the Rust port of Ultroid (SarahUB) on grammers 0.12.0
(TL layer 233). This guide takes you from a fresh clone to a running
userbot. Everything here was checked against this repo — nothing in it
is guessed.

## 1. What you need

| Requirement | Notes |
|---|---|
| Rust toolchain, edition 2024 | `rustc` 1.85+ (edition 2024); the sandbox builds with 1.99.1. Install via [rustup](https://rustup.rs). |
| `git`, `curl`, `unzip` | `curl` + `unzip` are needed for the vendor step below. |
| System build libraries | A C compiler + linker and the zlib dev library — the final link needs `-lz`. Debian/Ubuntu: `sudo apt-get update && sudo apt-get install -y build-essential pkg-config zlib1g-dev`. Fedora: `sudo dnf install -y gcc pkg-config zlib-devel`. Alpine: `apk add build-base pkg-config zlib-dev`. Without `zlib1g-dev` the release build fails at link time with `rust-lld: error: unable to find library -lz` — that is a missing system package, not a code error. |
| `ffmpeg` + `ffprobe` on `PATH` | Media plugins and voice chat probe/convert with them. |
| `yt-dlp` on `PATH` | YouTube / media download plugins shell out to it. |
| Telegram API id + hash | From https://my.telegram.org/apps — free, takes a minute. |
| A phone number with a Telegram account | For the first login (code + 2FA if enabled). |
| A bot token (optional but recommended) | From [@BotFather](https://t.me/BotFather). The assistant bot delivers rich messages and inline buttons — rich messages are **bot-only** in this port. Without it, the user client still works, but rich sends fall back to plain text. |

Linux x86_64 and arm64, and macOS arm64, are supported by the
vendor step. Windows is not covered by `scripts/fetch-ntgcalls.sh`.

## 2. Clone

```sh
git clone https://github.com/filewrap/Ultroid-RS.git
cd Ultroid-RS
```

## 3. Vendor cloning — the ntgcalls static library (required)

Voice chat uses the `ntgcalls` Rust crate (3.0.7). Its `-sys` crate
downloads a ~30 MB prebuilt static library at build time with
`ureq`/rustls. That download fails certificate validation behind
proxies that MITM TLS (verified in the sandbox:
`invalid peer certificate: UnknownIssuer`), and it also fails
anywhere the release host is unreachable. So the build does **not**
fetch it — you vendor it first with `curl` (system CA bundle):

```sh
./scripts/fetch-ntgcalls.sh
```

What that script does, step by step:

1. Picks the archive for your platform from `uname`:
   `linux-x86_64`, `linux-arm64`, or `macos-arm64`. Anything else
   exits with `unsupported platform`.
2. Downloads (only if not already present)
   `vendor/ntgcalls.<platform>-static_libs.zip` from
   `https://github.com/pytgcalls/ntgcalls/releases/download/v3.0.7/ntgcalls.<platform>-static_libs.zip`
   with `curl -sL`. Pin a different version with
   `NTGCALLS_VERSION=3.0.7 ./scripts/fetch-ntgcalls.sh` — it must
   match the `ntgcalls = "3.0.7"` pin in the workspace `Cargo.toml`.
3. Extracts it to `vendor/ntgcalls-lib/` (`lib/` +
   `include/`). The extracted tree is git-ignored (`vendor/` in
   `.gitignore`) — it is **never committed**. Re-running the script
   wipes `vendor/ntgcalls-lib` and re-extracts from the kept zip.

Verify it landed:

```sh
ls vendor/ntgcalls-lib/lib
# libntgcalls.a
```

Manual equivalent, if you prefer to do it by hand instead of the
script (same URLs, same layout):

```sh
VER=3.0.7
curl -sL -o /tmp/ntgcalls.zip \
  "https://github.com/pytgcalls/ntgcalls/releases/download/v${VER}/ntgcalls.linux-x86_64-static_libs.zip"
mkdir -p vendor/ntgcalls-lib
unzip -o -q /tmp/ntgcalls.zip -d vendor/ntgcalls-lib
```

Then point every build at it — this export is required for
**every** `cargo build` / `cargo check` / `cargo test` invocation
in this repo:

```sh
export NTGCALLS_LIB_DIR="$PWD/vendor/ntgcalls-lib/lib"
```

Without it, the build fails in `ntgcalls-sys`'s build script on the
download. With it, the build links the vendored `libntgcalls.a`
and never touches the network for the library.

## 4. Build

```sh
export NTGCALLS_LIB_DIR="$PWD/vendor/ntgcalls-lib/lib"
cargo build --release
```

The binary is `./target/release/ultroid-rs`. A debug build
(`cargo build`) works the same way and is faster to iterate with.

Sanity check without running the bot:

```sh
export NTGCALLS_LIB_DIR="$PWD/vendor/ntgcalls-lib/lib"
cargo check --workspace
```

## 5. Configure

Configuration is environment variables (or a `.env` file in the
repo root, loaded at startup — **never commit it**; `.env` is
git-ignored). There is no `.env.example` in the repo yet, so create
`.env` yourself:

```sh
# Minimum — get these from https://my.telegram.org/apps
API_ID=123456
API_HASH=0123456789abcdef0123456789abcdef

# Assistant bot (recommended) — from @BotFather
BOT_TOKEN=123456:ABC-DEF1234ghIkl-zyx57W2v1u123ew11

# Optional
LOG_CHANNEL=            # numeric channel/group id for logs (0 = disable)
OWNER_ID=               # your Telegram user id
SUDO=                   # comma-separated extra user ids allowed to run commands
SESSION=                # see Sessions below; leave empty for first login
SESSION2=               # optional extra user account (session-file path)
SESSION3=               # optional extra user account (session-file path)
BOT_TOKEN2=             # optional extra assistant bot
BOT_TOKEN3=             # optional extra assistant bot
VC_SESSION=             # optional voice-chat account (session-file path)
BOT_MODE=               # set to 1 for bot-only deployment (no user account; voice disabled)
REDIS_URI=              # optional DB backend (priority: Redis > Mongo > SQL > local JSON)
MONGO_URI=              # optional MongoDB backend
DATABASE_URL=           # optional Postgres backend
SETUP_TOKEN=            # REQUIRED for the setup server: set this yourself.
                      # If unset, a token is minted at startup but never
                      # printed — the banner URL shows ?token=<redacted>.
                      # Set it here so you know it.
SETUP_BIND=0.0.0.0      # setup server bind address (default 0.0.0.0)
PORT=7860               # setup server port (default 7860)
ULTROID_DATA_DIR=.      # data dir (default "."); sessions/downloads/addons live under it
LANG=en                 # UI language (24 locales available)
```

The same names work as CLI arguments in the Python order, if you
prefer: `./target/release/ultroid-rs <API_ID> <API_HASH> <SESSION>
<REDIS_URI> <REDIS_PASSWORD>`.

Agent (AI) keys are also env vars — see
`crates/ultroid-agent/src/config.rs` for the full provider table
(`AGENT_OPENAI_KEY`, `AGENT_ANTHROPIC_KEY`, `AGENT_GEMINI_KEY`,
`AGENT_GROQ_KEY`, `AGENT_XAI_KEY`, `AGENT_META_LLAMA_KEY`,
`AGENT_OPENROUTER_KEY`, `AGENT_APIMIX_KEY`, …). Missing keys are
an honest `Error::Config` naming the variable, never a faked
response.

### Sessions

Sessions are **grammers SQLite files**, one per client, under
`<ULTROID_DATA_DIR>/sessions/` (`ultroid.db` for the user client,
`asst.db` for the assistant bot, `session2.db` / `session3.db` for
extra accounts). `SESSION` is a session-file path, or empty for
interactive login.

A Telethon `StringSession` string is **not portable** to grammers —
there is no decoder for it here and none is claimed. To move an
account over, log in again (section 6). Session files are
git-ignored; never share them — anyone with the file owns the
account.

## 6. First run — login

Run the binary:

```sh
./target/release/ultroid-rs
```

Two login paths, depending on what is configured:

### A. Setup server (no API keys configured)

If `API_ID`/`API_HASH` are missing, the binary starts the setup
server instead of the bot:

- Open `http://<host>:7860` (`SETUP_BIND` / `PORT`).
- Enter your `SETUP_TOKEN` from `.env` in the page. Set it
  yourself before running — if it is unset, the server mints a
  token at startup but never prints it (the banner URL shows
  `?token=<redacted>`), so there would be no way to know it.
- Complete phone-OTP or QR login in the page. Sessions are written
  and the binary re-execs itself into the bot on completion.

### B. Terminal login (API keys configured, no session yet)

With `API_ID`/`API_HASH` set and no session file, the binary
prompts on stdin:

1. `Phone number (international format, e.g. +91...):` — enter it.
2. `Login code sent by Telegram:` — enter the code Telegram sends.
3. If the account has two-step verification:
   `Two-step verification password:` — enter it.

Codes and passwords are read from stdin and never logged. Sign-up
is not possible from a third-party client — the number must already
have a Telegram account (sign up in an official app first).

On success the session file is saved under `sessions/` and the bot
starts. Later runs reuse it — no login prompt.

## 7. Running it day to day

```sh
export NTGCALLS_LIB_DIR="$PWD/vendor/ntgcalls-lib/lib"  # only needed for builds, not for running
./target/release/ultroid-rs
```

Logs go to stdout (`tracing`, `info` by default; set
`RUST_LOG=debug` for more). The bot stays in the foreground — for
a server, run it under a supervisor. Example systemd unit (adjust
paths and user):

```ini
[Unit]
Description=Ultroid-RS
After=network-online.target
Wants=network-online.target

[Service]
User=ultroid
WorkingDirectory=/opt/Ultroid-RS
EnvironmentFile=/opt/Ultroid-RS/.env
ExecStart=/opt/Ultroid-RS/target/release/ultroid-rs
Restart=on-failure
RestartSec=5

[Install]
WantedBy=multi-user.target
```

## 8. Verify it works (live checks)

These only work against real Telegram — nothing in this repo logs
in or sends anything on its own.

1. In Telegram, send `.alive` from the owner account — the user
   client answers with the alive card.
2. Send `.help` — the plugin list answers.
3. Send `.ping` — it answers with latency.
4. Voice: join a voice chat and send `.vplay <query or reply to
   audio>` — the user client joins and plays. Voice joins on the
   **user client**, never the bot client; `BOT_MODE` disables voice
   entirely.

## 9. Updating

```sh
git pull
./scripts/fetch-ntgcalls.sh   # only needed if the ntgcalls pin changed
export NTGCALLS_LIB_DIR="$PWD/vendor/ntgcalls-lib/lib"
cargo build --release
# restart the binary (systemd: systemctl restart ultroid-rs)
```

Session files and `.env` survive updates — they live outside git.

## 10. Troubleshooting

| Symptom | Cause / fix |
|---|---|
| `rust-lld: error: unable to find library -lz` at link time (after everything compiles) | Missing zlib dev library on the host. Debian/Ubuntu: `sudo apt-get install -y zlib1g-dev`, then re-run `cargo build --release`. Fedora: `zlib-devel`; Alpine: `zlib-dev`. Nothing in the repo changed — this is a system package. |
| `failed to fetch ... ntgcalls...-static_libs.zip: invalid peer certificate: UnknownIssuer` | The build tried to download the lib itself. Run `./scripts/fetch-ntgcalls.sh` and `export NTGCALLS_LIB_DIR="$PWD/vendor/ntgcalls-lib/lib"` before building. |
| `cargo build` fails in `ntgcalls-sys` | Same as above — `NTGCALLS_LIB_DIR` is unset or points at the wrong dir. Check `ls vendor/ntgcalls-lib/lib/libntgcalls.a`. |
| `unsupported platform` from the fetch script | Your OS/arch isn't in the script's list (Linux x86_64/arm64, macOS arm64). Download the matching release zip by hand (section 3) or build ntgcalls from source (needs `libpulse-dev` + a C++ toolchain). |
| `API_ID is required (env or argv[1])` / `API_HASH is required` | Set them in `.env` or the environment; see section 5. |
| `invalid login code; restart and try again` | The code expired or was mistyped. Restart and request a fresh one. |
| `this phone number has no Telegram account` | Sign up in an official Telegram app first; third-party clients cannot register accounts. |
| `invalid 2FA password` | Wrong two-step verification password. Check it in an official app. |
| Setup server page won't load / port in use | Something else is on `PORT` (default 7860). Set `PORT` to a free port, or stop the other process. |
| `.alive` gets no reply | Check the bot is running and the logs; confirm the sender is the owner or in `SUDO`. Commands are owner/sudo-gated by default. |
| A command errors naming a missing call (`TODO(responder-op)`) | That operation is one of the 167 documented gaps in `RESPONDER_OPS.md` (payments/Stars, BusinessConnection, forum topics, factcheck, block/unblock, emoji status, reactions, chat actions, create/invite chats, media-by-reference, custom-emoji upload, public-username resolution). It fails honestly instead of pretending to work. |
| Voice join fails | No live group-call join was ever verified in the sandbox — this is a live-only path. Check `VC_SESSION` / the user session, `ffmpeg` on `PATH`, and the logs; report the exact error. |

## 11. What is not verified

- No live Telegram login, send, or group-call join was ever
  performed from this repo in the sandbox — there is no account or
  call there. A green build is not proof a join works.
- The 167 `TODO(responder-op)` sites in `RESPONDER_OPS.md` are
  honest gaps, not working features.
- Imaging text rendering is deferred — it needs a real host font
  source to do honestly.

## 12. Layout (for reference)

| Path | Contents |
|---|---|
| `crates/ultroid-core` | Config, DB, router, `listen()` waiters, strings, tracer, updater, imaging, extender, shell, business/guest mode, stars/stories, games |
| `crates/ultroid-rich` | Rich-message builder + bot-only sender |
| `crates/ultroid-vc` | Voice chat: grammers `phone.*` bridge + ntgcalls engine + resolvers/downloaders |
| `crates/ultroid-agent` | AI agent: 10 providers, tools, agentic loop, MiniAI |
| `crates/ultroid-assistant` | Assistant bot handlers + manager modules |
| `crates/ultroid-plugins` | All 102 plugins |
| `crates/ultroid-web` | Setup server (axum) + mini-app server |
| `crates/ultroid-rs` | The binary |
| `scripts/fetch-ntgcalls.sh` | Vendor downloader (section 3) |
| `DESIGN.md` | Python → Rust module mapping |
| `RESPONDER_OPS.md` | Responder contract + gap inventory |
