use anyhow::{anyhow, Context, Result};
use serde::{Deserialize, Serialize};
use std::path::Path;

#[derive(Debug, Clone)]
pub struct Config {
    pub byr_username: String,
    pub byr_password: String,
    pub qb_host: String,
    pub qb_username: String,
    pub qb_password: String,
    pub qb_download_path: String,
    pub dry_run: bool,
    pub include_categories: Option<Vec<String>>,
    pub auto_rotate_enabled: bool,
    pub min_seeding_hours_before_remove: u64,
    pub max_leechers_for_stale: i32,
    pub max_recent_upspeed_kib: u64,
    pub max_remove_per_cycle: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConfigForm {
    pub byr_username: String,
    pub byr_password: String,
    pub qb_host: String,
    pub qb_username: String,
    pub qb_password: String,
    pub qb_download_path: String,
    pub download_budget_gb: String,
    pub include_categories: String,
    pub auto_rotate_enabled: bool,
    pub min_seeding_hours_before_remove: String,
    pub max_leechers_for_stale: String,
    pub max_recent_upspeed_kib: String,
    pub max_remove_per_cycle: String,
}

impl Default for ConfigForm {
    fn default() -> Self {
        Self {
            byr_username: String::new(),
            byr_password: String::new(),
            qb_host: "http://127.0.0.1:8080".to_string(),
            qb_username: "admin".to_string(),
            qb_password: String::new(),
            qb_download_path: "H:/BT".to_string(),
            download_budget_gb: "200".to_string(),
            include_categories: String::new(),
            auto_rotate_enabled: false,
            min_seeding_hours_before_remove: "72".to_string(),
            max_leechers_for_stale: "1".to_string(),
            max_recent_upspeed_kib: "50".to_string(),
            max_remove_per_cycle: "3".to_string(),
        }
    }
}

impl ConfigForm {
    pub fn load_from_path(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();
        if !path.exists() {
            return Ok(Self::default());
        }

        let content = std::fs::read_to_string(path)
            .with_context(|| format!("failed to read config file: {}", path.display()))?;
        let defaults = Self::default();
        let mut form = Self::default();

        for line in content.lines() {
            let trimmed = line.trim();
            if trimmed.is_empty() || trimmed.starts_with('#') {
                continue;
            }
            let Some((key, value)) = trimmed.split_once('=') else {
                continue;
            };
            let value = normalize_env_value(value);
            match key.trim() {
                "BYRBT_USERNAME" => form.byr_username = value,
                "BYRBT_PASSWORD" => form.byr_password = value,
                "QBITTORRENT_HOST" => form.qb_host = value,
                "QBITTORRENT_USERNAME" => form.qb_username = value,
                "QBITTORRENT_PASSWORD" => form.qb_password = value,
                "QBITTORRENT_DOWNLOAD_PATH" => form.qb_download_path = value,
                "DOWNLOAD_BUDGET_GB" => form.download_budget_gb = value,
                "INCLUDE_CATEGORIES" => form.include_categories = value,
                "AUTO_ROTATE_ENABLED" => form.auto_rotate_enabled = parse_bool_value(&value),
                "MIN_SEEDING_HOURS_BEFORE_REMOVE" => form.min_seeding_hours_before_remove = value,
                "MAX_LEECHERS_FOR_STALE" => form.max_leechers_for_stale = value,
                "MAX_RECENT_UPSPEED_KIB" => form.max_recent_upspeed_kib = value,
                "MAX_REMOVE_PER_CYCLE" => form.max_remove_per_cycle = value,
                _ => {}
            }
        }

        if form.qb_host.trim().is_empty() {
            form.qb_host = defaults.qb_host;
        }
        if form.qb_username.trim().is_empty() {
            form.qb_username = defaults.qb_username;
        }
        if form.qb_download_path.trim().is_empty() {
            form.qb_download_path = defaults.qb_download_path;
        }
        if form.download_budget_gb.trim().is_empty() {
            form.download_budget_gb = defaults.download_budget_gb;
        }
        if form.min_seeding_hours_before_remove.trim().is_empty() {
            form.min_seeding_hours_before_remove = defaults.min_seeding_hours_before_remove;
        }
        if form.max_leechers_for_stale.trim().is_empty() {
            form.max_leechers_for_stale = defaults.max_leechers_for_stale;
        }
        if form.max_recent_upspeed_kib.trim().is_empty() {
            form.max_recent_upspeed_kib = defaults.max_recent_upspeed_kib;
        }
        if form.max_remove_per_cycle.trim().is_empty() {
            form.max_remove_per_cycle = defaults.max_remove_per_cycle;
        }

        Ok(form)
    }

