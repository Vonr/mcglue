use eyre::{Context as _, bail};
use flate2::{
    Compression,
    write::{GzDecoder, GzEncoder},
};
use jaq_core::{
    Ctx, Vars, data,
    load::{Arena, Loader},
    unwrap_valr,
};
use nbtq_core::{
    Val,
    nbt::{Nbt, NbtTag},
    print::WriterOptions,
};
use poise::{CreateReply, serenity_prelude::CreateAttachment};
use std::io::Write;
use std::{fs::OpenOptions, io::Read};

use super::Context;
use crate::{Error, SafeJoin};

/// Teleport an offline player.
#[poise::command(slash_command, guild_only, check = "super::is_operator")]
pub async fn nbtq(
    ctx: Context<'_>,
    #[description = "Filter"] filter: Option<String>,
    #[description = "Path"]
    #[autocomplete = "super::autocomplete_path_nbt"]
    path: String,
    #[description = "Save output of filter to the input file"] save: Option<bool>,
) -> Result<(), Error> {
    let save = save.unwrap_or(false);
    let path = ctx.data().server_directory.safe_join(path)?;
    let file_name = path
        .file_name()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| "original".into());

    if !path.try_exists().unwrap_or(false) {
        bail!("{path:?} does not exist.");
    }

    let filter = filter.unwrap_or_else(|| String::from("."));

    let (original_bytes, output) = tokio::task::spawn_blocking(move || {
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

        let mut raw_input = Vec::new();
        file.read_to_end(&mut raw_input)?;

        let mut input_format = OutputFormat::Nbt;
        let input = Nbt::read(&mut raw_input.as_slice())
            .or_else(|_| {
                let decoded = Vec::new();
                let mut decoder = GzDecoder::new(decoded);
                decoder.write_all(&raw_input)?;
                raw_input = decoder.finish().context("gzip decode failure")?;
                input_format = OutputFormat::Gzip;
                Nbt::read(&mut raw_input.as_slice()).context("failed post-ungzip parse")
            })
            .or_else(|_| {
                input_format = OutputFormat::Snbt;
                std::str::from_utf8(&raw_input)
                    .context("failed utf8 check after non-stringified failures")?
                    .trim_ascii_end()
                    .parse()
                    .context("failed snbt parse")
            })
            .context("input should be NBT or SNBT")?;
        let (name, input) = (input.name, input.root_tag);

        let program = jaq_core::load::File {
            code: filter.as_str(),
            path: (),
        };

        let defs = jaq_core::defs()
            .chain(jaq_std::defs())
            .chain(nbtq_core::defs());
        let funs = jaq_core::funs()
            .chain(jaq_std::funs())
            .chain(nbtq_core::funs());

        let loader = Loader::new(defs);
        let arena = Arena::default();

        let modules = match loader.load(&arena, program) {
            Ok(modules) => modules,
            Err(errors) => {
                let mut err = String::from("Error loading program:");
                for e in errors {
                    err.push_str(&format!("\n- {:?}", e.1));
                }
                bail!(err);
            }
        };

        let filter = match jaq_core::Compiler::default()
            .with_funs(funs)
            .compile(modules)
        {
            Ok(filter) => filter,
            Err(errors) => {
                let mut err = String::from("Error compiling filter:");
                for e in errors {
                    err.push_str(&format!("\n- {:?}", e.1));
                }
                bail!(err);
            }
        };

        let ctx = Ctx::<data::JustLut<Val>>::new(&filter.lut, Vars::new([]));

        let input = Val(input.into());
        let out = match filter
            .id
            .run((ctx, input))
            .map(unwrap_valr)
            .map(|v| v.map(|v| v.0))
            .collect::<Result<Vec<_>, _>>()
        {
            Ok(out) => out,
            Err(e) => {
                bail!("Error running filter: {e}");
            }
        };

        if save {
            let NbtTag::Compound(tag) = (match out.len() {
                0 => bail!("No values to write"),
                1 => out[0].clone(),
                _ => bail!("Too many values to write"),
            }) else {
                bail!("Cannot write a non-Compound value");
            };

            let temp_path = path.with_added_extension(".tmp");
            let mut temp_file = OpenOptions::new()
                .read(true)
                .write(true)
                .append(false)
                .create(true)
                .truncate(true)
                .open(&temp_path)
                .map_err(Error::from)?;

            input_format.write(&mut temp_file, Nbt::new(name, tag))?;
            temp_file.flush()?;

            // Drop temporary file
            drop(temp_file);

            // Drop and unlock the original file
            drop(file);

            std::fs::rename(temp_path, &path)?;
        }

        Ok((raw_input, out))
    })
    .await??;

    let mut output_string = String::new();
    for tag in output {
        output_string.push_str(&nbtq_core::print::to_snbt_string(
            &tag,
            WriterOptions {
                pretty: true,
                ..Default::default()
            },
        )?);
        output_string.push('\n');
    }

    if output_string.len() > 1900 {
        ctx.send(
            CreateReply::default()
                .ephemeral(true)
                .content("Executed filter.")
                .attachment(CreateAttachment::bytes(
                    output_string.as_bytes(),
                    "result.txt",
                ))
                .attachment(CreateAttachment::bytes(original_bytes, file_name)),
        )
        .await?;
    } else {
        ctx.send(
            CreateReply::default()
                .ephemeral(true)
                .content(format!("Executed filter.\n```\n{output_string}```",))
                .attachment(CreateAttachment::bytes(original_bytes, file_name)),
        )
        .await?;
    }

    Ok(())
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum OutputFormat {
    Snbt,
    Nbt,
    Gzip,
}

impl OutputFormat {
    fn write(self, writer: &mut impl std::io::Write, value: Nbt) -> Result<(), Error> {
        match self {
            OutputFormat::Nbt => Self::write_nbt(writer, value),
            OutputFormat::Snbt => Self::write_snbt(writer, value),
            OutputFormat::Gzip => Self::write_gzip(6, writer, value),
        }
    }

    fn write_nbt(writer: &mut impl std::io::Write, value: Nbt) -> Result<(), Error> {
        value.write_to_writer(writer)?;
        Ok(())
    }

    fn write_gzip(level: u8, writer: &mut impl std::io::Write, value: Nbt) -> Result<(), Error> {
        let mut encoder = GzEncoder::new(writer, Compression::new(level as u32));
        Self::write_nbt(&mut encoder, value)
    }

    fn write_snbt(writer: &mut impl std::io::Write, value: Nbt) -> Result<(), Error> {
        writer.write_all(value.to_string().as_bytes())?;
        Ok(())
    }
}
