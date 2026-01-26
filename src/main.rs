mod bot;
mod parsing;

use crate::parsing::*;
use std::{borrow::Cow, convert::Infallible, path::PathBuf, sync::OnceLock};
use tokio::io::{AsyncBufReadExt, BufReader};

use async_signal::{Signal, Signals};
use bstr::ByteSlice;
use chumsky::prelude::*;
use futures::StreamExt;
use poise::serenity_prelude::{CreateEmbed, CreateEmbedAuthor, ExecuteWebhook, Http, Webhook};

const HELP: &str = "\
Usage: gluemc <path>
";

type Error = eyre::Error;
type Result<T, E = Error> = eyre::Result<T, E>;

#[allow(clippy::type_complexity)]
static DEATH_MESSAGES: OnceLock<
    &'static [(
        &'static [u8],
        DeathMessageComponent,
        &'static [u8],
        DeathMessageComponent,
        &'static [u8],
        DeathMessageComponent,
        &'static [u8],
    )],
> = OnceLock::new();

#[derive(Clone, Copy, Debug)]
enum DeathMessageComponent {
    Victim,
    Attacker,
    Weapon,
    Empty,
}

mod env {
    use menv::require_envs;
    require_envs! {
        (assert_env_vars, any_set, gen_help);

        discord_bot_token, "DISCORD_BOT_TOKEN", String,
        "DISCORD_BOT_TOKEN should be set to a Discord bot token";

        discord_webhook_url, "DISCORD_WEBHOOK_URL", String,
        "DISCORD_WEBHOOK_URL should be set to a Discord webhook URL";

        discord_console_webhook_url, "DISCORD_CONSOLE_WEBHOOK_URL", String,
        "DISCORD_CONSOLE_WEBHOOK_URL should be set to a Discord webhook URL";

        discord_channel_id, "DISCORD_CHANNEL_ID", u64,
        "DISCORD_CHANNEL_ID should be set to a Discord channel ID";

        discord_console_channel_id, "DISCORD_CONSOLE_CHANNEL_ID", u64,
        "DISCORD_CONSOLE_CHANNEL_ID should be set to a Discord channel ID";

        // server_root, "SERVER_ROOT", String,
        // "SERVER_ROOT should be set to the path to the server's root directory";
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    color_eyre::install()?;
    let _ = dotenvy::dotenv();

    if env::any_set() {
        env::assert_env_vars();
    } else {
        eprintln!("{}", env::gen_help());
        std::process::exit(1);
    }

    let token = env::discord_bot_token();

    let http = Http::new(&token);
    let webhook = Webhook::from_url(&http, &crate::env::discord_webhook_url()).await?;
    let console_webhook =
        Webhook::from_url(&http, &crate::env::discord_console_webhook_url()).await?;

    let mut pargs = pico_args::Arguments::from_env();
    if pargs.contains(["-h", "--help"]) {
        print!("{}", HELP);
        std::process::exit(0);
    }

    let path = pargs
        .free_from_os_str::<PathBuf, Infallible>(|s| Ok(PathBuf::from(s)))
        .unwrap();

    pargs.finish();

    let death_messages =
        reqwest::get("https://assets.mcasset.cloud/1.21.11/assets/minecraft/lang/en_us.json")
            .await?
            .bytes()
            .await?
            .split(|b| *b == b'\n')
            .filter_map(|b| {
                if b.starts_with(br#"  "death."#) {
                    b.split_once_str(br#": ""#)
                        .map(|(_, snd)| &snd[..snd.rfind(b"\"").unwrap()])
                        .and_then(|s| {
                            let victim = s.find(b"%1$s")?;

                            let mut first = (victim, DeathMessageComponent::Victim);
                            let mut second = (s.len(), DeathMessageComponent::Empty);
                            let mut third = (s.len(), DeathMessageComponent::Empty);

                            if let Some(attacker) = s.find(b"%2$s") {
                                if attacker < victim {
                                    second = first;
                                    first = (attacker, DeathMessageComponent::Attacker);
                                } else {
                                    second = (attacker, DeathMessageComponent::Attacker);
                                }
                            }

                            if let Some(weapon) = s.find(b"%3$s") {
                                if matches!(second.1, DeathMessageComponent::Empty) {
                                    if weapon < first.0 {
                                        second = first;
                                        first = (weapon, DeathMessageComponent::Weapon);
                                    } else {
                                        second = (weapon, DeathMessageComponent::Weapon);
                                    }
                                } else {
                                    if weapon < first.0 {
                                        third = second;
                                        second = first;
                                        first = (weapon, DeathMessageComponent::Weapon);
                                    } else if weapon < second.0 {
                                        third = second;
                                        second = (weapon, DeathMessageComponent::Weapon);
                                    } else {
                                        third = (weapon, DeathMessageComponent::Weapon);
                                    }
                                }
                            }

                            let ret = (
                                Box::leak(Box::<[u8]>::from(&s[..first.0])) as &'static [u8],
                                first.1,
                                if first.0 + 4 < s.len() {
                                    {
                                        Box::leak(Box::<[u8]>::from(&s[first.0 + 4..second.0]))
                                            as &'static [u8]
                                    }
                                } else {
                                    b"".as_slice()
                                },
                                second.1,
                                if second.0 + 4 < s.len() {
                                    Box::leak(Box::<[u8]>::from(&s[second.0 + 4..third.0]))
                                        as &'static [u8]
                                } else {
                                    b"".as_slice()
                                },
                                third.1,
                                if third.0 + 4 < s.len() {
                                    Box::leak(Box::<[u8]>::from(&s[third.0 + 4..]))
                                        as &'static [u8]
                                } else {
                                    b"".as_slice()
                                },
                            );

                            Some(ret)
                        })
                } else {
                    None
                }
            })
            .collect::<Box<_>>();

    let _ = DEATH_MESSAGES.get_or_init(|| Box::leak(death_messages));

    let log_ingester = tokio::task::spawn(async move {
        let input = tokio::fs::OpenOptions::new()
            .read(true)
            .open(path)
            .await
            .unwrap();
        let mut input = BufReader::new(input);
        let mut buf = Vec::with_capacity(16384);

        while let Ok(n) = input.read_until(b'\n', &mut buf).await {
            if n == 0 {
                continue;
            }

            let _ = {
                let s = buf[..n].to_str_lossy().chars().collect::<Vec<_>>();
                let mut s = s.as_slice();

                while s.len() > 2000 {
                    if let Some(idx) = s
                        .iter()
                        .enumerate()
                        .rev()
                        .skip(s.len() - 2000)
                        .find(|(_, c)| **c == '\n')
                        .map(|(idx, _)| idx)
                    {
                        if console_webhook
                            .execute(
                                &http,
                                false,
                                ExecuteWebhook::new()
                                    .username("Console")
                                    .content(s[..idx].iter().collect::<String>()),
                            )
                            .await
                            .is_err()
                        {
                            break;
                        }

                        s = &s[idx..];
                    } else {
                        if console_webhook
                            .execute(
                                &http,
                                false,
                                ExecuteWebhook::new()
                                    .username("Console")
                                    .content(s[..2000].iter().collect::<String>()),
                            )
                            .await
                            .is_err()
                        {
                            break;
                        }

                        s = &s[2000..];
                    }
                }

                console_webhook
                    .execute(
                        &http,
                        true,
                        ExecuteWebhook::new()
                            .username("Console")
                            .content(s.iter().collect::<String>()),
                    )
                    .await
            };

            let parsed = {
                let parser = Log::parser();
                parser.parse(&buf[..n - 1]).into_result()
            };

            let (log, span) = match parsed {
                Ok(parsed) => parsed,
                Err(e) => {
                    eprintln!("error: {:?}", e);
                    buf.clear();
                    continue;
                }
            };

            eprintln!("{log:?}");
            match log {
                Log::Chat(ChatLog {
                    sender, message, ..
                }) => {
                    let sender: &str = &sender.to_str_lossy();

                    let avatar: Cow<'_, str> = if sender == "[Server]" {
                        Cow::Borrowed("https://skinatar.firstdark.dev/avatar/Console")
                    } else {
                        Cow::Owned(format!("https://skinatar.firstdark.dev/avatar/{sender}"))
                    };

                    let _ = webhook
                        .execute(
                            &http,
                            false,
                            ExecuteWebhook::new()
                                .username(sender)
                                .avatar_url(avatar)
                                .content(message.to_str_lossy()),
                        )
                        .await;
                }
                Log::Join(JoinLog { player, .. }) => {
                    let sender: &str = &player.to_str_lossy();

                    let avatar = format!("https://skinatar.firstdark.dev/avatar/{sender}");

                    let _ = webhook
                        .execute(
                            &http,
                            false,
                            ExecuteWebhook::new()
                                .username(sender)
                                .avatar_url(&avatar)
                                .embed(
                                    CreateEmbed::new().author(
                                        CreateEmbedAuthor::new(format!("{sender} joined"))
                                            .icon_url(&avatar),
                                    ),
                                ),
                        )
                        .await;
                }
                Log::Leave(LeaveLog { player, .. }) => {
                    let sender: &str = &player.to_str_lossy();

                    let avatar = format!("https://skinatar.firstdark.dev/avatar/{sender}");

                    let _ = webhook
                        .execute(
                            &http,
                            false,
                            ExecuteWebhook::new()
                                .username(sender)
                                .avatar_url(&avatar)
                                .embed(
                                    CreateEmbed::new().author(
                                        CreateEmbedAuthor::new(format!("{sender} left"))
                                            .icon_url(&avatar),
                                    ),
                                ),
                        )
                        .await;
                }
                Log::Advancement(AdvancementLog { player, .. }) => {
                    let sender: &str = &player.to_str_lossy();

                    let avatar = format!("https://skinatar.firstdark.dev/avatar/{sender}");

                    let _ = webhook
                        .execute(
                            &http,
                            false,
                            ExecuteWebhook::new()
                                .username(sender)
                                .avatar_url(&avatar)
                                .embed(
                                    CreateEmbed::new().author(
                                        CreateEmbedAuthor::new(
                                            buf[span.into_range()].to_str_lossy(),
                                        )
                                        .icon_url(&avatar),
                                    ),
                                ),
                        )
                        .await;
                }
                Log::Death(DeathLog { victim, .. }) => {
                    let sender: &str = &victim.to_str_lossy();

                    let avatar = format!("https://skinatar.firstdark.dev/avatar/{sender}");

                    let _ = webhook
                        .execute(
                            &http,
                            false,
                            ExecuteWebhook::new()
                                .username(sender)
                                .avatar_url(&avatar)
                                .embed(
                                    CreateEmbed::new().author(
                                        CreateEmbedAuthor::new(
                                            buf[span.into_range()].to_str_lossy(),
                                        )
                                        .icon_url(&avatar),
                                    ),
                                ),
                        )
                        .await;
                }
                _ => (),
            }

            buf.clear();
        }

        Ok::<(), Error>(())
    });

    let bot = tokio::task::spawn(async move { bot::start_bot().await.unwrap() });

    let mut signals = Signals::new([Signal::Term, Signal::Quit, Signal::Int])?;
    if signals.next().await.is_some() {
        eprintln!("Stopping server.");
        println!("stop");
    }

    bot.abort();
    log_ingester.abort();

    Ok(())
}
