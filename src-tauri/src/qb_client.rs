use anyhow::{anyhow, Context, Result};
use reqwest::blocking::Client;
use reqwest_cookie_store::{CookieStore, CookieStoreMutex};
use serde::Deserialize;
use serde_bencode::de;
use serde_bytes::ByteBuf;
use std::sync::Arc;
use url::Url;
use uuid::Uuid;

use crate::config::Config;
use crate::models::{QbTorrent, SpaceSnapshot};

pub struct QbClient {
    client: Client,
    base: String,
}

#[derive(Debug, Deserialize)]
struct ServerState {
    free_space_on_disk: u64,
}

#[derive(Debug, Deserialize)]
struct MainData {
    server_state: ServerState,
}

#[derive(Debug, Deserialize)]
struct TorrentDto {
    name: String,
    hash: String,
    state: String,
    tags: Option<String>,
    total_size: Option<u64>,
    size: Option<u64>,
    completed: Option<u64>,
}

#[derive(Debug, Deserialize)]
struct TorrentMetaInfo {
    info: TorrentInfoDict,
}

#[derive(Debug, Deserialize)]
struct TorrentInfoDict {
    name: Option<String>,
    #[serde(rename = "name.utf-8")]
    name_utf8: Option<String>,
    pieces: Option<ByteBuf>,
}

impl QbClient {
    pub fn login(config: &Config) -> Result<Self> {
        let cookie_store = Arc::new(CookieStoreMutex::new(CookieStore::default()));
        let client = reqwest::blocking::ClientBuilder::new()
            .cookie_provider(cookie_store.clone())
            .build()
            .context("failed to build qB client")?;

        let base = config.qb_host.trim_end_matches('/').to_string();
        let login_resp = client
            .post(format!("{base}/api/v2/auth/login"))
            .form(&[
                ("username", config.qb_username.as_str()),
                ("password", config.qb_password.as_str()),
            ])
            .send()
            .context("failed to login qBittorrent")?;

        println!("qB login status: {}", login_resp.status());
        let login_body = login_resp
            .text()
            .context("failed to read qB login response body")?;
        println!("qB login body: {}", login_body.trim());

        let cookie_url = Url::parse(&base).context("failed to parse qB host as URL")?;
        let has_cookie = cookie_store
            .lock()
            .map_err(|_| anyhow!("failed to lock qB cookie store"))?
            .get_request_values(&cookie_url)
            .next()
            .is_some();
        println!("qB session cookie present: {has_cookie}");

        Ok(Self { client, base })
    }

    pub fn version(&self) -> Result<String> {
        self.client
            .get(format!("{}/api/v2/app/version", self.base))
            .send()
            .context("failed to get qBittorrent version")?
            .error_for_status()
            .context("qBittorrent version endpoint returned error status")?
            .text()
            .context("failed to read qBittorrent version")
    }

    pub fn space_snapshot(&self) -> Result<SpaceSnapshot> {
        let main_data: MainData = self
            .client
            .get(format!("{}/api/v2/sync/maindata", self.base))
            .send()
            .context("failed to fetch qB maindata")?
            .error_for_status()
            .context("qB maindata endpoint returned error status")?
            .json()
            .context("failed to parse qB maindata")?;

        let torrents = self.torrents_info()?;
        let active_states = [
            "downloading",
            "forcedDL",
            "metaDL",
            "forcedMetaDL",
            "stalledDL",
            "checkingDL",
            "queuedDL",
        ];

        let downloading_remaining_bytes = torrents
            .iter()
            .filter(|t| active_states.contains(&t.state.as_str()))
            .map(|t| t.total_size.saturating_sub(t.completed))
            .sum::<u64>();

        Ok(SpaceSnapshot {
            free_space_bytes: main_data.server_state.free_space_on_disk,
            downloading_remaining_bytes,
        })
    }

    pub fn torrents_info(&self) -> Result<Vec<QbTorrent>> {
        let items: Vec<TorrentDto> = self
            .client
            .get(format!("{}/api/v2/torrents/info", self.base))
            .send()
            .context("failed to fetch qB torrent list")?
            .error_for_status()
            .context("qB torrent list endpoint returned error status")?
            .json()
            .context("failed to parse qB torrent list")?;

        Ok(items
            .into_iter()
            .map(|t| QbTorrent {
                name: t.name,
                hash: t.hash,
                state: t.state,
                total_size: t.total_size.or(t.size).unwrap_or(0),
                completed: t.completed.unwrap_or(0),
            })
            .collect())
    }

