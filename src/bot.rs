mod run;

use std::borrow::Cow;

use poise::serenity_prelude::{self as serenity, CacheHttp, GatewayIntents};

use crate::{Error, Result};

pub struct Data {}

pub type Context<'a> = poise::Context<'a, Data, Error>;

pub async fn start_bot() -> Result<()> {
    let token = crate::env::discord_bot_token();
    let intents = GatewayIntents::non_privileged()
        | GatewayIntents::MESSAGE_CONTENT
        | GatewayIntents::GUILD_MESSAGES;

    let framework = poise::Framework::builder()
        .options(poise::FrameworkOptions {
            commands: vec![run::run()],
            event_handler: |ctx, event, framework, data| {
                Box::pin(event_handler(ctx, event, framework, data))
            },
            ..Default::default()
        })
        .setup(|ctx, _ready, framework| {
            Box::pin(async move {
                poise::builtins::register_globally(ctx, &framework.options().commands).await?;
                Ok(Data {})
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
