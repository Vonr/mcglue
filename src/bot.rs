mod tpo;

use std::{
    borrow::Cow,
    fmt::Display,
    path::{Path, PathBuf},
};

use poise::serenity_prelude::{self as serenity, CacheHttp, GatewayIntents, RoleId};
use serde::Deserialize;
use uuid::Uuid;

use crate::{Error, Result};

pub struct Data {
    pub server_directory: Box<Path>,
    pub operator_role_id: RoleId,
}

pub type Context<'a> = poise::Context<'a, Data, Error>;

pub async fn start_bot() -> Result<()> {
    let token = crate::env::discord_bot_token();
    let intents = GatewayIntents::non_privileged()
        | GatewayIntents::MESSAGE_CONTENT
        | GatewayIntents::GUILD_MESSAGES;

    let framework = poise::Framework::builder()
        .options(poise::FrameworkOptions {
            commands: vec![tpo::tpo()],
            event_handler: |ctx, event, framework, data| {
                Box::pin(event_handler(ctx, event, framework, data))
            },
            ..Default::default()
        })
        .setup(|ctx, _ready, framework| {
            Box::pin(async move {
                poise::builtins::register_globally(ctx, &framework.options().commands).await?;
                Ok(Data {
                    server_directory: PathBuf::from(crate::env::server_directory()).into(),
                    operator_role_id: crate::env::discord_operator_role_id().into(),
                })
            })
        })
        .build();

    let client = serenity::ClientBuilder::new(token, intents)
        .framework(framework)
        .await;

    client.unwrap().start().await.unwrap();

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
        }
        serenity::FullEvent::Message { new_message } => {
            if !new_message.author.bot && new_message.thread.is_none() {
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

                    println!(r#"tellraw @a {{"text":{:?}}}"#, content);
                } else if new_message.channel_id.get() == crate::env::discord_console_channel_id() {
                    println!("{}", &new_message.content);
                }
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
            .reply("You do not have the required role to run this command.")
            .await;

        Ok(false)
    }
}
