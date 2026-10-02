#![allow(unused_imports)]

use std::{
    fmt::Write as _,
    fs::{File, OpenOptions},
    io::{BufWriter, Cursor, Read, Write},
    num::NonZero,
    path::{Path, PathBuf},
    str::FromStr,
    time::{Duration, Instant},
};

use eyre::{ContextCompat, bail, ensure};
use humansize::SizeFormatter;
use poise::{CreateReply, serenity_prelude::*};
use walkdir::WalkDir;
use zip::{CompressionMethod, result::ZipError, write::SimpleFileOptions};

#[cfg(feature = "iroh")]
use iroh::{Endpoint, SecretKey, TransportAddr, protocol::Router};
#[cfg(feature = "iroh")]
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
#[cfg(feature = "iroh")]
use n0_future::{BufferedStreamExt, StreamExt};

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

    let components = [CreateComponent::ActionRow(CreateActionRow::buttons(vec![
        CreateButton::new("confirm")
            .label("Confirm")
            .style(ButtonStyle::Danger),
        CreateButton::new("cancel").label("Cancel"),
    ]))];

    let metadata = path.metadata()?;

    let mut count = 0;
    let size_formatter;

    let response = if metadata.is_dir() {
        let mut total_size = 0;
        for file in WalkDir::new(&path) {
            total_size += file?.metadata()?.len();
            count += 1;
        }
        size_formatter = SizeFormatter::new(total_size, humansize::BINARY);

        ctx.send(
            CreateReply::default()
                .ephemeral(true)
                .content(format!(
                    "Delete {count} files in {path:?} ({size_formatter})?"
                ))
                .components(&components),
        )
        .await?
    } else {
        size_formatter = SizeFormatter::new(metadata.len(), humansize::BINARY);
        ctx.send(
            CreateReply::default()
                .ephemeral(true)
                .content(format!("Delete {path:?} ({size_formatter})?"))
                .components(&components),
        )
        .await?
    };

    tokio::select! {
        Some(interaction) = response
        .message()
        .await?
        .id
        .collect_component_interactions(ctx.serenity_context()) => {
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

/// Download files from the server
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

    let download_file = |path: PathBuf| async move {
        let Some(mut name) = path.file_name().map(|s| s.to_string_lossy().into_owned()) else {
            bail!("requested file has no name");
        };

        let mut file = OpenOptions::new().read(true).open(&path)?;
        let metadata = file.metadata()?;
        let is_file = metadata.is_file();

        let mut buf = Vec::new();
        let size_limit = 10 << 20;

        let buf = if is_file {
            if metadata.len() > size_limit {
                bail!("Requested content too large");
            }
            buf.reserve(metadata.len() as usize);

            tokio::task::spawn_blocking(move || {
                file.read_to_end(&mut buf)?;
                Ok::<_, crate::Error>(buf)
            })
        } else {
            name.push_str(".zip");
            let path = path.clone();
            tokio::task::spawn_blocking(move || {
                zip_dir(
                    &mut buf,
                    &path,
                    CompressionMethod::Zstd,
                    size_limit as usize,
                )?;
                Ok::<_, crate::Error>(buf)
            })
        }
        .await??;

        ctx.send(
            CreateReply::default()
                .ephemeral(true)
                .content(format!(
                    "Requested {} of content from path {path:?}",
                    SizeFormatter::new(buf.len(), humansize::BINARY),
                ))
                .attachment(CreateAttachment::bytes(buf, name)),
        )
        .await?;

        Ok::<_, crate::Error>(())
    };

    #[allow(unused_variables)]
    if let Err(e) = download_file(path.clone()).await {
        cfg_select! {
            feature = "iroh" => {
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
                    "Requested {} of content from path {path:?}\nTicket: [{ticket}](<https://app.dashbeam.net/receive?ticket={ticket}>)",
                    SizeFormatter::new(total_size, humansize::BINARY),
                );
                let response = ctx
                    .send(
                        CreateReply::default()
                        .ephemeral(true)
                        .content(&message)
                        .components(&[CreateComponent::ActionRow(CreateActionRow::Buttons(
                                    vec![CreateButton::new("stop").label("Stop Sharing")].into(),
                        ))]),
                    )
                    .await?;
                ctx.defer_ephemeral().await?;

                tokio::select! {
                    Some(interaction) = response
                        .message()
                        .await?
                        .id
                        .collect_component_interactions(ctx.serenity_context()) => {
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
            }
            _ => bail!(e),
        }
    }

    Ok(())
}

#[cfg(feature = "iroh")]
#[poise::command(
    slash_command,
    guild_only,
    subcommands("upload_file", "upload_iroh"),
    check = "super::is_operator"
)]
pub async fn upload(_ctx: Context<'_>) -> Result<()> {
    Ok(())
}

