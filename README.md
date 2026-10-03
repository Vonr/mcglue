# mcglue

Simple wrapper for Minecraft servers that aims to be version-independent and independent of mods.

Uses [poise](https://github.com/serenity-rs/poise) to create a Discord Bot as well as manage two webhooks for a chat and console channel.

Messages from Discord are relayed to clients using the `/tellraw` command, while messages from the game are relayed to the chat channel via a webhook.

Console logs are sent to the console channel, and messages sent there are executed on the server as commands.

### Commands

- **tpo** - Teleport offline players by editing their player data
- **crash** - Get the latest crash report, if one exists
- **list** - Get a list of online players
- **nbtq** - Inpsect or edit NBT files using jq-like syntax
- **download** - Download a file from the server, using iroh for P2P transfers above the attachment size limit
- **upload** - Upload a file to the server, with the option of using iroh for P2P transfers
- **delete** - Delete a file from the server

### Installation

mcglue provides automatically built binaries for certain targets in the [releases](https://github.com/Vonr/mcglue/releases).   
They may be retrieved manually or with [cargo-binstall](https://github.com/cargo-bins/cargo-binstall) with `cargo binstall --git https://github.com/Vonr/mcglue mcglue`.

You can choose to install from source with `cargo install --git https://github.com/Vonr/mcglue`

There is also an `eggs/` directory in the repository containing Pterodactyl-compatible eggs that automatically install mcglue via cargo-binstall.

Your Discord bot should have the `bot` scope and the following permissions:
- Send Messages
- Embed Links
- Attach Files
- Use Slash Commands
- Bypass Slowmode (optional)

These permissions correspond to the permission integer of `4503601774905344`, meaning your invite link should look like `https://discord.com/oauth2/authorize?client_id=YOUR_CLIENT_ID&permissions=4503601774905344&integration_type=0&scope=bot`

### Usage

```sh
mcglue <command>

# Examples
mcglue java -jar server.jar -nogui
mcglue ./start.sh

# Integrated with itzg/docker-minecraft-server (see test.sh and compose.yml)
docker compose up
```

See `test.sh`, and `compose.yml` for a setup that uses [`itzg/docker-minecraft-server`](https://docker-minecraft-server.readthedocs.io/) via Docker Compose.

### Environment Variables
- `$DISCORD_BOT_TOKEN` should be set to a Discord bot token
- `$DISCORD_WEBHOOK_URL` should be set to a Discord webhook URL
- `$DISCORD_CONSOLE_WEBHOOK_URL` should be set to a Discord webhook URL
- `$DISCORD_CHANNEL_ID` should be set to a Discord channel ID
- `$DISCORD_CONSOLE_CHANNEL_ID` should be set to a Discord channel ID
- `$DISCORD_OPERATOR_ROLE_ID` should be set to a Discord role ID
- `$SERVER_DIRECTORY` should be set to the path to the server's root directory
