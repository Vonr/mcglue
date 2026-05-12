use std::sync::OnceLock;

use crate::{Result, parsing::ListData};

pub static LIST_SENDER: OnceLock<tokio::sync::broadcast::Sender<ListData>> = OnceLock::new();

pub async fn list() -> Result<ListData> {
    let mut rx = LIST_SENDER.get().unwrap().subscribe();
    crate::command(*b"list").await?;
    Ok(rx.recv().await?)
}
