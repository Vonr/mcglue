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
    subcommands("download", "upload")
)]
pub async fn dashbeam(_ctx: Context<'_>) -> Result<()> {
    Ok(())
}

/// Download files from the server via dashbeam
#[poise::command(slash_command, guild_only, check = "super::is_operator")]
pub async fn download(
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

    ctx.send(CreateReply::default().ephemeral(true).content(format!(
        "Requested content from path {path:?}\nTicket: [{ticket}](<https://app.dashbeam.net/receive?ticket={ticket}>)",
        ticket = result.ticket
    )))
    .await?;

    let peers = result.completed_peers.clone();

    match tokio::time::timeout(Duration::from_secs(60), async move {
        while peers.load(Ordering::Relaxed) < 1 {
            tokio::time::sleep(Duration::from_secs(1)).await;
        }
    })
    .await
    {
        Ok(_) => {
            ctx.send(CreateReply::default().ephemeral(true).content(format!(
                "File received by {} peer(s)",
                result.completed_peers.load(Ordering::Relaxed)
            )))
            .await?;
        }
        Err(_) => {
            ctx.send(
                CreateReply::default()
                    .ephemeral(true)
                    .content("Shutting down service due to timeout".to_string()),
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
pub async fn upload(
    ctx: Context<'_>,
    #[description = "DashBeam ticket"] ticket: String,
    #[description = "Path to the file or folder"]
    #[autocomplete = "super::autocomplete_path_any"]
    path: String,
) -> Result<()> {
    ctx.defer_ephemeral().await?;
    let path = ctx.data().server_directory.safe_join(path)?;

    let (_tx, rx) = tokio::sync::oneshot::channel();

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

    ctx.send(
        CreateReply::default()
            .ephemeral(true)
            .content(result.message.to_string()),
    )
    .await?;

    Ok(())
}
