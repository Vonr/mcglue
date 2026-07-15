mod crash;
mod download;
mod list;
mod nbtq;
mod tpo;

use std::{
    borrow::Cow,
    fmt::Display,
    path::{Path, PathBuf},
};

use parking_lot::Mutex;
use poise::{
    CreateReply, FrameworkError,
    serenity_prelude::{self as serenity, CacheHttp, GatewayIntents, RoleId},
};
use serde::Deserialize;
use uuid::Uuid;
use walkdir::WalkDir;

use crate::{Error, Result};

pub struct Data {
    pub bot_start_notifier: Mutex<Option<tokio::sync::oneshot::Sender<()>>>,
    pub server_directory: Box<Path>,
    pub operator_role_id: RoleId,
}

pub type Context<'a> = poise::Context<'a, Data, Error>;

pub async fn start_bot(bot_start_notifier: tokio::sync::oneshot::Sender<()>) -> Result<()> {
    let token = crate::env::discord_bot_token();
    let intents = GatewayIntents::non_privileged()
        | GatewayIntents::MESSAGE_CONTENT
        | GatewayIntents::GUILD_MESSAGES;

    let framework = poise::Framework::builder()
        .options(poise::FrameworkOptions {
            commands: vec![
                crash::crash(),
                tpo::tpo(),
                download::download(),
                list::list(),
                nbtq::nbtq(),
            ],
            event_handler: |ctx, event, framework, data| {
                Box::pin(event_handler(ctx, event, framework, data))
            },
            on_error: |error| {
                Box::pin(async move {
                    match error {
                        FrameworkError::Command { ctx, error, .. } => {
                            let error = error.to_string();
                            if let Err(e) = ctx
                                .send(CreateReply::default().ephemeral(true).content(error))
                                .await
                            {
                                eprintln!("Error while handling bot error: {}", e);
                            }
                        }
                        error => {
                            if let Err(e) = poise::builtins::on_error(error).await {
                                eprintln!("Error while handling bot error: {}", e);
                            }
                        }
                    }
                })
            },
            ..Default::default()
        })
        .setup(|ctx, _ready, framework| {
            Box::pin(async move {
                poise::builtins::register_globally(ctx, &framework.options().commands).await?;
                Ok(Data {
                    bot_start_notifier: Mutex::new(Some(bot_start_notifier)),
                    server_directory: crate::server_directory().into(),
                    operator_role_id: crate::env::discord_operator_role_id().into(),
                })
            })
        })
        .build();

    let client = serenity::ClientBuilder::new(token, intents)
        .framework(framework)
        .await;

    client?.start().await?;

    Ok(())
}

async fn event_handler(
    ctx: &serenity::Context,
    event: &serenity::FullEvent,
    _framework: poise::FrameworkContext<'_, Data, Error>,
    data: &Data,
) -> Result<(), Error> {
    match event {
        serenity::FullEvent::Ready { data_about_bot, .. } => {
            eprintln!("Logged in as {}", data_about_bot.user.name);
            data.bot_start_notifier
                .lock()
                .take()
                .unwrap()
                .send(())
                .unwrap();
        }
        serenity::FullEvent::Message { new_message }
            if !new_message.author.bot && new_message.thread.is_none() =>
        {
            if new_message.channel_id.get() == crate::env::discord_channel_id() {
                const PREFIX: &str = "[Discord] ";
                let author = new_message
                    .author_nick(ctx.http())
                    .await
                    .map(Cow::Owned)
                    .unwrap_or_else(|| new_message.author.display_name().into());

                let mut content = String::with_capacity(
                    new_message.content.len() + PREFIX.len() + author.len() + 3,
                );

                content.push_str(PREFIX);
                content.push('<');
                content.push_str(&author);
                content.push_str("> ");
                content.push_str(&new_message.content);

                crate::command(format!(r#"tellraw @a {{"text":{:?}}}"#, content).as_bytes())
                    .await?;
            } else if new_message.channel_id.get() == crate::env::discord_console_channel_id()
                && new_message
                    .member(&ctx.http)
                    .await
                    .is_ok_and(|m| m.roles.contains(&data.operator_role_id))
            {
                crate::command(new_message.content.as_bytes()).await?;
            }
        }
        _ => {}
    }

    Ok(())
}

pub async fn maybe_username_to_uuid<S>(s: &S) -> Result<Uuid>
where
    S: ?Sized,
    for<'a> &'a S: Display,
    Uuid: for<'a> TryFrom<&'a S>,
{
    if let Ok(uuid) = Uuid::try_from(s) {
        return Ok(uuid);
    }

    #[derive(Deserialize)]
    struct Response {
        id: Box<str>,
    }

    Ok(Uuid::parse_str(
        &reqwest::get(format!(
            "https://api.mojang.com/users/profiles/minecraft/{s}"
        ))
        .await?
        .error_for_status()?
        .json::<Response>()
        .await?
        .id,
    )?)
}

pub async fn is_operator(ctx: Context<'_>) -> Result<bool> {
    let Some(member) = ctx.author_member().await else {
        return Ok(false);
    };

    if member.roles.contains(&ctx.data().operator_role_id) {
        Ok(true)
    } else {
        let _ = ctx
            .send(
                CreateReply::default()
                    .ephemeral(true)
                    .content("You do not have the required role to run this command."),
            )
            .await;

        Ok(false)
    }
}

async fn autocomplete_path(
    ctx: Context<'_>,
    partial: &str,
    condition: impl FnMut(&PathBuf) -> bool,
) -> Vec<String> {
    if !matches!(is_operator(ctx).await, Ok(true)) {
        return Vec::new();
    }

    let mut path = PathBuf::from(partial);
    if path
        .components()
        .any(|c| !matches!(c, std::path::Component::Normal(_)))
    {
        return Vec::new();
    }

    let Some(mut root) = crate::server_directory()
        .canonicalize()
        .ok()
        .and_then(|d| d.to_str().map(|s| s.to_string()))
    else {
        return Vec::new();
    };

    root.push('/');

    if matches!(std::fs::exists(&path), Ok(true)) {
        if !path.is_dir() {
            return vec![partial.to_string()];
        }
    } else {
        if let Some(parent) = path.parent() {
            path = parent.to_path_buf();
        } else {
            path = PathBuf::from(&root);
        };
    }

    WalkDir::new(path)
        .max_depth(1)
        .into_iter()
        .filter_map(|e| e.ok())
        .filter_map(|e| e.path().canonicalize().ok())
        .filter(condition)
        .filter_map(|e| {
            e.to_str().map(|s| {
                let mut s = s.to_string();
                if let Some(stripped) = s.strip_prefix(&root) {
                    s = stripped.to_string();
                }
                if e.is_dir() {
                    s.push('/');
                }
                s
            })
        })
        .filter(|e| !e.starts_with('/') && e.contains(partial))
        .collect()
}

pub async fn autocomplete_path_any(ctx: Context<'_>, partial: &str) -> Vec<String> {
    autocomplete_path(ctx, partial, |_| true).await
}

pub async fn autocomplete_path_nbt(ctx: Context<'_>, partial: &str) -> Vec<String> {
    autocomplete_path(ctx, partial, |e| {
        e.extension().is_some_and(|e| {
            e.to_str()
                .is_some_and(|e| matches!(e, "nbt" | "dat" | "snbt"))
        })
    })
    .await
}
