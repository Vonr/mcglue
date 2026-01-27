use std::sync::{Arc, OnceLock};

use crate::{Result, parsing::OwnedPlayerData};

pub static LIST_SENDER: OnceLock<tokio::sync::broadcast::Sender<Arc<[OwnedPlayerData]>>> =
    OnceLock::new();

pub async fn list() -> Result<Arc<[OwnedPlayerData]>> {
    let mut rx = LIST_SENDER.get().unwrap().subscribe();
    crate::command(*b"list").await?;
    Ok(rx.recv().await?)
}
