mod bot;
mod interface;
mod jar;
mod parsing;

use crate::parsing::*;
use async_signal::{Signal, Signals};
use eyre::{bail, eyre};
use rustyline::error::ReadlineError;
use std::{
    borrow::Cow,
    collections::HashMap,
    fs::OpenOptions,
    io::Read,
    path::{Path, PathBuf},
    process::Stdio,
    sync::{Arc, OnceLock},
    time::Duration,
};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use zip::ZipArchive;

use bstr::ByteSlice;
use chumsky::prelude::*;
use poise::serenity_prelude::{
    CreateEmbed, CreateEmbedAuthor, ExecuteWebhook, Http, Webhook, colours, futures::StreamExt,
};

type Error = eyre::Error;
type Result<T, E = Error> = eyre::Result<T, E>;

static LANG: OnceLock<HashMap<&'static str, &'static str>> = OnceLock::new();

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

static ADVANCEMENTS: OnceLock<HashMap<&'static str, &'static str>> = OnceLock::new();

static COMMAND_CHANNEL: OnceLock<flume::Sender<Box<[u8]>>> = OnceLock::new();

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

        discord_operator_role_id, "DISCORD_OPERATOR_ROLE_ID", u64,
        "DISCORD_OPERATOR_ROLE_ID should be set to a Discord role ID";

        server_directory, "SERVER_DIRECTORY", String,
        "SERVER_DIRECTORY should be set to the path to the server's root directory";

        language?, "GAME_LANGUAGE", String,
        r#"GAME_LANGUAGE ("en_us" by default) should be set to the language the server is running"#;
    }
}

pub fn server_directory() -> PathBuf {
    PathBuf::from(crate::env::server_directory())
}

pub fn language() -> String {
    crate::env::language().unwrap_or_else(|| String::from("en_us"))
}