    pub fn save_to_path(&self, path: impl AsRef<Path>) -> Result<()> {
        let path = path.as_ref();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).with_context(|| {
                format!("failed to create config directory: {}", parent.display())
            })?;
        }
        let content = format!(
            "BYRBT_USERNAME=\"{}\"\nBYRBT_PASSWORD=\"{}\"\n\nQBITTORRENT_HOST=\"{}\"\nQBITTORRENT_USERNAME=\"{}\"\nQBITTORRENT_PASSWORD=\"{}\"\nQBITTORRENT_DOWNLOAD_PATH=\"{}\"\n\nDOWNLOAD_BUDGET_GB={}\nINCLUDE_CATEGORIES=\"{}\"\nAUTO_ROTATE_ENABLED={}\nMIN_SEEDING_HOURS_BEFORE_REMOVE={}\nMAX_LEECHERS_FOR_STALE={}\nMAX_RECENT_UPSPEED_KIB={}\nMAX_REMOVE_PER_CYCLE={}\n",
            escape_env_value(&self.byr_username),
            escape_env_value(&self.byr_password),
            escape_env_value(&self.qb_host),
            escape_env_value(&self.qb_username),
            escape_env_value(&self.qb_password),
            escape_env_value(&self.qb_download_path),
            self.download_budget_gb.trim(),
            escape_env_value(&self.include_categories),
            self.auto_rotate_enabled,
            self.min_seeding_hours_before_remove.trim(),
            self.max_leechers_for_stale.trim(),
            self.max_recent_upspeed_kib.trim(),
            self.max_remove_per_cycle.trim(),
        );

        std::fs::write(path, content)
            .with_context(|| format!("failed to write config file: {}", path.display()))
    }

    pub fn into_core_config(&self, dry_run: bool) -> Result<Config> {
        validate_required("BYRBT_USERNAME", &self.byr_username)?;
        validate_required("BYRBT_PASSWORD", &self.byr_password)?;
        let mut config = self.into_qb_only_config()?;
        config.byr_username = self.byr_username.trim().to_string();
        config.byr_password = self.byr_password.clone();
        config.dry_run = dry_run;
        config.include_categories = parse_category_filter(Some(self.include_categories.clone()));
        Ok(config)
    }

    pub fn into_qb_only_config(&self) -> Result<Config> {
        validate_required("QBITTORRENT_HOST", &self.qb_host)?;
        validate_required("QBITTORRENT_USERNAME", &self.qb_username)?;
        validate_required("QBITTORRENT_PASSWORD", &self.qb_password)?;

        let budget = self.download_budget_gb.trim();
        if !budget.is_empty() {
            let parsed = budget
                .parse::<f64>()
                .map_err(|_| anyhow!("DOWNLOAD_BUDGET_GB 必须是数字"))?;
            if parsed <= 0.0 {
                return Err(anyhow!("DOWNLOAD_BUDGET_GB 必须大于 0"));
            }
        }

        let min_seeding_hours_before_remove = parse_u64_config(
            "MIN_SEEDING_HOURS_BEFORE_REMOVE",
            &self.min_seeding_hours_before_remove,
        )?;
        let max_recent_upspeed_kib =
            parse_u64_config("MAX_RECENT_UPSPEED_KIB", &self.max_recent_upspeed_kib)?;
        let max_remove_per_cycle =
            parse_usize_config("MAX_REMOVE_PER_CYCLE", &self.max_remove_per_cycle)?;
        let max_leechers_for_stale = parse_i32_config(
            "MAX_LEECHERS_FOR_STALE",
            &self.max_leechers_for_stale,
        )?;

        Ok(Config {
            byr_username: String::new(),
            byr_password: String::new(),
            qb_host: self.qb_host.trim().trim_end_matches('/').to_string(),
            qb_username: self.qb_username.trim().to_string(),
            qb_password: self.qb_password.clone(),
            qb_download_path: if self.qb_download_path.trim().is_empty() {
                "H:/BT".to_string()
            } else {
                self.qb_download_path.trim().to_string()
            },
            dry_run: false,
            include_categories: None,
            auto_rotate_enabled: self.auto_rotate_enabled,
            min_seeding_hours_before_remove,
            max_leechers_for_stale,
            max_recent_upspeed_kib,
            max_remove_per_cycle,
        })
    }

    pub fn apply_process_env(&self) {
        set_or_remove("BYRBT_USERNAME", &self.byr_username);
        set_or_remove("BYRBT_PASSWORD", &self.byr_password);
        set_or_remove("QBITTORRENT_HOST", &self.qb_host);
        set_or_remove("QBITTORRENT_USERNAME", &self.qb_username);
        set_or_remove("QBITTORRENT_PASSWORD", &self.qb_password);
        set_or_remove("QBITTORRENT_DOWNLOAD_PATH", &self.qb_download_path);
        set_or_remove("DOWNLOAD_BUDGET_GB", &self.download_budget_gb);
        set_or_remove("INCLUDE_CATEGORIES", &self.include_categories);
        set_or_remove(
            "AUTO_ROTATE_ENABLED",
            if self.auto_rotate_enabled { "true" } else { "false" },
        );
        set_or_remove(
            "MIN_SEEDING_HOURS_BEFORE_REMOVE",
            &self.min_seeding_hours_before_remove,
        );
        set_or_remove("MAX_LEECHERS_FOR_STALE", &self.max_leechers_for_stale);
        set_or_remove("MAX_RECENT_UPSPEED_KIB", &self.max_recent_upspeed_kib);
        set_or_remove("MAX_REMOVE_PER_CYCLE", &self.max_remove_per_cycle);
    }
}

