use crate::config::Config;
use crate::models::QbTorrent;
use std::time::{SystemTime, UNIX_EPOCH};

pub const MANAGED_TAG: &str = "byrradar";
pub const KEEP_TAG: &str = "keep";

#[derive(Debug, Clone, Default)]
pub struct CleanupPlan {
    pub selected: Vec<QbTorrent>,
    pub reclaimed_bytes: u64,
}

pub fn current_unix_ts() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs() as i64)
        .unwrap_or_default()
}

pub fn select_cleanup_pool(qb_torrents: &[QbTorrent], config: &Config) -> Vec<QbTorrent> {
    let now = current_unix_ts();
    let mut candidates = qb_torrents
        .iter()
        .filter(|torrent| is_cleanup_eligible(torrent, config, now))
        .cloned()
        .collect::<Vec<_>>();

    candidates.sort_by_key(|torrent| {
        (
            torrent.num_leechs.max(0),
            torrent.num_seeds.max(0),
            torrent.upspeed,
            torrent.added_on,
            -(torrent.total_size as i64),
        )
    });
    candidates
}

pub fn select_adoption_pool(qb_torrents: &[QbTorrent]) -> Vec<QbTorrent> {
    let now = current_unix_ts();
    let mut candidates = qb_torrents
        .iter()
        .filter(|torrent| !is_managed_torrent(torrent))
        .filter(|torrent| !has_keep_tag(torrent))
        .filter(|torrent| is_completed_torrent(torrent))
        .cloned()
        .collect::<Vec<_>>();

    candidates.sort_by_key(|torrent| {
        (
            -(age_hours(torrent, now) as i64),
            torrent.num_leechs.max(0),
            torrent.upspeed,
            -(torrent.total_size as i64),
        )
    });
    candidates
}

pub fn count_managed_torrents(qb_torrents: &[QbTorrent]) -> usize {
    qb_torrents
        .iter()
        .filter(|torrent| is_managed_torrent(torrent))
        .count()
}

pub fn count_keep_torrents(qb_torrents: &[QbTorrent]) -> usize {
    qb_torrents
        .iter()
        .filter(|torrent| has_keep_tag(torrent))
        .count()
}

pub fn select_keep_targets(
    qb_torrents: &[QbTorrent],
    query: &str,
    keep_state: Option<bool>,
) -> Vec<QbTorrent> {
    let tokens = parse_keep_query(query);
    if tokens.is_empty() {
        return Vec::new();
    }

    qb_torrents
        .iter()
        .filter(|torrent| match keep_state {
            Some(true) => has_keep_tag(torrent),
            Some(false) => !has_keep_tag(torrent),
            None => true,
        })
        .filter(|torrent| matches_keep_query(torrent, &tokens))
        .cloned()
        .collect()
}

pub fn build_cleanup_plan(
    cleanup_pool: &[QbTorrent],
    required_bytes: u64,
    max_remove_per_cycle: usize,
) -> CleanupPlan {
    let mut plan = CleanupPlan {
        selected: Vec::new(),
        reclaimed_bytes: 0,
    };

    if required_bytes == 0 || cleanup_pool.is_empty() {
        return plan;
    }

    for torrent in cleanup_pool.iter().take(max_remove_per_cycle) {
        if plan.reclaimed_bytes >= required_bytes {
            break;
        }
        plan.reclaimed_bytes = plan.reclaimed_bytes.saturating_add(torrent.total_size);
        plan.selected.push(torrent.clone());
    }

    plan
}

pub fn cleanup_reason_summary(torrent: &QbTorrent) -> String {
    format!(
        "age={}h leechers={} upspeed={}KiB/s size={:.2}GiB",
        age_hours(torrent, current_unix_ts()),
        torrent.num_leechs,
        torrent.upspeed / 1024,
        torrent.total_size as f64 / 1024.0 / 1024.0 / 1024.0
    )
}

fn is_cleanup_eligible(torrent: &QbTorrent, config: &Config, now: i64) -> bool {
    if !is_managed_torrent(torrent) || has_keep_tag(torrent) {
        return false;
    }
    if !is_completed_torrent(torrent) {
        return false;
    }
    if age_hours(torrent, now) < config.min_seeding_hours_before_remove {
        return false;
    }
    if torrent.num_leechs > config.max_leechers_for_stale {
        return false;
    }
    if torrent.upspeed > config.max_recent_upspeed_kib.saturating_mul(1024) {
        return false;
    }
    true
}

fn is_managed_torrent(torrent: &QbTorrent) -> bool {
    torrent
        .tags
        .split(',')
        .map(str::trim)
        .any(|tag| tag.eq_ignore_ascii_case(MANAGED_TAG))
}

fn has_keep_tag(torrent: &QbTorrent) -> bool {
    torrent
        .tags
        .split(',')
        .map(str::trim)
        .any(|tag| tag.eq_ignore_ascii_case(KEEP_TAG))
}

fn parse_keep_query(query: &str) -> Vec<String> {
    query
        .split(|ch: char| ch == ',' || ch == '\n' || ch == '\r')
        .map(|item| item.trim().to_ascii_lowercase())
        .filter(|item| !item.is_empty())
        .collect()
}

fn matches_keep_query(torrent: &QbTorrent, tokens: &[String]) -> bool {
    let name = torrent.name.to_ascii_lowercase();
    let hash = torrent.hash.to_ascii_lowercase();
    tokens
        .iter()
        .any(|token| name.contains(token) || hash.contains(token))
}

fn is_completed_torrent(torrent: &QbTorrent) -> bool {
    torrent.total_size > 0
        && (torrent.completed >= torrent.total_size || torrent.progress >= 0.999)
        && !matches!(
            torrent.state.as_str(),
            "downloading"
                | "forcedDL"
                | "metaDL"
                | "forcedMetaDL"
                | "checkingDL"
                | "allocating"
        )
}

fn age_hours(torrent: &QbTorrent, now: i64) -> u64 {
    if torrent.added_on <= 0 || now <= torrent.added_on {
        return 0;
    }
    ((now - torrent.added_on) as u64) / 3600
}
