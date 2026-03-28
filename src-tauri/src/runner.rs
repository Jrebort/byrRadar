use anyhow::Result;

use crate::byr_client::ByrClient;
use crate::config::Config;
use crate::models::TorrentInfo;
use crate::planner::{default_planner_config, find_appropriate_torrents, plan_downloads};
use crate::qb_client::{extract_torrent_name, QbClient};

#[derive(Debug, Clone, Default)]
pub struct RunReport {
    pub free_count: usize,
    pub filtered_count: usize,
    pub planned_count: usize,
    pub rough_planned_count: usize,
    pub added_count: usize,
    pub skipped_count: usize,
    pub duplicate_skip_count: usize,
    pub budget_bytes: u64,
    pub free_space_bytes: u64,
    pub downloading_remaining_bytes: u64,
    pub seeding_count: usize,
    pub total_torrents: usize,
    pub queue_saturated: bool,
}

pub fn execute<F>(config: &Config, mut log: F) -> Result<RunReport>
where
    F: FnMut(String),
{
    log("Starting byrRadar core".to_string());
    if config.dry_run {
        log("Mode: dry-run".to_string());
    }
    if let Some(categories) = &config.include_categories {
        log(format!("Category filter: {}", categories.join(", ")));
    }

    let byr = ByrClient::login(config)?;
    log("BYR login: ok".to_string());

    let qb = QbClient::login(config)?;
    let version = qb.version()?;
    log(format!("qBittorrent: ok ({version})"));

    let torrents = byr.fetch_free_torrents()?;
    let torrents = apply_category_filter(torrents, config.include_categories.as_ref());
    let planner = default_planner_config();
    let space = qb.space_snapshot()?;
    let qb_torrents = qb.torrents_info()?;
    let existing_names = qb_torrents
        .iter()
        .map(|t| t.name.clone())
        .collect::<Vec<_>>();
    let seeding_count = qb_torrents
        .iter()
        .filter(|t| {
            matches!(
                t.state.as_str(),
                "uploading" | "stalledUP" | "queuedUP" | "forcedUP"
            )
        })
        .count();
    let filtered = find_appropriate_torrents(&torrents);
    let (rough_planned, budget) = plan_downloads(&filtered, &planner, &space, &existing_names);
    let rough_planned_count = rough_planned.len();

    let mut planned = Vec::new();
    let mut predownloaded_bytes = Vec::new();
    let mut duplicate_skip_count = 0usize;

    if config.dry_run {
        planned = rough_planned;
    } else {
        for torrent in rough_planned {
            let bytes = match byr.download_torrent_file(&torrent.seed_id) {
                Ok(bytes) => bytes,
                Err(err) => {
                    log(format!(
                        "Preflight skip: [{}] failed to download torrent file: {err}",
                        torrent.seed_id
                    ));
                    continue;
                }
            };

            match extract_torrent_name(&bytes) {
                Ok(torrent_name) => {
                    if existing_names.iter().any(|existing| existing == &torrent_name) {
                        duplicate_skip_count += 1;
                        log(format!(
                            "Preflight duplicate: [{}] {}",
                            torrent.seed_id, torrent_name
                        ));
                        continue;
                    }
                }
                Err(err) => {
                    log(format!(
                        "Preflight warning: [{}] failed to parse torrent name: {err}",
                        torrent.seed_id
                    ));
                }
            }

            predownloaded_bytes.push(bytes);
            planned.push(torrent);
        }
    }

    let queue_saturated = budget == 0 || (!filtered.is_empty() && planned.is_empty());

    log(format!(
        "Scan summary: free={}, filtered={}, rough_planned={}, actionable={}, duplicate_skipped={}, budget={} bytes, free_space={} bytes, downloading_remaining={} bytes",
        torrents.len(),
        filtered.len(),
        rough_planned_count,
        planned.len(),
        duplicate_skip_count,
        budget,
        space.free_space_bytes,
        space.downloading_remaining_bytes
    ));

    let mut added_count = 0usize;
    let mut skipped_count = 0usize;

    for (index, torrent) in planned.iter().enumerate() {
        log(format!(
            "Candidate: [{}][{}][{}] {}",
            torrent.seed_id, torrent.tag, torrent.size_text, torrent.title
        ));
        if config.dry_run {
            skipped_count += 1;
            log(format!("Skip: [{}] dry-run mode", torrent.seed_id));
            continue;
        }

        let bytes = predownloaded_bytes
            .get(index)
            .cloned()
            .expect("predownloaded bytes should align with actionable plan");

        match qb.add_torrent_from_bytes(&torrent.seed_id, bytes, &config.qb_download_path, true) {
            Ok(added) => {
                qb.resume_torrent(&added.hash)?;
                added_count += 1;
                log(format!(
                    "Added: [{}][{}][{} bytes]",
                    added.hash, added.name, added.total_size
                ));
            }
            Err(err) => {
                skipped_count += 1;
                log(format!("Skip: [{}] {}", torrent.seed_id, err));
            }
        }
    }

    log(format!(
        "Execution summary: added={}, skipped={}, duplicate_skipped={}, actionable={}, rough_planned={}",
        added_count,
        skipped_count,
        duplicate_skip_count,
        planned.len(),
        rough_planned_count
    ));

    Ok(RunReport {
        free_count: torrents.len(),
        filtered_count: filtered.len(),
        planned_count: planned.len(),
        rough_planned_count,
        added_count,
        skipped_count,
        duplicate_skip_count,
        budget_bytes: budget,
        free_space_bytes: space.free_space_bytes,
        downloading_remaining_bytes: space.downloading_remaining_bytes,
        seeding_count,
        total_torrents: qb_torrents.len(),
        queue_saturated,
    })
}

fn apply_category_filter(
    torrents: Vec<TorrentInfo>,
    include_categories: Option<&Vec<String>>,
) -> Vec<TorrentInfo> {
    let Some(categories) = include_categories else {
        return torrents;
    };
    torrents
        .into_iter()
        .filter(|torrent| categories.iter().any(|cat| torrent.category == *cat))
        .collect()
}
