use eyre::bail;
use poise::{CreateReply, serenity_prelude::CreateAttachment};
use std::fs::File;
use std::io::Cursor;
use std::path::Path;
use std::{fs::OpenOptions, io::Read};
use walkdir::WalkDir;
use zip::CompressionMethod;
use zip::result::ZipError;
use zip::write::SimpleFileOptions;

use super::Context;
use crate::{Result, SafeJoin};

/// Download a file from the server directory
#[poise::command(slash_command, guild_only, check = "super::is_operator")]
pub async fn download(
    ctx: Context<'_>,
    #[description = "Path to the file or folder"] path: String,
) -> Result<()> {
    let path = ctx.data().server_directory.safe_join(path)?;
    let Some(mut name) = path.file_name().map(|s| s.to_string_lossy().into_owned()) else {
        bail!("Requested file has no name");
    };

    let (bytes, is_file) = {
        let path = path.clone();
        tokio::task::spawn_blocking(move || {
            let mut file = OpenOptions::new().read(true).open(&path)?;
            let metadata = file.metadata()?;
            let is_file = metadata.is_file();

            let mut buf = Vec::with_capacity(metadata.len() as usize);

            if is_file {
                file.read_to_end(&mut buf)?;
            } else if metadata.is_dir() {
                zip_dir(&mut buf, &path, CompressionMethod::Zstd)?;
            }

            Ok::<_, crate::Error>((buf, is_file))
        })
        .await??
    };

    if !is_file {
        name.push_str(".zip");
    }

    ctx.send(
        CreateReply::default()
            .ephemeral(true)
            .attachment(CreateAttachment::bytes(bytes, name))
            .content(format!("Requested content from path {path:?}")),
    )
    .await?;

    Ok(())
}

fn zip_dir(buf: &mut Vec<u8>, src_dir: &Path, method: CompressionMethod) -> Result<()> {
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
    }
    zip.finish()?;

    Ok(())
}
