# mcglue

Simple wrapper for Minecraft servers that aims to be version-independent and independent of mods.

Uses [poise](https://github.com/serenity-rs/poise) to create a Discord Bot as well as manage two webhooks for a chat and console channel.

Messages from Discord are relayed to clients using the `/tellraw` command, while messages from the game are relayed to the chat channel via a webhook.

Console logs are sent to the console channel, and messages sent there are executed on the server as commands.

## Installation

mcglue provides automatically built binaries for certain targets in the [releases](https://github.com/Vonr/mcglue/releases).   
They may be retrieved manually or with [cargo-binstall](https://github.com/cargo-bins/cargo-binstall) with `cargo binstall --git https://github.com/Vonr/mcglue mcglue`.

You can choose to install from source with `cargo install --git https://github.com/Vonr/mcglue`

### Usage

```sh
mcglue <command>

# Examples
mcglue docker compose attach server

# With custom entrypoint (see test.sh, compose.yml, mcglue-entrypoint)
docker compose up
```

See `test.sh`, `compose.yml`, and `mcglue-entrypoint` for a setup that uses [`itzg/docker-minecraft-server`](https://docker-minecraft-server.readthedocs.io/) via Docker Compose with a custom entrypoint.

### Environment Variables
- `$DISCORD_BOT_TOKEN` should be set to a Discord bot token
- `$DISCORD_WEBHOOK_URL` should be set to a Discord webhook URL
- `$DISCORD_CONSOLE_WEBHOOK_URL` should be set to a Discord webhook URL
- `$DISCORD_CHANNEL_ID` should be set to a Discord channel ID
- `$DISCORD_CONSOLE_CHANNEL_ID` should be set to a Discord channel ID
- `$DISCORD_OPERATOR_ROLE_ID` should be set to a Discord role ID
- `$SERVER_DIRECTORY` should be set to the path to the server's root directory
