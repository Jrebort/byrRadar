use crate::models::{PlannerConfig, TorrentInfo};

const KIB: u64 = 1024;
const MIB: u64 = KIB * 1024;
const GIB: u64 = MIB * 1024;
const TIB: u64 = GIB * 1024;

pub fn default_planner_config() -> PlannerConfig {
    PlannerConfig {
        min_free_space_bytes: 5 * GIB,
        download_budget_bytes: std::env::var("DOWNLOAD_BUDGET_GB")
            .ok()
            .and_then(|v| v.parse::<f64>().ok())
            .filter(|v| *v > 0.0)
            .map(|v| (v * GIB as f64) as u64),
    }
}

pub fn parse_size_to_bytes(size_text: &str) -> Option<u64> {
    let normalized = size_text.replace(' ', "").to_uppercase();
    let units = [("TIB", TIB), ("GIB", GIB), ("MIB", MIB), ("KIB", KIB)];
    for (unit, mul) in units {
        if let Some(num) = normalized.strip_suffix(unit) {
            let value = num.parse::<f64>().ok()?;
            return Some((value * mul as f64) as u64);
        }
    }
    None
}

pub fn find_appropriate_torrents(torrents: &[TorrentInfo]) -> Vec<TorrentInfo> {
    let mut out = Vec::new();
    if torrents.len() >= 20 {
        for torrent in torrents {
            if !torrent.size_text.contains("GiB") {
                continue;
            }
            if torrent.seeders <= 0 || torrent.leechers < 0 {
                continue;
            }
            if torrent.seeders != 0 && (torrent.leechers as f64 / torrent.seeders as f64) < 20.0 {
                continue;
            }
            let size_gib = torrent
                .size_text
                .replace("GiB", "")
                .trim()
                .parse::<f64>()
                .unwrap_or(0.0);
            if size_gib < 20.0 {
                continue;
            }
            out.push(torrent.clone());
        }
    } else {
        for torrent in torrents {
            if torrent.seeders <= 0 || torrent.leechers < 0 {
                continue;
            }
            out.push(torrent.clone());
        }
    }
    out
}

pub fn sort_torrents_by_priority(torrents: &[TorrentInfo]) -> Vec<TorrentInfo> {
    let mut sorted = torrents.to_vec();
    sorted.sort_by_key(priority_key);
    sorted
}

fn priority_key(t: &TorrentInfo) -> (i32, i64, i64, i32) {
    let tag_priority = match t.tag.as_str() {
        "免费&2x上传" => 0,
        "免费" => 1,
        "50%下载&2x上传" => 2,
        "50%下载" => 3,
        "30%下载" => 4,
        _ => 5,
    };
    let ratio_scaled = if t.seeders > 0 {
        ((t.leechers as f64 / t.seeders as f64) * 1000.0) as i64
    } else {
        0
    };
    let size = t.size_bytes.unwrap_or(0) as i64;
    (tag_priority, -ratio_scaled, -size, -t.leechers)
}
