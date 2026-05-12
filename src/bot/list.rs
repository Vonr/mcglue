use poise::CreateReply;

use super::Context;
use crate::{Result, interface};

/// Get a list of players
#[poise::command(slash_command, guild_only, check = "super::is_operator")]
pub async fn list(ctx: Context<'_>) -> Result<()> {
    let list = {
        let mut recv = interface::LIST_SENDER.get().unwrap().subscribe();
        interface::list().await?;
        recv.recv().await?
    };

    if list.players.is_empty() {
        ctx.send(
            CreateReply::default()
                .ephemeral(true)
                .content(format!("There are 0/{} players online.", list.max)),
        )
        .await?;
    } else {
        ctx.send(CreateReply::default().ephemeral(true).content(format!(
            "There are {}/{} players online: {}.",
            list.players.len(),
            list.max,
            list.players.iter().fold(String::new(), |mut acc, x| {
                if !acc.is_empty() {
                    acc.push_str(", ");
                }
                acc.push_str(&x.name);
                acc
            })
        )))
        .await?;
    }

    Ok(())
}
