use anyhow::Result;
use std::collections::HashSet;
use std::time::Duration;

use crate::byr_client::ByrClient;
use crate::config::Config;
use crate::models::TorrentInfo;
use crate::planner::{default_planner_config, find_appropriate_torrents, sort_torrents_by_priority};
use crate::qb_client::{extract_torrent_name, QbClient};
use crate::rotation::{build_cleanup_plan, cleanup_reason_summary, count_keep_torrents, count_managed_torrents, select_cleanup_pool};

#[derive(Debug, Clone, Default)]
pub struct RunReport {
    pub free_count: usize,
    pub filtered_count: usize,
    pub planned_count: usize,
    pub rough_planned_count: usize,
    pub added_count: usize,
    pub skipped_count: usize,
    pub duplicate_skip_count: usize,
    pub cleanup_eligible_count: usize,
    pub cleanup_selected_count: usize,
    pub cleanup_reclaimed_bytes: u64,
    pub managed_count: usize,
    pub keep_count: usize,
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
    let filtered = find_appropriate_torrents(&torrents);
    let sorted_candidates = sort_torrents_by_priority(&filtered);
    let planner = default_planner_config();
    let space = qb.space_snapshot()?;
    let qb_torrents = qb.torrents_info()?;
    let existing_names = qb_torrents
        .iter()
        .map(|torrent| torrent.name.clone())
        .collect::<Vec<_>>();
    let managed_count = count_managed_torrents(&qb_torrents);
    let keep_count = count_keep_torrents(&qb_torrents);
    let seeding_count = qb_torrents
        .iter()
        .filter(|torrent| {
            matches!(
                torrent.state.as_str(),
                "uploading" | "stalledUP" | "queuedUP" | "forcedUP"
            )
        })
        .count();

    let cleanup_pool = select_cleanup_pool(&qb_torrents, config);
    let cleanup_eligible_count = cleanup_pool.len();
    if cleanup_eligible_count > 0 {
        log(format!(
            "Cleanup pool: eligible={} auto_rotate={} max_remove_per_cycle={}",
            cleanup_eligible_count, config.auto_rotate_enabled, config.max_remove_per_cycle
        ));
    }

    let current_capacity = space
        .free_space_bytes
        .saturating_sub(space.downloading_remaining_bytes)
        .saturating_sub(planner.min_free_space_bytes);
    let budget = planner
        .download_budget_bytes
        .map(|limit| limit.min(current_capacity))
        .unwrap_or(current_capacity);

    let mut planned = Vec::new();
    let mut predownloaded_bytes = Vec::new();
    let mut rough_planned_count = 0usize;
    let mut duplicate_skip_count = 0usize;
    let mut skipped_count = 0usize;
    let mut remaining_capacity = current_capacity;
    let mut cleanup_selected = Vec::new();
    let mut cleanup_selected_hashes = HashSet::new();
    let mut cleanup_reclaimed_bytes = 0u64;
    let mut remaining_download_budget = planner.download_budget_bytes.unwrap_or(u64::MAX);

