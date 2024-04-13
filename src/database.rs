use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use tbot::types::parameters::ChatId;
use tokio::fs;

#[derive(Deserialize, Serialize)]
pub struct Database {
    #[serde(skip)]
    file: PathBuf,
    pub token: String,
    pub channel: String,
    pub assets_channel: String,
    imgs: HashMap<String, u64>,
    comments: HashMap<u64, u64>,
}

impl Database {
    pub async fn open<P: AsRef<Path>>(file: P) -> Result<Self, anyhow::Error> {
        let s = fs::read_to_string(&file).await?;
        let mut r: Self = serde_json::from_str(&s)?;
        r.file = file.as_ref().into();
        Ok(r)
    }
    pub async fn save(&self) -> Result<(), anyhow::Error> {
        let s = serde_json::to_string_pretty(self)?;
        fs::write(&self.file, s).await?;
        Ok(())
    }
    pub fn channel(&self) -> ChatId<'_> {
        self.channel.as_str().into()
    }
    pub fn assets_channel(&self) -> ChatId<'_> {
        self.assets_channel.as_str().into()
    }
    pub fn get_img(&self, url: &str) -> Option<String> {
        self.imgs.get(url).map(|id| {
            format!(
                "https://t.me/{}/{}",
                self.assets_channel.trim_start_matches('@'),
                id
            )
        })
    }
    pub fn get_comment(&self, comment_id: u64) -> Option<String> {
        self.comments.get(&comment_id).map(|msg_id| {
            format!(
                "https://t.me/{}/{}",
                self.assets_channel.trim_start_matches('@'),
                msg_id
            )
        })
    }
    pub async fn put_img(&mut self, url: String, msg_id: u64) {
        self.imgs.insert(url, msg_id);
        let _ = self.save().await;
    }
    pub async fn put_comment(&mut self, comment_id: u64, msg_id: u64) {
        self.comments.insert(comment_id, msg_id);
        let _ = self.save().await;
    }
}
