use eyre::bail;

use crate::Result;
use std::path::{Path, PathBuf};

pub fn files(directory: &Path) -> Result<impl Iterator<Item = PathBuf>> {
    if !directory.is_dir() {
        bail!("`directory` must be a directory");
    }

    Ok(directory
        .read_dir()?
        .filter_map(Result::ok)
        .filter(|e| {
            e.file_type().is_ok_and(|t| t.is_file())
                && e.path().extension().is_some_and(|ext| ext == "jar")
        })
        .map(|e| e.path()))
}
