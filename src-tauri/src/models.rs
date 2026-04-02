use serde::Deserialize;

#[derive(Debug, Clone)]
pub struct TorrentInfo {
    pub seed_id: String,
    pub category: String,
    pub title: String,
    pub tag: String,
    pub size_text: String,
    pub size_bytes: Option<u64>,
    pub seeders: i32,
    pub leechers: i32,
    pub finished: i32,
    pub is_hot: bool,
    pub is_new: bool,
    pub is_recommended: bool,
}

#[derive(Debug, Deserialize)]
pub struct ApiEnvelope<T> {
    pub success: bool,
    pub code: i32,
    pub msg: String,
    pub data: T,
}

#[derive(Debug, Deserialize)]
pub struct LoginData {
    pub auth_token: Option<String>,
    #[serde(rename = "verifyToken")]
    pub verify_token: Option<String>,
    #[serde(rename = "userId")]
    pub user_id: Option<i64>,
    #[serde(rename = "needMobileVerify")]
    pub need_mobile_verify: Option<bool>,
    #[serde(rename = "needResetPassword")]
    pub need_reset_password: Option<bool>,
}

#[derive(Debug)]
pub struct PlannerConfig {
    pub min_free_space_bytes: u64,
    pub download_budget_bytes: Option<u64>,
}

#[derive(Debug)]
pub struct SpaceSnapshot {
    pub free_space_bytes: u64,
    pub downloading_remaining_bytes: u64,
}

#[derive(Debug, Clone)]
pub struct QbTorrent {
    pub name: String,
    pub hash: String,
    pub state: String,
    pub tags: String,
    pub total_size: u64,
    pub completed: u64,
    pub added_on: i64,
    pub upspeed: u64,
    pub num_leechs: i32,
    pub num_seeds: i32,
    pub progress: f64,
}
