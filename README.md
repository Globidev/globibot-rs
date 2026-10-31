# globibot-rs

A Discord bot built as a small core plus independent plugin processes.

- **`globibot-bot`**: holds the Discord connection. Plugins talk to it over TCP: an RPC server (`RPC_ADDR`) to act on Discord, an event stream (`SUBSCRIBER_ADDR`) to receive messages and interactions. It also serves the web API (`WEB_ADDR`, Discord OAuth login) and registers plugin web APIs in a Caddy proxy (`CADDY_ADDR`).
- **`globibot-core`**: shared types: the `Plugin` trait, RPC protocol (tarpc), events, Redis storage.
- **`plugins/*`**: one binary per plugin (`globibot-plugin-<name>`). `plugins/common` holds shared helpers.
- **`web/`**: SvelteKit static site (globibot.com). `/api/*` goes to the bot, `/plugin-api/<plugin>/*` to plugin web APIs.

## Plugins

| Plugin | What it does |
|---|---|
| `crossword` | `/crossword` launches the Mini Crossword Discord Activity; keeps an all-time leaderboard fed by the game's results webhook |
| `llm` | Replies when mentioned, plus `/roast`, `/hype`, `/tldr`, lore commands… (OpenRouter), with a web page |
| `rateme`, `slap`, `tuck` | Image commands |
| `ping` | Minimal example: replies `pong!` to `!ping` |

## Local development

```sh
set -a; . ./local.env; set +a       # env vars (not committed)
caddy run --config Caddyfile        # plugin API gateway on :7777
cargo run -p globibot-bot
cargo run -p globibot-plugin-<name>
cd web && bun install && bun run dev  # proxies /api -> :8001, /plugin-api -> :7777
```

## Build & release

```sh
./build-artifacts.sh   # cross-builds the bot + all plugins in the `globibot-builder` image into arm64-artifacts/
docker build -f release.Dockerfile -t globidocker/globibot:<tag> . && docker push globidocker/globibot:<tag>
cd web && bun run build   # static site in web/build/
```

One image holds everything: the bot is `/globibot`, plugins are `/plugins/<name>`. Production runs each as a Nomad job using that image.
