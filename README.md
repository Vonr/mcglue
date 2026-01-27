# gluemc

Simple wrapper for Minecraft servers that aims to be version-independent and independent of mods.

Uses [poise](https://github.com/serenity-rs/poise) to create a Discord Bot as well as manage two webhooks for a chat and console channel.

Messages from Discord are relayed to clients using the `/tellraw` command, while messages from the game are relayed to the chat channel via a webhook.

Console logs are sent to the console channel, and messages sent there are executed on the server as commands.

### Usage

```sh
gluemc <command>

# Example (see test.sh)
gluemc docker compose attach server
```

### Environment Variables
- `$DISCORD_BOT_TOKEN` should be set to a Discord bot token
- `$DISCORD_WEBHOOK_URL` should be set to a Discord webhook URL
- `$DISCORD_CONSOLE_WEBHOOK_URL` should be set to a Discord webhook URL
- `$DISCORD_CHANNEL_ID` should be set to a Discord channel ID
- `$DISCORD_CONSOLE_CHANNEL_ID` should be set to a Discord channel ID
- `$DISCORD_OPERATOR_ROLE_ID` should be set to a Discord role ID
- `$SERVER_DIRECTORY` should be set to the path to the server's root directory
