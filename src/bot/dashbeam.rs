use std::{sync::atomic::Ordering, time::Duration};

use dashbeam_engine::{ReceiveOptions, SendOptions};
use eyre::bail;
use poise::{
    CreateReply,
    serenity_prelude::{CreateActionRow, CreateButton, CreateInteractionResponse},
};

use super::Context;
use crate::{Result, SafeJoin};

/// Send or receive files via dashbeam
#[poise::command(
    slash_command,
    guild_only,
    check = "super::is_operator",
    subcommands("get", "put")
)]
pub async fn dashbeam(_ctx: Context<'_>) -> Result<()> {
    Ok(())
}

/// Download files from the server via dashbeam
#[poise::command(slash_command, guild_only, check = "super::is_operator")]
pub async fn get(
    ctx: Context<'_>,
    #[description = "Path to the file or folder"]
    #[autocomplete = "super::autocomplete_path_any"]
    path: String,
) -> Result<()> {
    ctx.defer_ephemeral().await?;
    let path = ctx.data().server_directory.safe_join(path)?;

    let Ok(share) = dashbeam_engine::send::start_share_items(
        vec![path.clone()],
        SendOptions {
            ticket_type: dashbeam_engine::AddrInfoOptions::Relay,
            ..Default::default()
        },
        &None,
        None,
    )
    .await
    else {
        bail!("Could not send file");
    };

    let message = format!(
        "Requested content from path {path:?}\nTicket: [{ticket}](<https://app.dashbeam.net/receive?ticket={ticket}>)",
        ticket = share.ticket,
    );
    let response = ctx
        .send(
            CreateReply::default()
                .ephemeral(true)
                .content(&message)
                .components(vec![CreateActionRow::Buttons(vec![
                    CreateButton::new("stop").label("Stop Sharing"),
                ])]),
        )
        .await?;
    ctx.defer_ephemeral().await?;

    let peers = share.completed_peers.clone();

    let poll_response = response.clone();

    tokio::select! {
        Some(interaction) = response
        .message()
        .await?
        .await_component_interaction(ctx.serenity_context()) => {
            if interaction.data.custom_id == "stop" {
                response
                    .edit(
                        ctx,
                        CreateReply::default().ephemeral(true).content(format!(
                                "Sharing stopped after being received by {} peer(s)",
                                share.completed_peers.load(Ordering::Relaxed)
                        )).components(vec![]),
                    )
                    .await?;
                interaction.create_response(ctx.http(), CreateInteractionResponse::Acknowledge).await?;
            }
        }
        result = async move {
            let mut previous_peers = 0;
            loop {
                let peers = peers.load(Ordering::Relaxed);
                if peers != previous_peers {
                    previous_peers = peers;
                    poll_response
                        .edit(
                            ctx,
                            CreateReply::default()
                            .ephemeral(true)
                            .content(format!("{message}\nFile received by {peers} peer(s)"))
                        )
                        .await?;
                }

                tokio::time::sleep(Duration::from_secs(1)).await;
            }

            #[allow(unreachable_code)]
            Ok::<_, crate::Error>(())
        } => result?,
        _ = tokio::time::sleep(Duration::from_mins(10)) => {
            response
                .edit(
                    ctx,
                    CreateReply::default().ephemeral(true).content(format!(
                            "File timed out after being received by {} peer(s)",
                            share.completed_peers.load(Ordering::Relaxed)
                    )).components(vec![]),
                )
                .await?;
        }
    }

    Ok(())
}

/// Upload files to the server via dashbeam
#[poise::command(slash_command, guild_only, check = "super::is_operator")]
pub async fn put(
    ctx: Context<'_>,
    #[description = "DashBeam ticket"] ticket: String,
    #[description = "Path to the file or folder"]
    #[autocomplete = "super::autocomplete_path_any"]
    path: String,
) -> Result<()> {
    let path = ctx.data().server_directory.safe_join(path)?;

    let (_tx, rx) = tokio::sync::oneshot::channel();

    let response = ctx
        .send(
            CreateReply::default()
                .ephemeral(true)
                .content("Receiving file".to_string()),
        )
        .await?;
    ctx.defer_ephemeral().await?;

    let Ok(result) = dashbeam_engine::receive::download(
        ticket,
        ReceiveOptions {
            output_dir: Some(path),
            ..Default::default()
        },
        None,
        rx,
    )
    .await
    else {
        bail!("Could not receive file");
    };

    response
        .edit(
            ctx,
            CreateReply::default()
                .ephemeral(true)
                .content(result.message.to_string()),
        )
        .await?;

    Ok(())
}
