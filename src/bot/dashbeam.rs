use std::{sync::atomic::Ordering, time::Duration};

use dashbeam_engine::{ReceiveOptions, SendOptions};
use eyre::bail;
use iroh::RelayMode;
use poise::CreateReply;
use tempfile::tempdir;

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

    let store = tempdir()?;
    let service = match dashbeam_engine::NodeService::start(
        store.path(),
        RelayMode::Default,
        dashbeam_engine::DiscoveryModeOption::Default,
        dashbeam_engine::Discoverability::Off,
        None,
    )
    .await
    {
        Ok(service) => service,
        Err(e) => bail!("Failed to start service: {e:?}"),
    };

    let Ok(result) = dashbeam_engine::send::start_share_items(
        vec![path.clone()],
        SendOptions {
            ticket_type: dashbeam_engine::AddrInfoOptions::RelayAndAddresses,
            ..Default::default()
        },
        &None,
        None,
    )
    .await
    else {
        bail!("Could not send file");
    };

    let response = ctx.send(CreateReply::default().ephemeral(true).content(format!(
        "Requested content from path {path:?}\nTicket: [{ticket}](<https://app.dashbeam.net/receive?ticket={ticket}>)",
        ticket = result.ticket
    )))
    .await?;
    ctx.defer_ephemeral().await?;

    let peers = result.completed_peers.clone();

    match tokio::time::timeout(Duration::from_secs(60), async move {
        while peers.load(Ordering::Relaxed) < 1 {
            tokio::time::sleep(Duration::from_secs(1)).await;
        }
    })
    .await
    {
        Ok(_) => {
            response
                .edit(
                    ctx,
                    CreateReply::default().ephemeral(true).content(format!(
                        "File received by {} peer(s)",
                        result.completed_peers.load(Ordering::Relaxed)
                    )),
                )
                .await?;
        }
        Err(_) => {
            response
                .edit(
                    ctx,
                    CreateReply::default()
                        .ephemeral(true)
                        .content("DashBeam upload service timed out".to_string()),
                )
                .await?;
        }
    };

    let Ok(()) = service.shutdown().await else {
        bail!("Could not shutdown service");
    };

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