    for torrent in sorted_candidates {
        let Some(size_bytes) = torrent.size_bytes else {
            log(format!(
                "Planner skip: [{}] missing size_bytes from '{}'",
                torrent.seed_id, torrent.size_text
            ));
            skipped_count += 1;
            continue;
        };

        if size_bytes > remaining_download_budget {
            log(format!(
                "Planner skip: [{}] exceeds remaining download budget {} bytes",
                torrent.seed_id, remaining_download_budget
            ));
            skipped_count += 1;
            continue;
        }

        let bytes = match byr.download_torrent_file(&torrent.seed_id) {
            Ok(bytes) => bytes,
            Err(err) => {
                log(format!(
                    "Preflight skip: [{}] failed to download torrent file: {err}",
                    torrent.seed_id
                ));
                skipped_count += 1;
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

        rough_planned_count += 1;

        if size_bytes > remaining_capacity {
            let required_bytes = size_bytes.saturating_sub(remaining_capacity);
            if !config.auto_rotate_enabled {
                log(format!(
                    "Rotation skip: [{}] needs {} bytes but auto rotation is disabled",
                    torrent.seed_id, required_bytes
                ));
                skipped_count += 1;
                continue;
            }

            let available_cleanup = cleanup_pool
                .iter()
                .filter(|candidate| !cleanup_selected_hashes.contains(&candidate.hash))
                .cloned()
                .collect::<Vec<_>>();
            let max_remove_left = config
                .max_remove_per_cycle
                .saturating_sub(cleanup_selected.len());
            let cleanup_plan =
                build_cleanup_plan(&available_cleanup, required_bytes, max_remove_left);

            if cleanup_plan.reclaimed_bytes < required_bytes {
                log(format!(
                    "Rotation skip: [{}] still short {} bytes after cleanup planning",
                    torrent.seed_id,
                    required_bytes.saturating_sub(cleanup_plan.reclaimed_bytes)
                ));
                skipped_count += 1;
                continue;
            }

            for candidate in cleanup_plan.selected {
                if cleanup_selected_hashes.insert(candidate.hash.clone()) {
                    log(format!(
                        "Cleanup planned: [{}] {} | {}",
                        candidate.hash,
                        candidate.name,
                        cleanup_reason_summary(&candidate)
                    ));
                    cleanup_reclaimed_bytes =
                        cleanup_reclaimed_bytes.saturating_add(candidate.total_size);
                    remaining_capacity = remaining_capacity.saturating_add(candidate.total_size);
                    cleanup_selected.push(candidate);
                }
            }
        }

        if size_bytes > remaining_capacity {
            log(format!(
                "Planner skip: [{}] need={} remaining={}",
                torrent.seed_id, size_bytes, remaining_capacity
            ));
            skipped_count += 1;
            continue;
        }

        planned.push(torrent);
        predownloaded_bytes.push(bytes);
        remaining_capacity = remaining_capacity.saturating_sub(size_bytes);
        remaining_download_budget = remaining_download_budget.saturating_sub(size_bytes);
    }

    let queue_saturated = budget == 0 || (!filtered.is_empty() && planned.is_empty());

    log(format!(
        "Scan summary: free={}, filtered={}, rough_planned={}, actionable={}, duplicate_skipped={}, cleanup_eligible={}, cleanup_selected={}, reclaimed={} bytes, budget={} bytes, free_space={} bytes, downloading_remaining={} bytes",
        torrents.len(),
        filtered.len(),
        rough_planned_count,
        planned.len(),
        duplicate_skip_count,
        cleanup_eligible_count,
        cleanup_selected.len(),
        cleanup_reclaimed_bytes,
        budget,
        space.free_space_bytes,
        space.downloading_remaining_bytes
    ));

    if !cleanup_selected.is_empty() {
        if config.dry_run {
            log(format!(
                "Dry-run cleanup summary: would remove {} torrents and reclaim {} bytes",
                cleanup_selected.len(),
                cleanup_reclaimed_bytes
            ));
        } else {
            let hashes = cleanup_selected
                .iter()
                .map(|torrent| torrent.hash.clone())
                .collect::<Vec<_>>();
            qb.delete_torrents(&hashes, true)?;
            for candidate in &cleanup_selected {
                log(format!(
                    "Cleanup removed: [{}][{} bytes] {}",
                    candidate.hash, candidate.total_size, candidate.name
                ));
            }
            std::thread::sleep(Duration::from_secs(2));
        }
    }

    let mut added_count = 0usize;
    for (index, torrent) in planned.iter().enumerate() {
        log(format!(
            "Candidate: [{}][{}][{}] {}",
            torrent.seed_id, torrent.tag, torrent.size_text, torrent.title
        ));
        if config.dry_run {
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
        "Execution summary: added={}, skipped={}, duplicate_skipped={}, actionable={}, rough_planned={}, cleanup_selected={}, reclaimed={} bytes",
        added_count,
        skipped_count,
        duplicate_skip_count,
        planned.len(),
        rough_planned_count,
        cleanup_selected.len(),
        cleanup_reclaimed_bytes
    ));

    Ok(RunReport {
        free_count: torrents.len(),
        filtered_count: filtered.len(),
        planned_count: planned.len(),
        rough_planned_count,
        added_count,
        skipped_count,
        duplicate_skip_count,
        cleanup_eligible_count,
        cleanup_selected_count: cleanup_selected.len(),
        cleanup_reclaimed_bytes,
        managed_count,
        keep_count,
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