    pub fn add_torrent_from_bytes(
        &self,
        torrent_id: &str,
        torrent_bytes: Vec<u8>,
        save_path: &str,
        paused: bool,
    ) -> Result<QbTorrent> {
        let torrent_name = extract_torrent_name(&torrent_bytes).ok();
        if let Some(name) = &torrent_name {
            let existing_names = self.torrent_names()?;
            if existing_names.iter().any(|existing| existing == name) {
                return Err(anyhow!(
                    "torrent appears to already exist in qB by name: {name}"
                ));
            }
        }

        let unique_tag = format!("temp_{}", Uuid::new_v4().simple());

        let part = reqwest::blocking::multipart::Part::bytes(torrent_bytes)
            .file_name(format!("{torrent_id}.torrent"))
            .mime_str("application/x-bittorrent")
            .context("failed to build multipart torrent part")?;
        let form = reqwest::blocking::multipart::Form::new()
            .part("torrents", part)
            .text("savepath", save_path.to_string())
            .text("paused", if paused { "true" } else { "false" }.to_string())
            .text("tags", unique_tag.clone());

        let response = self
            .client
            .post(format!("{}/api/v2/torrents/add", self.base))
            .multipart(form)
            .send()
            .context("failed to add torrent to qBittorrent")?;
        let status = response.status();
        let body = response
            .text()
            .context("failed to read qB add torrent response body")?;
        println!("qB add status: {}", status);
        println!("qB add body: {}", body.trim());
        if !status.is_success() {
            return Err(anyhow!(
                "qB add torrent endpoint returned error status {} with body {}",
                status,
                body.trim()
            ));
        }
        if body.trim() == "Fails." {
            return Err(anyhow!(
                "qB rejected torrent add request with body 'Fails.'{}",
                torrent_name
                    .as_ref()
                    .map(|name| format!(" (torrent name: {name})"))
                    .unwrap_or_default()
            ));
        }

        for _ in 0..10 {
            std::thread::sleep(std::time::Duration::from_millis(500));
            if let Some(found) = self.find_torrent_by_tag(&unique_tag)? {
                let _ = self.remove_tag(&found.hash, &unique_tag);
                let _ = self.delete_tags(&unique_tag);
                return Ok(found);
            }
        }

        Err(anyhow!(
            "torrent added request succeeded but no new qB torrent was detected"
        ))
    }

    pub fn torrent_names(&self) -> Result<Vec<String>> {
        Ok(self.torrents_info()?.into_iter().map(|t| t.name).collect())
    }

    pub fn resume_torrent(&self, hash: &str) -> Result<()> {
        self.client
            .post(format!("{}/api/v2/torrents/resume", self.base))
            .form(&[("hashes", hash)])
            .send()
            .context("failed to resume qB torrent")?
            .error_for_status()
            .context("qB resume endpoint returned error status")?;
        Ok(())
    }

    fn find_torrent_by_tag(&self, tag: &str) -> Result<Option<QbTorrent>> {
        let items: Vec<TorrentDto> = self
            .client
            .get(format!("{}/api/v2/torrents/info", self.base))
            .query(&[("tag", tag)])
            .send()
            .context("failed to query qB torrent by tag")?
            .error_for_status()
            .context("qB torrent-by-tag endpoint returned error status")?
            .json()
            .context("failed to parse qB torrent-by-tag response")?;

        Ok(items.into_iter().next().map(|t| QbTorrent {
            name: t.name,
            hash: t.hash,
            state: t.state,
            total_size: t.total_size.or(t.size).unwrap_or(0),
            completed: t.completed.unwrap_or(0),
        }))
    }

    fn remove_tag(&self, hash: &str, tag: &str) -> Result<()> {
        self.client
            .post(format!("{}/api/v2/torrents/removeTags", self.base))
            .form(&[("hashes", hash), ("tags", tag)])
            .send()
            .context("failed to remove tag from qB torrent")?
            .error_for_status()
            .context("qB removeTags endpoint returned error status")?;
        Ok(())
    }

    fn delete_tags(&self, tag: &str) -> Result<()> {
        self.client
            .post(format!("{}/api/v2/torrents/deleteTags", self.base))
            .form(&[("tags", tag)])
            .send()
            .context("failed to delete qB tag")?
            .error_for_status()
            .context("qB deleteTags endpoint returned error status")?;
        Ok(())
    }
}

pub fn extract_torrent_name(bytes: &[u8]) -> Result<String> {
    let meta: TorrentMetaInfo =
        de::from_bytes(bytes).context("failed to parse torrent metadata with serde_bencode")?;
    meta.info
        .name_utf8
        .or(meta.info.name)
        .ok_or_else(|| anyhow!("torrent info dictionary did not contain a name field"))
}
