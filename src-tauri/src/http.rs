use anyhow::{Context, Result};
use reqwest::blocking::{Client, ClientBuilder};
use reqwest::header::{HeaderMap, HeaderValue, USER_AGENT};
use reqwest_cookie_store::{CookieStore, CookieStoreMutex};
use std::sync::Arc;

pub fn build_client() -> Result<Client> {
    let cookie_store = Arc::new(CookieStoreMutex::new(CookieStore::default()));
    ClientBuilder::new()
        .cookie_provider(cookie_store)
        .build()
        .context("failed to build HTTP client")
}

pub fn default_headers() -> HeaderMap {
    let mut headers = HeaderMap::new();
    headers.insert(USER_AGENT, HeaderValue::from_static("Mozilla/5.0"));
    headers
}
