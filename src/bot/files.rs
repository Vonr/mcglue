use std::{num::NonZero, os::unix::fs::MetadataExt, path::PathBuf, str::FromStr, time::Duration};

use eyre::{ContextCompat, bail, ensure};
use humansize::SizeFormatter;
use iroh::{Endpoint, SecretKey, TransportAddr, protocol::Router};
use iroh_blobs::{
    BlobFormat, BlobsProtocol,
    api::{
        blobs::{
            AddPathOptions, AddProgressItem, ExportMode, ExportOptions, ExportProgressItem,
            ImportMode,
        },
        remote::GetProgressItem,
    },
    format::collection::Collection,
    get::{Stats, request::get_hash_seq_and_sizes},
    store::fs::FsStore,
    ticket::BlobTicket,
};
use n0_future::{BufferedStreamExt, StreamExt};
use poise::{
    CreateReply,
    serenity_prelude::{CreateActionRow, CreateButton, CreateInteractionResponse},
};
use walkdir::WalkDir;

use super::Context;
use crate::{Result, SafeJoin};

/// Download files from the server via iroh-blobs
#[poise::command(slash_command, guild_only, check = "super::is_operator")]
pub async fn download(
    ctx: Context<'_>,
    #[description = "Path to the file or folder"]
    #[autocomplete = "super::autocomplete_path_any"]
    path: String,
) -> Result<()> {
    ctx.defer_ephemeral().await?;
    let path = ctx.data().server_directory.safe_join(path)?;
    ensure!(path.exists(), "Requested file at {path:?} does not exist");

    let endpoint = new_endpoint().await?;
    let temp_dir = tempfile::tempdir()?;

    let store = FsStore::load(&temp_dir).await?;
    let blobs = BlobsProtocol::new(&store, None);
    let store = blobs.store();

    let root = path.parent().context("Could not get parent of path")?;
    let files = WalkDir::new(path.clone()).into_iter();
    let data_sources: Box<[(String, PathBuf)]> = files
        .map(|entry| {
            let entry = entry?;
            if !entry.file_type().is_file() {
                return Ok(None);
            }

            let path = entry.into_path();
            let name = path.strip_prefix(root)?.to_string_lossy().to_string();
            Ok(Some((name, path)))
        })
        .filter_map(Result::transpose)
        .collect::<Result<Box<[_]>>>()?;

    let names_and_tags = n0_future::stream::iter(data_sources)
        .map(|(name, path)| {
            let db = store.clone();
            async move {
                let import = db.add_path_with_opts(AddPathOptions {
                    path,
                    mode: ImportMode::TryReference,
                    format: BlobFormat::Raw,
                });

                let mut stream = import.stream().await;
                let mut item_size = 0;

                let temp_tag = loop {
                    let item = stream
                        .next()
                        .await
                        .context("import stream ended without a tag")?;
                    match item {
                        AddProgressItem::Size(size) => {
                            item_size = size;
                        }
                        AddProgressItem::Error(cause) => {
                            bail!("error importing {name:?}: {cause}");
                        }
                        AddProgressItem::Done(tt) => {
                            break tt;
                        }
                        _ => {}
                    }
                };

                Ok::<_, crate::Error>((name, temp_tag, item_size))
            }
        })
        .buffered_unordered(
            std::thread::available_parallelism()
                .map(NonZero::get)
                .unwrap_or(1),
        )
        .collect::<Vec<_>>()
        .await
        .into_iter()
        .collect::<Result<Box<_>>>()?;

    let mut total_size = 0;
    let mut collection = Collection::default();
    let mut tags = Vec::with_capacity(names_and_tags.len());
    for (name, tag, size) in names_and_tags {
        total_size += size;

        collection.push(name, tag.hash());
        tags.push(tag);
    }

    let tag = collection.store(store).await?;
    drop(tags);

    let router = Router::builder(endpoint)
        .accept(iroh_blobs::ALPN, blobs.clone())
        .spawn();
    let ep = router.endpoint();

    tokio::time::timeout(Duration::from_secs(30), ep.online()).await?;

    let hash = tag.hash();
    let mut addr = router.endpoint().addr();
    addr.addrs
        .retain(|addr| matches!(addr, TransportAddr::Relay(_)));

    let ticket = BlobTicket::new(addr, hash, BlobFormat::HashSeq);

    let message = format!(
        "Requested {} bytes of content from path {path:?}\nTicket: [{ticket}](<https://app.dashbeam.net/receive?ticket={ticket}>)",
        SizeFormatter::new(total_size, humansize::BINARY),
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

    tokio::select! {
        Some(interaction) = response
        .message()
        .await?
        .await_component_interaction(ctx.serenity_context()) => {
            if interaction.data.custom_id == "stop" {
                response
                    .edit(
                        ctx,
                        CreateReply::default()
                            .ephemeral(true)
                            .content("Sharing stopped".to_string())
                            .components(vec![]),
                    )
                    .await?;
                interaction.create_response(ctx.http(), CreateInteractionResponse::Acknowledge).await?;
            }
        }
        _ = tokio::time::sleep(Duration::from_mins(10)) => {
            response
                .edit(
                    ctx,
                    CreateReply::default()
                        .ephemeral(true)
                        .content("File timed out".to_string())
                        .components(vec![]),
                )
                .await?;
        }
    }

    drop(tag);

    tokio::time::timeout(Duration::from_secs(2), router.shutdown()).await??;
    drop(router);

    Ok(())
}

/// Upload files to the server via iroh-blobs
#[poise::command(slash_command, guild_only, check = "super::is_operator")]
pub async fn upload(
    ctx: Context<'_>,
    #[description = "iroh-blobs ticket"] ticket: String,
    #[description = "Path to upload into"]
    #[autocomplete = "super::autocomplete_path_directory"]
    path: String,
    #[description = "Whether to overwrite existing files, off by default"] overwrite: Option<bool>,
) -> Result<()> {
    let ticket = BlobTicket::from_str(&ticket)?;
    let path = ctx.data().server_directory.safe_join(path)?;

    ensure!(path.is_dir(), "{path:?} is not a folder");

    let overwrite = overwrite.unwrap_or(false);

    let response = ctx
        .send(
            CreateReply::default()
                .ephemeral(true)
                .content("Receiving file(s)...".to_string()),
        )
        .await?;
    ctx.defer_ephemeral().await?;

    let endpoint = new_endpoint().await?;
    let temp_dir = tempfile::tempdir()?;

    let store = FsStore::load(&temp_dir).await?;
    let blobs = BlobsProtocol::new(&store, None);
    let store = blobs.store();

    let hash_and_format = ticket.hash_and_format();
    let local = store.remote().local(hash_and_format).await?;

    let connection = endpoint
        .connect(ticket.addr().clone(), iroh_blobs::ALPN)
        .await?;
    let (_hash_seq, sizes) =
        get_hash_seq_and_sizes(&connection, &hash_and_format.hash, 1024 * 1024 * 32, None).await?;

    let total_files = sizes.len().saturating_sub(1) as u64;
    let get = store.remote().execute_get(connection, local.missing());

    let mut stats = Stats::default();
    let mut stream = get.stream();
    while let Some(item) = stream.next().await {
        match item {
            GetProgressItem::Error(cause) => {
                bail!("failed to get item: {}", cause.to_string());
            }
            GetProgressItem::Done(value) => {
                stats = value;
                break;
            }
            _ => {}
        }
    }

    let collection = Collection::load(hash_and_format.hash, store).await?;

    for (name, hash) in collection {
        let target = path.join(&name);
        ensure!(
            overwrite || !target.exists(),
            "{target:?} already exists and overwrite is off",
        );

        response
            .edit(
                ctx,
                CreateReply::default()
                    .ephemeral(true)
                    .content(format!("Receiving {name:?} with hash {hash}")),
            )
            .await?;

        let mut stream = store
            .export_with_opts(ExportOptions {
                hash,
                target,
                mode: ExportMode::TryReference,
            })
            .stream()
            .await;

        while let Some(item) = stream.next().await {
            if let ExportProgressItem::Error(cause) = item {
                bail!("error exporting {}: {}", name, cause);
            }
        }
    }

    response
        .edit(
            ctx,
            CreateReply::default().ephemeral(true).content(format!(
                "Received {total_files} files totalling {} bytes in {:.02}s ({}ps)",
                SizeFormatter::new(stats.payload_bytes_read, humansize::BINARY),
                stats.elapsed.as_secs_f64(),
                SizeFormatter::new(
                    (stats.payload_bytes_read as f64 / stats.elapsed.as_secs_f64()) as u64,
                    humansize::BINARY
                ),
            )),
        )
        .await?;

    endpoint.close().await;
    store.shutdown().await?;

    Ok(())
}

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
            total_size += file?.metadata()?.size();
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
        size_formatter = SizeFormatter::new(metadata.size(), humansize::BINARY);
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

async fn new_endpoint() -> Result<Endpoint> {
    let secret = SecretKey::generate();
    Ok(Endpoint::builder(iroh::endpoint::presets::N0)
        .alpns(vec![iroh_blobs::protocol::ALPN.to_vec()])
        .secret_key(secret)
        .relay_mode(iroh::RelayMode::Default)
        .bind()
        .await?)
}
