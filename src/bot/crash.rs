use eyre::eyre;
use poise::{CreateReply, serenity_prelude::CreateAttachment};
use std::fs;
use std::{fs::OpenOptions, io::Read};

use super::Context;
use crate::Error;

/// Get the latest crash log
#[poise::command(slash_command, guild_only, check = "super::is_operator")]
pub async fn crash(ctx: Context<'_>) -> Result<(), Error> {
    let path = ctx.data().server_directory.join("crash-reports");

    let (name, bytes) = tokio::task::spawn_blocking(move || {
        let dir = fs::read_dir(&path)?;
        let latest = dir
            .filter_map(|f| f.ok())
            .filter_map(|f| {
                f.metadata()
                    .ok()
                    .filter(|m| m.is_file())
                    .and_then(|m| m.created().ok().map(|c| (f.path(), c)))
            })
            .max_by_key(|(_, c)| *c)
            .ok_or(eyre!("No files in {path:?}"))?
            .0;

        let bytes = {
            let mut buf = Vec::with_capacity(4096);
            let mut file = OpenOptions::new().read(true).open(&latest)?;
            file.read_to_end(&mut buf)?;
            buf
        };

        let name = latest
            .file_name()
            .map(|s| String::from_utf8(s.as_encoded_bytes().to_vec()))
            .ok_or(eyre!("No file name for {latest:?}"))??;

        Ok::<_, crate::Error>((name, bytes))
    })
    .await??;

    ctx.send(CreateReply::default().attachment(CreateAttachment::bytes(bytes, name)))
        .await?;

    Ok(())
}
