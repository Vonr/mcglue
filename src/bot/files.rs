use std::time::Duration;

use eyre::ensure;
use humansize::SizeFormatter;
use poise::{
    CreateReply,
    serenity_prelude::{CreateActionRow, CreateButton, CreateInteractionResponse},
};
use walkdir::WalkDir;

use super::Context;
use crate::{Result, SafeJoin};

/// Download files from the server via iroh-blobs
#[poise::command(slash_command, guild_only, check = "super::is_operator")]
pub async fn delete(
    ctx: Context<'_>,
    #[description = "Path to the file or folder"]
    #[autocomplete = "super::autocomplete_path_any"]
    path: String,
) -> Result<()> {
    let path = ctx.data().server_directory.safe_join(path)?;
    ensure!(path.exists(), "Requested file at {path:?} does not exist");

    let components = vec![CreateActionRow::Buttons(vec![
        CreateButton::new("confirm")
            .label("Confirm")
            .style(poise::serenity_prelude::ButtonStyle::Danger),
        CreateButton::new("cancel").label("Cancel"),
    ])];

    let metadata = path.metadata()?;

    let response;
    let mut count = 0;
    let size_formatter;

    if metadata.is_dir() {
        let mut total_size = 0;
        for file in WalkDir::new(&path) {
            total_size += file?.metadata()?.len();
            count += 1;
        }
        size_formatter = SizeFormatter::new(total_size, humansize::BINARY);

        response = ctx
            .send(
                CreateReply::default()
                    .ephemeral(true)
                    .content(format!(
                        "Delete {count} files in {path:?} ({size_formatter})?"
                    ))
                    .components(components),
            )
            .await?;
    } else {
        size_formatter = SizeFormatter::new(metadata.len(), humansize::BINARY);
        response = ctx
            .send(
                CreateReply::default()
                    .ephemeral(true)
                    .content(format!("Delete {path:?} ({size_formatter})?"))
                    .components(components),
            )
            .await?;
    }

    tokio::select! {
        Some(interaction) = response
        .message()
        .await?
        .await_component_interaction(ctx.serenity_context()) => {
            match interaction.data.custom_id.as_str() {
                "confirm" => {
                    if metadata.is_dir() {
                        std::fs::remove_dir_all(&path)?;
                        response
                            .edit(
                                ctx,
                                CreateReply::default()
                                .ephemeral(true)
                                .content(format!("Deleted {count} files in {path:?} ({size_formatter})"))
                                .components(vec![]),
                            )
                            .await?;
                    } else {
                        std::fs::remove_file(&path)?;
                        response
                            .edit(
                                ctx,
                                CreateReply::default()
                                .ephemeral(true)
                                .content(format!("Deleted {path:?} ({size_formatter})"))
                                .components(vec![]),
                            )
                            .await?;
                    }
                }
                "cancel" => {
                        response
                            .edit(
                                ctx,
                                CreateReply::default()
                                .ephemeral(true)
                                .content(format!("Cancelled deletion of {path:?}"))
                                .components(vec![]),
                            )
                            .await?;
                }
                _ => {}
            }

            interaction.create_response(ctx.http(), CreateInteractionResponse::Acknowledge).await?;
        }
        _ = tokio::time::sleep(Duration::from_mins(10)) => {
            response
                .edit(
                    ctx,
                    CreateReply::default()
                        .ephemeral(true)
                        .content("Delete interaction timed out".to_string())
                        .components(vec![]),
                )
                .await?;
        }
    }

    Ok(())
}
