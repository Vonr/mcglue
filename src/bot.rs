mod crash;
mod files;
#[cfg(feature = "iroh")]
mod iroh;
mod list;
mod nbtq;
mod tpo;

use std::{
    borrow::Cow,
    fmt::Display,
    path::{Path, PathBuf},
    str::FromStr,
    sync::Arc,
};

use parking_lot::Mutex;
use poise::{CreateReply, FrameworkError, serenity_prelude::*};
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
    let token = Token::from_str(&crate::env::discord_bot_token())?;
    let intents = GatewayIntents::non_privileged()
        | GatewayIntents::MESSAGE_CONTENT
        | GatewayIntents::GUILD_MESSAGES;

    let options = poise::FrameworkOptions {
        commands: vec![
            crash::crash(),
            tpo::tpo(),
            #[cfg(feature = "iroh")]
            iroh::download(),
            #[cfg(feature = "iroh")]
            iroh::upload(),
            files::delete(),
            list::list(),
            nbtq::nbtq(),
        ],
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
    };

    let mut client = ClientBuilder::new(token, intents)
        .framework(Box::new(poise::Framework::new(options)))
        .event_handler(Arc::new(McglueEventHandler))
        .data(
            Data {
                bot_start_notifier: Mutex::new(Some(bot_start_notifier)),
                server_directory: crate::server_directory().into(),
                operator_role_id: crate::env::discord_operator_role_id().into(),
            }
            .into(),
        )
        .await?;

    client.start().await?;

    Ok(())
}

struct McglueEventHandler;

#[async_trait]
impl EventHandler for McglueEventHandler {
    async fn dispatch(&self, context: &poise::serenity_prelude::Context, event: &FullEvent) {
        match event {
            FullEvent::Ready { data_about_bot, .. } => {
                eprintln!("Logged in as {}", data_about_bot.user.name);
                context
                    .data_ref::<Data>()
                    .bot_start_notifier
                    .lock()
                    .take()
                    .unwrap()
                    .send(())
                    .unwrap();
            }
            FullEvent::Message { new_message, .. }
                if !new_message.author.bot() && new_message.thread.is_none() =>
            {
                if new_message.channel_id.get() == crate::env::discord_channel_id() {
                    const PREFIX: &str = "[Discord] ";
                    let author = new_message
                        .author_nick(&context.http)
                        .await
                        .map(Cow::Owned)
                        .unwrap_or_else(|| new_message.author.display_name().into());

                    let mut content = String::with_capacity(
                        new_message.content.len() as usize + PREFIX.len() + author.len() + 3,
                    );

                    content.push_str(PREFIX);
                    content.push('<');
                    content.push_str(&author);
                    content.push_str("> ");
                    content.push_str(&new_message.content);

                    let _ = crate::command(
                        format!(r#"tellraw @a {{"text":{:?}}}"#, content).as_bytes(),
                    )
                    .await;
                } else if new_message.channel_id.get() == crate::env::discord_console_channel_id()
                    && new_message.member(&context.http).await.is_ok_and(|m| {
                        m.roles
                            .contains(&context.data_ref::<Data>().operator_role_id)
                    })
                {
                    let _ = crate::command(new_message.content.as_bytes()).await;
                }
            }
            _ => {}
        }
    }
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

async fn autocomplete_path<'ctx>(
    ctx: Context<'ctx>,
    partial: &'ctx str,
    condition: impl FnMut(&PathBuf) -> bool,
) -> CreateAutocompleteResponse<'ctx> {
    let mut response = CreateAutocompleteResponse::new();
    if !matches!(is_operator(ctx).await, Ok(true)) {
        return response;
    }

    let mut path = PathBuf::from(partial);
    if path
        .components()
        .any(|c| !matches!(c, std::path::Component::Normal(_)))
    {
        return response;
    }

    let Some(mut root) = crate::server_directory()
        .canonicalize()
        .ok()
        .and_then(|d| d.to_str().map(|s| s.to_string()))
    else {
        return response;
    };

    root.push('/');

    if matches!(std::fs::exists(&path), Ok(true)) {
        if !path.is_dir() {
            return response.add_choice(partial);
        }
    } else {
        if let Some(parent) = path.parent() {
            path = parent.to_path_buf();
        } else {
            path = PathBuf::from(&root);
        };
    }

    for e in WalkDir::new(path)
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
        .take(25)
    {
        response = response.add_choice(e);
    }

    response
}

pub async fn autocomplete_path_any<'ctx>(
    ctx: Context<'ctx>,
    partial: &'ctx str,
) -> CreateAutocompleteResponse<'ctx> {
    autocomplete_path(ctx, partial, |_| true).await
}

pub async fn autocomplete_path_nbt<'ctx>(
    ctx: Context<'ctx>,
    partial: &'ctx str,
) -> CreateAutocompleteResponse<'ctx> {
    autocomplete_path(ctx, partial, |e| {
        e.extension().is_some_and(|e| {
            e.to_str()
                .is_some_and(|e| matches!(e, "nbt" | "dat" | "snbt"))
        })
    })
    .await
}

pub async fn autocomplete_path_directory<'ctx>(
    ctx: Context<'ctx>,
    partial: &'ctx str,
) -> CreateAutocompleteResponse<'ctx> {
    autocomplete_path(ctx, partial, |e| e.is_dir()).await
}
