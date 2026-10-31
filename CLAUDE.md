# Agent notes: globibot-rs

See README.md for the architecture. Private deploy/infra notes, if present, are in CLAUDE.local.md.

## Code map
- Plugin entry: `Plugin::connect_init(endpoints, |rpc| ...)` returns `ConnectedPlugin { inner: Arc<T>, rpc }`; then `plugin.handle_events()`. Endpoints from env via `common::endpoints::tcp_from_env` (`RPC_ADDR`, `SUBSCRIBER_ADDR`).
- RPC methods plugins can call: `globibot-core/src/rpc.rs`. Text-only `send_message`; `send_file` has no text.
- Answering commands: `interaction.ctx(&rpc)` gives a `globibot_core::interaction::CommandCtx` (`reply`, `reply_ephemeral`, `defer`, `edit`, `respond` with serenity builders). Handlers take `ctx: &CommandCtx<'_>`. Builder attachments aren't sent: the RPC carries JSON only.
- The bot forwards only slash/command interactions to plugins (`globibot-bot/src/discord.rs`). Button/component clicks are dropped.
- Plugin web API: `rpc.register_web_api(advertise_addr)` makes the bot add a Caddy route `/<plugin id>/*` (prefix stripped). Publicly: `https://globibot.com/plugin-api/<plugin id>/...`. Logged-in users are checked via the `discord_session` private cookie + `COOKIE_SECRET` (copy from the llm plugin).
- Storage: `RedisStorage::from_env()` (`REDIS_URL`), values implement `StorageValue` with a `REDIS_NS`.

## Slash commands
- Declared with the `globibot_core::command` macros (`bot_command!`, `bot_commands!`, see the module docs), registered on every plugin start through `rpc.upsert_global_command` / `upsert_guild_command`.
- Upsert matches existing commands by name and type, and edits only if `command_changed` (`globibot-bot/src/rpc.rs`) sees a difference in a field the plugin sends (or in options).
- Changes made directly via the Discord API are overwritten on the next plugin start. Commands a plugin stops declaring, or whose type changed, are left on Discord: delete them by hand.
- Discord Activities: one Activity per application. Entry point commands (type 4) show only in the App Launcher, never in the `/` picker. Handler 1 = app answers (no channel message), handler 2 = Discord launches and posts an invite card. Discord ignores type changes in a PATCH; delete and recreate instead.

## Build gotchas
- `build.sh` copies `target/release/globibot-plugin-*` (skipping `*.d`) into `arm64-artifacts/plugins/<name>`. Every plugin crate must be named `globibot-plugin-<name>`.
- `cargo check -p <crate>` gives quick type checks; release builds go through `./build-artifacts.sh` (Docker, interactive `-it`).

## Deploy shape
- Each process is a Nomad job using the same image tag; plugin jobs find the bot and Redis through Nomad service discovery (`globibot-rpc`, `globibot-subscriber`, `redis`) in a `template` block, and secrets through a Nomad variable at `nomad/jobs/<job>/<group>/<task>`.
- A new plugin needs its own `globibot-plugin-<name>.nomad` job file (copy a similar plugin's). A plugin with a web API also needs a `web` port and `<NAME>_WEB_ADDR_LISTEN` / `<NAME>_WEB_ADDR_ADVERTISE` (see llm/crossword).
- Ship config with the code: a plugin that requires new env vars exits at startup until its job file and Nomad variable are updated.
- Never print secrets; print key names only.
