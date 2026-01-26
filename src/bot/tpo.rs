use eyre::bail;
use flate2::{Compression, read::GzDecoder, write::GzEncoder};
use poise::{CreateReply, serenity_prelude::CreateAttachment};
use serde::{Deserialize, Serialize};
use std::io::Write;
use std::{collections::HashMap, fs::OpenOptions, io::Read};
use uuid::Uuid;

use fastnbt::Value;

use super::Context;
use crate::Error;

async fn autocomplete_dimension<'a>(
    _ctx: Context<'_>,
    partial: &'a str,
) -> impl Iterator<Item = &'static str> + 'a {
    const KNOWN_DIMENSIONS: [&str; 3] = [
        "minecraft:overworld",
        "minecraft:the_nether",
        "minecraft:the_end",
    ];

    KNOWN_DIMENSIONS
        .into_iter()
        .filter(move |s| s.contains(partial))
}

/// Teleport an offline player.
#[poise::command(slash_command, guild_only)]
pub async fn tpo(
    ctx: Context<'_>,
    #[description = "Name or UUID of the player"] player: String,
    #[description = "X coordinate"] x: f64,
    #[description = "Y coordinate"] y: f64,
    #[description = "Z coordinate"] z: f64,
    #[description = "Dimension ID"]
    #[autocomplete = "autocomplete_dimension"]
    dimension: Option<String>,
) -> Result<(), Error> {
    let uuid = super::maybe_username_to_uuid(&player).await?;

    let players = crate::interface::list().await?;

    eprintln!("players: {players:?}");

    if let Some(p) = players
        .iter()
        .map(|p| (&p.name, p.uuid))
        .find(|p| p.1 == uuid)
    {
        bail!(
            "Player {} ({}) is currently online.",
            p.0,
            p.1.as_hyphenated()
        );
    }

    let path = {
        let mut filename = uuid.as_hyphenated().to_string();
        filename.push_str(".dat");
        ctx.data()
            .server_directory
            .join("world")
            .join("playerdata")
            .join(filename)
    };

    if !path.try_exists().unwrap_or(false) {
        bail!("{path:?} does not exist. Has this player joined the game before?");
    }

    let (original_bytes, data) = tokio::task::spawn_blocking(move || {
        let mut file = OpenOptions::new()
            .read(true)
            .write(true)
            .append(false)
            .create(false)
            .open(&path)
            .map_err(Error::from)?;

        if file.try_lock().is_err() {
            bail!("{path:?} is already open. Is the player currently online?");
        }

        let mut original_bytes = Vec::with_capacity(2048);
        file.read_to_end(&mut original_bytes)?;

        let data = {
            let mut uncompressed = Vec::with_capacity(2048);
            let mut reader = GzDecoder::new(&*original_bytes);
            reader.read_to_end(&mut uncompressed)?;
            uncompressed
        };

        let mut data = fastnbt::from_bytes::<PlayerData>(&data)?;

        if data.pos.len() != 3 {
            bail!("Expected Pos to be a list of 3 64-bit floating point numbers but found {:?} instead.", data.pos);
        }

        data.pos = vec![x, y, z];
        if let Some(ref dimension) = dimension {
            data.dimension = dimension.clone();
        }

        if let Some(vehicle) = &mut data.root_vehicle {
            if vehicle.entity.pos.len() != 3 {
                bail!("Expected RootVehicle.Entity.Pos to be a list of 3 64-bit floating point numbers but found {:?} instead.", vehicle.entity.pos);
            }

            vehicle.entity.pos = vec![x, y, z];
            if let Some(dimension) = dimension {
                vehicle.entity.dimension = dimension;
            }
        }

        let bytes = fastnbt::to_bytes(&data)?;

        let temp_path = path.with_added_extension(".tmp");
        let temp_file = OpenOptions::new()
            .read(true)
            .write(true)
            .append(false)
            .create(true)
            .truncate(true)
            .open(&temp_path)
            .map_err(Error::from)?;

        let mut encoder = GzEncoder::new(&temp_file, Compression::fast());

        encoder.write_all(&bytes)?;
        encoder.finish()?;

        // Drop and flush temporary file
        drop(temp_file);

        // Drop and unlock the original .dat file
        drop(file);

        std::fs::rename(temp_path, &path)?;

        Ok((original_bytes, data))
    })
    .await??;

    ctx.send(
        CreateReply::default()
            .content(format!(
                "Teleported {} to {} {} {} in {}",
                uuid.as_hyphenated(),
                x,
                y,
                z,
                data.dimension
            ))
            .attachment(CreateAttachment::bytes(original_bytes, "backup.dat")),
    )
    .await?;

    Ok(())
}

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "PascalCase")]
pub struct PlayerData {
    pub pos: Vec<f64>,
    pub dimension: String,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub root_vehicle: Option<RootVehicle>,

    #[serde(flatten)]
    pub other: HashMap<String, Value>,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "PascalCase")]
pub struct RootVehicle {
    pub entity: Entity,

    #[serde(flatten)]
    pub other: HashMap<String, Value>,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "PascalCase")]
pub struct Entity {
    pub pos: Vec<f64>,
    pub dimension: String,

    #[serde(flatten)]
    pub other: HashMap<String, Value>,
}