/// Upload files to the server
#[cfg_attr(
    not(feature = "iroh"),
    poise::command(
        rename = "upload",
        slash_command,
        guild_only,
        check = "super::is_operator"
    )
)]
#[cfg_attr(
    feature = "iroh",
    poise::command(
        rename = "file",
        slash_command,
        guild_only,
        check = "super::is_operator"
    )
)]
pub async fn upload_file(
    ctx: Context<'_>,
    #[description = "File"] file: Attachment,
    #[description = "Path to upload into"]
    #[autocomplete = "super::autocomplete_path_directory"]
    path: String,
    #[description = "Whether to overwrite existing files, off by default"] overwrite: Option<bool>,
) -> Result<()> {
    let path = ctx.data().server_directory.safe_join(path)?;
    ensure!(path.is_dir(), "{path:?} is not a folder");

    ctx.defer_ephemeral().await?;

    let overwrite = overwrite.unwrap_or(false);

    let file_name = file.title.unwrap_or(file.filename);
    let target = path.join(file_name);
    ensure!(
        overwrite || !target.exists(),
        "{target:?} already exists and overwrite is off"
    );

    let fd = OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .open(&target)?;

    let start = Instant::now();
    let mut total_bytes = 0;

    let url = file.url;
    let mut response = reqwest::get(&*url).await?;
    let mut writer = BufWriter::new(fd);

    while let Some(chunk) = response.chunk().await? {
        total_bytes += chunk.len();
        if let Err(e) = writer.write_all(&chunk) {
            bail!("error getting {target:?} from {url}: {e}")
        }
    }

    let elapsed = start.elapsed().as_secs_f64();
    ctx.send(CreateReply::default().ephemeral(true).content(format!(
        "Received file totalling {} in {elapsed:.02}s ({}ps)",
        SizeFormatter::new(total_bytes, humansize::BINARY),
        SizeFormatter::new((total_bytes as f64 / elapsed) as u64, humansize::BINARY),
    )))
    .await?;

    Ok(())
}

#[cfg(feature = "iroh")]
#[poise::command(
    rename = "iroh",
    slash_command,
    guild_only,
    check = "super::is_operator"
)]
pub async fn upload_iroh(
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
                "Received {total_files} files totalling {} in {:.02}s ({}ps)",
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

#[cfg(feature = "iroh")]
async fn new_endpoint() -> Result<Endpoint> {
    let secret = SecretKey::generate();
    Ok(Endpoint::builder(iroh::endpoint::presets::N0)
        .alpns(vec![iroh_blobs::protocol::ALPN.to_vec()])
        .secret_key(secret)
        .relay_mode(iroh::RelayMode::Default)
        .bind()
        .await?)
}

fn zip_dir(
    buf: &mut Vec<u8>,
    src_dir: &Path,
    method: CompressionMethod,
    size_limit: usize,
) -> Result<()> {
    if !Path::new(src_dir).is_dir() {
        return Err(ZipError::FileNotFound.into());
    }

    let walkdir = WalkDir::new(src_dir);

    let mut writer = Cursor::new(buf);
    let mut zip = zip::ZipWriter::new(&mut writer);

    let options = SimpleFileOptions::default()
        .compression_method(method)
        .unix_permissions(0o755);

    for entry_result in walkdir.into_iter() {
        let entry = match entry_result {
            Ok(entry) => entry,
            Err(e) => {
                bail!("Error while traversing directory {src_dir:?}: {e}");
            }
        };

        let path = entry.path();
        let path_stripped = path.strip_prefix(src_dir)?;

        if path.is_file() {
            zip.start_file_from_path(path_stripped, options)?;
            let mut f = File::open(path)?;

            std::io::copy(&mut f, &mut zip)?;
        } else if !path_stripped.as_os_str().is_empty() {
            zip.add_directory_from_path(path_stripped, options)?;
        }

        if zip.get_ref().unwrap().get_ref().len() > size_limit {
            bail!("exceeded size limit");
        }
    }
    zip.finish()?;

    Ok(())
}
