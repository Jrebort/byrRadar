use anyhow::{anyhow, Context, Result};
use reqwest::blocking::Client;
use reqwest::header::AUTHORIZATION;
use scraper::{Html, Selector};
use select::document::Document;
use select::predicate::Name;
use serde_json::json;

use crate::config::Config;
use crate::http::{build_client, default_headers};
use crate::models::{ApiEnvelope, LoginData, TorrentInfo};
use crate::planner::parse_size_to_bytes;

pub struct ByrClient {
    client: Client,
    auth_token: Option<String>,
}

impl ByrClient {
    pub fn login(config: &Config) -> Result<Self> {
        let client = build_client()?;
        let headers = default_headers();
        let login_type = if looks_like_phone_number(&config.byr_username) {
            "mobile"
        } else {
            "username"
        };
        let payload = match login_type {
            "mobile" => json!({
                "type": "mobile",
                "mobile": config.byr_username,
                "password": config.byr_password,
                "remember": true
            }),
            _ => json!({
                "type": "username",
                "username": config.byr_username,
                "password": config.byr_password,
                "remember": true
            }),
        };

        let login_resp = client
            .post("https://byr.pt/api/v2/login.php")
            .headers(headers)
            .json(&payload)
            .send()
            .context("failed to submit BYR API login request")?;
        println!("BYR login POST status: {}", login_resp.status());

        let envelope: ApiEnvelope<LoginData> = login_resp
            .json()
            .context("failed to parse BYR login API response")?;

        println!(
            "BYR login API envelope: success={}, code={}, msg={}",
            envelope.success, envelope.code, envelope.msg
        );
        println!(
            "BYR login data: auth_token={}, need_mobile_verify={}, need_reset_password={}, user_id={:?}, verify_token={}",
            envelope.data.auth_token.is_some(),
            envelope.data.need_mobile_verify.unwrap_or(false),
            envelope.data.need_reset_password.unwrap_or(false),
            envelope.data.user_id,
            envelope.data.verify_token.is_some(),
        );

        let auth_token = envelope.data.auth_token.clone();
        if auth_token.is_none() {
            return Err(anyhow!("BYR login API did not return auth_token"));
        }

        Ok(Self { client, auth_token })
    }

    pub fn fetch_free_torrents(&self) -> Result<Vec<TorrentInfo>> {
        let html = self
            .request("https://byr.pt/torrents.php")?
            .text()
            .context("failed to read torrents page")?;

        let cat_selector = Selector::parse(".cat-link").unwrap();
        let title_selector =
            Selector::parse("table.torrentname a[href*='details.php?id=']").unwrap();
        let promo_selector = Selector::parse("img[src='/pic/trans.gif']").unwrap();
        let raw_document = Document::from(html.as_str());

        let mut torrents = Vec::new();
        for row in raw_document.find(Name("tr")) {
            let class_name = row.attr("class").unwrap_or_default();
            if class_name != "free_bg" && class_name != "twoupfree_bg" {
                continue;
            }

            let tds: Vec<_> = row
                .children()
                .filter(|child| child.name() == Some("td"))
                .collect();
            if tds.len() < 8 {
                continue;
            }

            let tds_html: Vec<String> = tds.iter().map(|td| td.html()).collect();

            let cat_doc = Html::parse_fragment(&tds_html[0]);
            let cat = cat_doc
                .select(&cat_selector)
                .next()
                .map(|x| x.text().collect::<String>().trim().to_string())
                .unwrap_or_default();

            let main_doc = Html::parse_fragment(&tds_html[1]);
            let Some(title_link) = main_doc.select(&title_selector).next() else {
                continue;
            };

            let href = title_link.value().attr("href").unwrap_or_default();
            let seed_id = href
                .split("id=")
                .nth(1)
                .and_then(|rest| rest.split('&').next())
                .unwrap_or_default()
                .to_string();
            if seed_id.is_empty() {
                continue;
            }

            let title = title_link
                .value()
                .attr("title")
                .unwrap_or_else(|| href)
                .to_string();

            let tag = map_tag(class_name)
                .strip_prefix("")
                .filter(|x| !x.is_empty())
                .map(str::to_string)
                .or_else(|| {
                    main_doc
                        .select(&promo_selector)
                        .filter_map(|img| img.value().attr("class"))
                        .flat_map(|v| v.split_whitespace())
                        .find(|cls| cls.starts_with("pro_"))
                        .map(|cls| map_tag(cls.trim_start_matches("pro_")))
                        .filter(|x| !x.is_empty())
                        .map(str::to_string)
                })
                .unwrap_or_default()
                .to_string();

            let size_text = collect_text(&tds[4]);
            let size_bytes = parse_size_to_bytes(&size_text);
            let seeders = parse_int(&collect_text(&tds[5]));
            let leechers = parse_int(&collect_text(&tds[6]));
            let finished = parse_int(&collect_text(&tds[7]));
            let main_html = tds_html[1].clone();

            torrents.push(TorrentInfo {
                seed_id,
                category: cat.clone(),
                title: format!("[{cat}] {title}"),
                tag,
                size_text,
                size_bytes,
                seeders,
                leechers,
                finished,
                is_hot: main_html.contains("class=\"hot\""),
                is_new: main_html.contains("class=\"new\""),
                is_recommended: main_html.contains("class=\"recommended\""),
            });
        }

        Ok(torrents)
    }

    pub fn download_torrent_file(&self, torrent_id: &str) -> Result<Vec<u8>> {
        let url = format!("https://byr.pt/download.php?id={torrent_id}");
        let response = self
            .request(&url)?
            .error_for_status()
            .with_context(|| format!("failed to download torrent file for {torrent_id}"))?;
        let content_type = response
            .headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_string();
        let bytes = response
            .bytes()
            .context("failed to read downloaded torrent bytes")?
            .to_vec();

        println!(
            "BYR torrent download: id={}, content_type='{}', bytes={}",
            torrent_id,
            content_type,
            bytes.len()
        );
        let preview_len = bytes.len().min(120);
        let preview = String::from_utf8_lossy(&bytes[..preview_len]);
        println!(
            "BYR torrent download preview: {}",
            preview.replace('\n', " ")
        );

        Ok(bytes)
    }

    fn request(&self, url: &str) -> Result<reqwest::blocking::Response> {
        let mut request = self.client.get(url);
        if let Some(token) = &self.auth_token {
            request = request.header(AUTHORIZATION, format!("Bearer {token}"));
        }
        request
            .send()
            .with_context(|| format!("failed to fetch {url}"))
    }
}

fn looks_like_phone_number(value: &str) -> bool {
    value.len() == 11 && value.chars().all(|ch| ch.is_ascii_digit())
}

fn parse_int(value: &str) -> i32 {
    value
        .chars()
        .filter(|ch| ch.is_ascii_digit())
        .collect::<String>()
        .parse::<i32>()
        .unwrap_or(-1)
}

fn collect_text(node: &select::node::Node) -> String {
    node.text().split_whitespace().collect::<Vec<_>>().join(" ")
}

fn map_tag(raw: &str) -> &'static str {
    match raw {
        "free_bg" | "free" => "免费",
        "twoupfree_bg" | "free2up" | "twoupfree" => "免费&2x上传",
        "twoup" | "2up" => "2x上传",
        "halfdown" | "50pctdown" => "50%下载",
        "twouphalfdown" | "50pctdown2up" => "50%下载&2x上传",
        "thirtypercentdown" | "30pctdown" => "30%下载",
        _ => "",
    }
}
