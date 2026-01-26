use std::{collections::VecDeque, sync::LazyLock};

use parking_lot::Mutex;
use tokio::sync::oneshot;

use crate::{Result, parsing::OwnedPlayerData};

pub static LIST_LISTENERS: LazyLock<Mutex<VecDeque<oneshot::Sender<Vec<OwnedPlayerData>>>>> =
    LazyLock::new(|| Mutex::new(VecDeque::new()));

pub async fn list() -> Result<Vec<OwnedPlayerData>> {
    let (tx, rx) = oneshot::channel::<Vec<OwnedPlayerData>>();
    LIST_LISTENERS.lock().push_back(tx);
    println!("list");
    Ok(rx.await?)
}
