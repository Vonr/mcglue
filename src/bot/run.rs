use poise::serenity_prelude::{self as serenity, *};

use crate::{Error, bot::Context};

#[poise::command(slash_command)]
pub async fn run(
    ctx: Context<'_>,
    #[description = "Command"] command: String,
) -> Result<(), Error> {
    println!("{command}");
    ctx.reply("Ran command").await?;
    Ok(())
}