fn validate_required(key: &str, value: &str) -> Result<()> {
    if value.trim().is_empty() {
        return Err(anyhow!("缺少配置项: {key}"));
    }
    Ok(())
}

fn parse_category_filter(value: Option<String>) -> Option<Vec<String>> {
    value
        .map(|raw| {
            raw.split(',')
                .map(|item| item.trim().to_string())
                .filter(|item| !item.is_empty())
                .collect::<Vec<_>>()
        })
        .filter(|items| !items.is_empty())
}

fn normalize_env_value(value: &str) -> String {
    let trimmed = value.trim();
    if trimmed.len() >= 2 {
        let bytes = trimmed.as_bytes();
        if (bytes[0] == b'"' && bytes[trimmed.len() - 1] == b'"')
            || (bytes[0] == b'\'' && bytes[trimmed.len() - 1] == b'\'')
        {
            return trimmed[1..trimmed.len() - 1]
                .replace("\\\"", "\"")
                .replace("\\\\", "\\");
        }
    }
    trimmed.to_string()
}

fn escape_env_value(value: &str) -> String {
    value.replace('\\', "\\\\").replace('"', "\\\"")
}

fn parse_bool_value(value: &str) -> bool {
    matches!(
        value.trim().to_ascii_lowercase().as_str(),
        "1" | "true" | "yes" | "on"
    )
}

fn parse_u64_config(key: &str, value: &str) -> Result<u64> {
    let parsed = value
        .trim()
        .parse::<u64>()
        .map_err(|_| anyhow!("{key} 必须是非负整数"))?;
    Ok(parsed)
}

fn parse_usize_config(key: &str, value: &str) -> Result<usize> {
    let parsed = value
        .trim()
        .parse::<usize>()
        .map_err(|_| anyhow!("{key} 必须是非负整数"))?;
    Ok(parsed.max(1))
}

fn parse_i32_config(key: &str, value: &str) -> Result<i32> {
    let parsed = value
        .trim()
        .parse::<i32>()
        .map_err(|_| anyhow!("{key} 必须是整数"))?;
    Ok(parsed)
}

fn set_or_remove(key: &str, value: &str) {
    if value.trim().is_empty() {
        std::env::remove_var(key);
    } else {
        std::env::set_var(key, value.trim());
    }
}