#[tokio::main]
async fn main() -> Result<()> {
    // console_subscriber::init();
    color_eyre::install()?;

    let _ = dotenvy::dotenv();

    if env::any_set() {
        env::assert_env_vars();
    } else {
        eprintln!("{}", env::gen_help());
        std::process::exit(1);
    }

    let (signal_fin_tx, signal_fin_rx) = tokio::sync::oneshot::channel::<()>();
    let mut signals = Signals::new([Signal::Term, Signal::Quit, Signal::Int])?;
    tokio::task::spawn(async move {
        signals.next().await;
        eprintln!("Received exit signal");
        let _ = signal_fin_tx.send(());
    });

    let token = env::discord_bot_token();

    let http = Http::new(&token);
    let webhook = Webhook::from_url(&http, &crate::env::discord_webhook_url()).await?;

    let mut join_set = tokio::task::JoinSet::<Result<()>>::new();

    let (logger, log_to_console) = {
        let (tx, rx) = flume::unbounded::<Box<str>>();
        let http = Http::new(&token);
        let console_webhook =
            Webhook::from_url(&http, &crate::env::discord_console_webhook_url()).await?;

        let logger = tokio::task::spawn(async move {
            let mut buf = String::with_capacity(4096);

            while rx.sender_count() > 0 || !rx.is_empty() {
                if !buf.is_empty() {
                    let s = buf.chars().collect::<Vec<_>>();
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
                                        .avatar_url("https://skinatar.firstdark.dev/avatar/Console")
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
                                        .avatar_url("https://skinatar.firstdark.dev/avatar/Console")
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

                    let _ = console_webhook
                        .execute(
                            &http,
                            true,
                            ExecuteWebhook::new()
                                .username("Console")
                                .avatar_url("https://skinatar.firstdark.dev/avatar/Console")
                                .content(s.iter().collect::<String>()),
                        )
                        .await;

                    buf.clear();
                }

                if let Ok(msg) = rx.recv_async().await {
                    buf.push_str(&msg);
                }

                while let Ok(msg) = rx.try_recv() {
                    buf.push_str(&msg);
                }
            }
        });

        (logger, tx)
    };

    interface::LIST_SENDER
        .set(tokio::sync::broadcast::channel(16).0)
        .map_err(|e| eyre!("Could not set LIST_SENDER to a broadcast channel sender: {e:?}"))?;

    let mut args = std::env::args();
    let binary_name = args.next().unwrap_or_else(|| String::from("gluemc"));

    eprintln!("Starting Discord bot");
    let (bot_started_tx, bot_started_rx) = tokio::sync::oneshot::channel::<()>();
    join_set.spawn(async move { bot::start_bot(bot_started_tx).await });
    bot_started_rx.await?;

    eprintln!("Starting server");
    webhook
        .execute(
            &http,
            false,
            ExecuteWebhook::new()
                .username("Console")
                .avatar_url("https://skinatar.firstdark.dev/avatar/Console")
                .embed(
                    CreateEmbed::new()
                        .author(CreateEmbedAuthor::new("Starting server"))
                        .colour(colours::branding::GREEN),
                ),
        )
        .await?;

    let mut process = {
        let Some(cmd_name) = args.next() else {
            println!("Usage: {binary_name} <command>");
            std::process::exit(1);
        };

        let mut process = tokio::process::Command::new(cmd_name);
        for arg in args {
            process.arg(arg);
        }

        process
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()?
    };

    let Some(stdout) = process.stdout.take() else {
        bail!("Could not get child stdout");
    };

    let Some(mut stdin) = process.stdin.take() else {
        bail!("Could not get child stdin")
    };

    let (tx, rx) = flume::unbounded();
    COMMAND_CHANNEL.set(tx).unwrap();

    join_set.spawn(async move {
        while let Ok(msg) = rx.recv_async().await {
            stdin.write_all(&msg).await?;
            stdin.write_u8(b'\n').await?;
            stdin.flush().await?;
        }

        Ok(())
    });

    let log_reader = tokio::task::spawn(async move {
        let http = Http::new(&token);
        let webhook = Webhook::from_url(&http, &crate::env::discord_webhook_url()).await?;

        let mut input = BufReader::new(stdout);
        let mut buf = Vec::with_capacity(16384);

        while let Ok(n) = input.read_until(b'\n', &mut buf).await {
            if n == 0 {
                continue;
            }

            let s = buf[..n].to_str_lossy();
            print!("{s}");

            log_to_console.send(s.into())?;

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

            match &log {
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
                Log::List(ListUuidsLog { players, max }) => {
                    let tx = interface::LIST_SENDER.get().unwrap();
                    if tx.receiver_count() == 0 {
                        continue;
                    }

                    let mut owned = Vec::with_capacity(players.len());
                    for player in players {
                        owned.push(OwnedPlayerData::try_from(player)?);
                    }

                    let owned: Arc<[OwnedPlayerData]> = owned.into();
                    let _ = tx.send(ListData {
                        players: owned,
                        max: *max,
                    });
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
                                    CreateEmbed::new()
                                        .author(
                                            CreateEmbedAuthor::new(format!("{sender} joined"))
                                                .icon_url(&avatar),
                                        )
                                        .colour(colours::branding::GREEN),
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
                                    CreateEmbed::new()
                                        .author(
                                            CreateEmbedAuthor::new(format!("{sender} left"))
                                                .icon_url(&avatar),
                                        )
                                        .colour(colours::branding::RED),
                                ),
                        )
                        .await;
                }
                Log::Advancement(AdvancementLog {
                    player,
                    advancement,
                    ..
                }) => {
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
                                    CreateEmbed::new()
                                        .author(
                                            CreateEmbedAuthor::new(
                                                buf[span.into_range()].to_str_lossy(),
                                            )
                                            .icon_url(&avatar),
                                        )
                                        .description(
                                            ADVANCEMENTS
                                                .get()
                                                .and_then(|adv| {
                                                    advancement
                                                        .to_str()
                                                        .ok()
                                                        .and_then(|s| adv.get(s))
                                                        .copied()
                                                })
                                                .unwrap_or_default(),
                                        )
                                        .colour(colours::branding::YELLOW),
                                ),
                        )
                        .await;
                }
                Log::Starting(StartingLog { version, .. }) => {
                    let version = version.to_str_lossy().into_owned();

                    tokio::spawn(async move {
                        let lang_file_name = {
                            let mut name = language();
                            name.push_str(".json");
                            name
                        };

                        eprintln!("Looking for language files named {}", lang_file_name);
                        let mut buf = Vec::new();

                        let mut death_messages = Vec::new();
                        let mut advancements = HashMap::new();

                        let mut full_lang: HashMap<&'static str, &'static str> = HashMap::new();

                        serde_json::from_slice::<HashMap<String, String>>(
                            &reqwest::get(
                                format!("https://assets.mcasset.cloud/{version}/assets/minecraft/lang/{lang_file_name}")
                            )
                            .await?
                            .bytes()
                            .await?
                        ).unwrap_or_default().into_iter().for_each(|(k, v)| {
                            full_lang.insert(k.leak(), v.leak());
                        });

                        let mods_folder = server_directory().join("mods");
                        if let Ok(mod_paths) = jar::files(&mods_folder) {
                            for path in mod_paths {
                                let file = OpenOptions::new().read(true).open(&path)?;
                                let mut archive = ZipArchive::new(file)?;

                                for i in 0..archive.len() {
                                    let mut file = archive.by_index(i)?;

                                    if !file.is_file() {
                                        continue;
                                    }

                                    if let Some(name) = file.enclosed_name()
                                        && name.file_name().is_some_and(|n| *n == *lang_file_name)
                                    {
                                        buf.clear();
                                        file.read_to_end(&mut buf)?;

                                        serde_json::from_slice::<HashMap<String, String>>(&buf)
                                            .unwrap_or_default()
                                            .into_iter()
                                            .for_each(|(k, v)| {
                                                full_lang.insert(k.leak(), v.leak());
                                            });
                                    }
                                }
                            }
                        }

                        for (k, v) in &full_lang {
                            if k.starts_with("death.") {
                                let Some(victim) = v.find("%1$s") else {
                                    continue;
                                };

                                let mut first = (victim, DeathMessageComponent::Victim);
                                let mut second = (v.len(), DeathMessageComponent::Empty);
                                let mut third = (v.len(), DeathMessageComponent::Empty);

                                if let Some(attacker) = v.find("%2$s") {
                                    if attacker < victim {
                                        second = first;
                                        first = (attacker, DeathMessageComponent::Attacker);
                                    } else {
                                        second = (attacker, DeathMessageComponent::Attacker);
                                    }
                                }

                                if let Some(weapon) = v.find("%3$s") {
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

                                death_messages.push((
                                    (Box::leak(Box::<str>::from(&v[..first.0])) as &'static str)
                                        .as_bytes(),
                                    first.1,
                                    if first.0 + 4 < v.len() {
                                        {
                                            Box::leak(Box::<str>::from(&v[first.0 + 4..second.0]))
                                                as &'static str
                                        }
                                    } else {
                                        ""
                                    }
                                    .as_bytes(),
                                    second.1,
                                    if second.0 + 4 < v.len() {
                                        Box::leak(Box::<str>::from(&v[second.0 + 4..third.0]))
                                            as &'static str
                                    } else {
                                        ""
                                    }
                                    .as_bytes(),
                                    third.1,
                                    if third.0 + 4 < v.len() {
                                        Box::leak(Box::<str>::from(&v[third.0 + 4..]))
                                            as &'static str
                                    } else {
                                        ""
                                    }
                                    .as_bytes(),
                                ));
                            } else if k.starts_with("advancements.")
                                && let Some(prefix) = k.strip_suffix(".title")
                            {
                                let mut desc_key = prefix.to_string();
                                desc_key.push_str(".description");
                                if let Some(desc) = full_lang.get(&*desc_key) {
                                    advancements.insert(*v, *desc);
                                }
                            }
                        }

                        let death_len = death_messages.len();
                        let _ = DEATH_MESSAGES.get_or_init(|| Box::leak(death_messages.into()));
                        let advancement_len = advancements.len();
                        let _ = ADVANCEMENTS.get_or_init(|| advancements);
                        let full_len = full_lang.len();
                        let _ = LANG.get_or_init(|| full_lang);
                        eprintln!(
                            "Initialized {} death messages and {} advancements from {} lang entries.",
                            death_len, advancement_len, full_len
                        );

                        Ok::<_, Error>(())
                    });
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
                                    CreateEmbed::new()
                                        .author(
                                            CreateEmbedAuthor::new(
                                                buf[span.into_range()].to_str_lossy(),
                                            )
                                            .icon_url(&avatar),
                                        )
                                        .colour(colours::branding::RED),
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

    let (input_fin_tx, input_fin_rx) = tokio::sync::oneshot::channel::<()>();
    std::thread::spawn(|| {
        let mut editor = rustyline::DefaultEditor::new().unwrap();

        loop {
            match editor.readline("") {
                Ok(line) => {
                    let _ = editor.add_history_entry(line.as_str());
                    if let Err(e) = command_sync(line.as_bytes()) {
                        eprintln!("Error sending command: {e:?}");
                    }
                }
                Err(ReadlineError::Interrupted) => {
                    eprintln!("Received CTRL+C, exiting.");
                    break;
                }
                Err(ReadlineError::Eof) => {
                    eprintln!("Received CTRL+D, exiting.");
                    break;
                }
                Err(e) => eprintln!("Error reading line: {e:?}"),
            }
        }

        let _ = input_fin_tx.send(());
    });

    tokio::select! {
        _ = input_fin_rx => {}
        _ = signal_fin_rx => {}
        Ok(_) = process.wait() => {}
    }

    if !matches!(process.try_wait(), Ok(Some(_))) {
        eprintln!("Stopping server");
        webhook
            .execute(
                &http,
                false,
                ExecuteWebhook::new()
                    .username("Console")
                    .avatar_url("https://skinatar.firstdark.dev/avatar/Console")
                    .embed(
                        CreateEmbed::new()
                            .author(CreateEmbedAuthor::new("Stopping server"))
                            .colour(colours::branding::RED),
                    ),
            )
            .await?;
        command(*b"stop").await?;
        let _ = process.wait().await;
    }

    eprintln!("Stopping wrapper");

    let _ = process.wait().await;
    tokio::time::sleep(Duration::from_secs(1)).await;
    log_reader.abort();
    logger.await?;
    join_set.abort_all();

    eprintln!("Stopped wrapper");

    Ok(())
}

pub async fn command(s: impl Into<Box<[u8]>>) -> Result<()> {
    COMMAND_CHANNEL.get().unwrap().send_async(s.into()).await?;
    Ok(())
}

pub fn command_sync(s: impl Into<Box<[u8]>>) -> Result<()> {
    COMMAND_CHANNEL.get().unwrap().send(s.into())?;
    Ok(())
}

pub trait SafeJoin {
    fn safe_join<P: AsRef<Path>>(&self, path: P) -> Result<PathBuf>;
}

impl SafeJoin for Path {
    fn safe_join<P: AsRef<Path>>(&self, path: P) -> Result<PathBuf> {
        let new = self.join(path).canonicalize()?;
        if !new.starts_with(self.canonicalize()?) {
            bail!("Attempted traversal above root {self:?}");
        }

        Ok(new)
    }
}
