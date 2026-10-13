use std::path::Path;

use anyhow::{Result, anyhow};
use chrono::{DateTime, Utc};
use serde::Deserialize;
use serde_json::Value;

use crate::http::shared_client;

// These constants are reverse-engineered from the Codex CLI binary.
// They may need updating when the upstream tool changes.
// Last verified: 2026-02-21
const USAGE_URL: &str = "https://chatgpt.com/backend-api/wham/usage";

#[derive(Debug, Deserialize)]
pub struct RateLimits {
    #[serde(rename = "primary_window")]
    pub primary: Option<RateWindow>,
    #[serde(rename = "secondary_window")]
    pub secondary: Option<RateWindow>,
}

#[derive(Debug, Deserialize)]
pub struct RateWindow {
    pub used_percent: f64,
    #[serde(rename = "reset_at")]
    pub resets_at: Option<i64>,
}

impl RateWindow {
    pub fn resets_at_utc(&self) -> Option<DateTime<Utc>> {
        self.resets_at
            .and_then(|ts| DateTime::from_timestamp(ts, 0))
    }
}

#[derive(Debug, Deserialize)]
struct UsageResponse {
    rate_limit: Option<RateLimits>,
}

#[derive(Debug, Deserialize)]
struct TokenData {
    access_token: String,
    account_id: Option<String>,
}

fn read_tokens(raw: &Value) -> Result<TokenData> {
    let tokens_value = raw
        .get("tokens")
        .ok_or_else(|| anyhow!("no tokens in auth.json"))?;
    Ok(serde_json::from_value(tokens_value.clone())?)
}

async fn fetch_from_auth_path(
    path: &Path,
    client: &reqwest::Client,
    url: &str,
) -> Result<Option<RateLimits>> {
    let content = tokio::fs::read_to_string(path).await?;
    let raw: Value = serde_json::from_str(&content)?;
    let tokens = read_tokens(&raw)?;
    let mut request = client
        .get(url)
        .header("Authorization", format!("Bearer {}", tokens.access_token));
    if let Some(id) = tokens.account_id {
        request = request.header("ChatGPT-Account-Id", id);
    }
    // Codex owns refresh for every running profile, including deselected ones.
    // Monitoring must never rotate or overwrite those credentials.
    parse_usage_response(request.send().await?).await
}

async fn parse_usage_response(resp: reqwest::Response) -> Result<Option<RateLimits>> {
    if resp.status() == reqwest::StatusCode::UNAUTHORIZED {
        return Err(anyhow!(
            "Codex credentials expired; launch this profile with 'aip codex' to refresh them"
        ));
    }
    if !resp.status().is_success() {
        return Err(anyhow!(
            "usage API returned status {}: {}",
            resp.status(),
            resp.text().await.unwrap_or_default()
        ));
    }
    let usage: UsageResponse = resp.json().await?;
    Ok(usage.rate_limit)
}

pub async fn fetch_usage_from_auth(path: &Path) -> Result<Option<RateLimits>> {
    match fetch_from_auth_path(path, shared_client(), USAGE_URL).await {
        Err(e)
            if e.downcast_ref::<std::io::Error>()
                .is_some_and(|io| io.kind() == std::io::ErrorKind::NotFound) =>
        {
            Ok(None)
        }
        other => other,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn expired_usage_does_not_refresh_or_write_auth() {
        use std::io::{Read, Write};
        let home = tempfile::tempdir().unwrap();
        let path = home.path().join("auth.json");
        let contents = r#"{"tokens":{"access_token":"usage-sentinel","refresh_token":"refresh-sentinel","account_id":"old-account"}}"#;
        std::fs::write(&path, contents).unwrap();
        let server = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = server.local_addr().unwrap();
        let handle = std::thread::spawn(move || {
            let (mut stream, _) = server.accept().unwrap();
            stream
                .set_read_timeout(Some(std::time::Duration::from_secs(5)))
                .unwrap();
            let mut request_bytes = Vec::new();
            let mut buffer = [0u8; 2048];
            while !request_bytes.windows(4).any(|part| part == b"\r\n\r\n") {
                let size = stream.read(&mut buffer).unwrap();
                assert!(size > 0);
                request_bytes.extend_from_slice(&buffer[..size]);
            }
            let request = String::from_utf8_lossy(&request_bytes).to_lowercase();
            assert!(request.starts_with("get /usage "));
            assert!(request.contains("authorization: bearer usage-sentinel"));
            assert!(request.contains("chatgpt-account-id: old-account"));
            stream
                .write_all(
                    b"HTTP/1.1 401 Unauthorized\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                )
                .unwrap();
        });
        let client = reqwest::Client::builder().no_proxy().build().unwrap();
        let result = fetch_from_auth_path(&path, &client, &format!("http://{address}/usage")).await;
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("launch this profile")
        );
        assert_eq!(std::fs::read_to_string(&path).unwrap(), contents);
        assert!(!path.with_extension("tmp").exists());
        handle.join().unwrap();
    }

    #[test]
    fn read_tokens_with_refresh_token() {
        let raw: Value = serde_json::json!({
            "tokens": {
                "access_token": "acc",
                "refresh_token": "ref",
            }
        });
        let tokens = read_tokens(&raw).unwrap();
        assert_eq!(tokens.access_token, "acc");
    }

    #[test]
    fn read_tokens_without_refresh_token() {
        let raw: Value = serde_json::json!({
            "tokens": {
                "access_token": "acc",
            }
        });
        let tokens = read_tokens(&raw).unwrap();
        assert_eq!(tokens.access_token, "acc");
    }

    #[test]
    fn read_tokens_missing_access_token_fails() {
        let raw: Value = serde_json::json!({
            "tokens": {
                "refresh_token": "ref",
            }
        });
        assert!(read_tokens(&raw).is_err());
    }

    #[test]
    fn read_tokens_missing_tokens_key_fails() {
        let raw: Value = serde_json::json!({});
        let err = read_tokens(&raw).unwrap_err();
        assert!(err.to_string().contains("no tokens in auth.json"));
    }

    #[test]
    fn deserialize_rate_window_with_null_reset_at() {
        let json = r#"{"used_percent": 42.0, "reset_at": null}"#;
        let window: RateWindow = serde_json::from_str(json).unwrap();
        assert_eq!(window.used_percent, 42.0);
        assert!(window.resets_at.is_none());
        assert!(window.resets_at_utc().is_none());
    }

    #[test]
    fn deserialize_rate_window_with_valid_reset_at() {
        let json = r#"{"used_percent": 10.0, "reset_at": 1700000000}"#;
        let window: RateWindow = serde_json::from_str(json).unwrap();
        assert_eq!(window.resets_at, Some(1700000000));
        assert!(window.resets_at_utc().is_some());
    }
}
