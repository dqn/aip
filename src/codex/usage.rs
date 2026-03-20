use std::fs;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::path::Path;

use anyhow::{Result, anyhow};
use chrono::{DateTime, Utc};
use serde::Deserialize;
use serde_json::Value;

use crate::fs_util;
use crate::http::shared_client;
use crate::tool::Tool;

// These constants are reverse-engineered from the Codex CLI binary.
// They may need updating when the upstream tool changes.
// Last verified: 2026-02-21
const USAGE_URL: &str = "https://chatgpt.com/backend-api/wham/usage";
const TOKEN_URL: &str = "https://auth.openai.com/oauth/token";
const CLIENT_ID: &str = "app_EMoamEEZ73f0CkXaXp7hrann";

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
    pub resets_at: i64,
}

impl RateWindow {
    pub fn resets_at_utc(&self) -> Option<DateTime<Utc>> {
        DateTime::from_timestamp(self.resets_at, 0)
    }
}

#[derive(Debug, Deserialize)]
struct UsageResponse {
    rate_limit: Option<RateLimits>,
}

#[derive(Debug, Deserialize)]
struct TokenData {
    access_token: String,
    refresh_token: Option<String>,
    account_id: Option<String>,
}

#[derive(Debug, Deserialize)]
struct RefreshResponse {
    access_token: Option<String>,
    refresh_token: Option<String>,
    id_token: Option<String>,
}

fn read_tokens(raw: &Value) -> Result<TokenData> {
    let tokens_value = raw
        .get("tokens")
        .ok_or_else(|| anyhow!("no tokens in auth.json"))?;
    Ok(serde_json::from_value(tokens_value.clone())?)
}

async fn read_auth(path: &Path) -> Result<(Value, TokenData)> {
    let content = tokio::fs::read_to_string(path).await?;
    let raw: Value = serde_json::from_str(&content)?;
    let tokens = read_tokens(&raw)?;
    Ok((raw, tokens))
}

async fn do_refresh_token(refresh_token: &str) -> Result<RefreshResponse> {
    let resp = shared_client()
        .post(TOKEN_URL)
        .json(&serde_json::json!({
            "client_id": CLIENT_ID,
            "grant_type": "refresh_token",
            "refresh_token": refresh_token,
            "scope": "openid profile email",
        }))
        .send()
        .await?;

    if !resp.status().is_success() {
        return Err(anyhow!(
            "token refresh failed ({}): {}",
            resp.status(),
            resp.text().await.unwrap_or_default()
        ));
    }

    Ok(resp.json().await?)
}

fn apply_refresh(raw: &mut Value, resp: &RefreshResponse) -> Result<()> {
    let tokens = raw
        .get_mut("tokens")
        .ok_or_else(|| anyhow!("malformed auth.json: missing 'tokens' key"))?;
    if let Some(new_access) = &resp.access_token {
        tokens["access_token"] = Value::String(new_access.clone());
    }
    if let Some(new_refresh) = &resp.refresh_token {
        tokens["refresh_token"] = Value::String(new_refresh.clone());
    }
    if let Some(new_id) = &resp.id_token {
        tokens["id_token"] = Value::String(new_id.clone());
    }
    Ok(())
}

async fn fetch_usage_api(tokens: &TokenData) -> Result<reqwest::Response> {
    let mut req = shared_client()
        .get(USAGE_URL)
        .header("Authorization", format!("Bearer {}", tokens.access_token));

    if let Some(account_id) = &tokens.account_id {
        req = req.header("ChatGPT-Account-Id", account_id);
    }

    Ok(req.send().await?)
}

async fn parse_usage_response(resp: reqwest::Response) -> Result<Option<RateLimits>> {
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

async fn fetch_from_auth_path(path: &Path) -> Result<Option<RateLimits>> {
    let (mut raw, tokens) = read_auth(path).await?;

    let resp = fetch_usage_api(&tokens).await?;

    match resp.status() {
        reqwest::StatusCode::UNAUTHORIZED => {}
        _ => return parse_usage_response(resp).await,
    }

    // Token expired, try refreshing
    let refresh_token = tokens
        .refresh_token
        .as_deref()
        .ok_or_else(|| anyhow!("auth.json does not contain a refresh_token"))?;
    let refresh_resp = do_refresh_token(refresh_token).await?;
    apply_refresh(&mut raw, &refresh_resp)?;

    let new_access_token = refresh_resp
        .access_token
        .as_deref()
        .ok_or_else(|| anyhow!("token refresh returned no new access token"))?;
    if new_access_token == tokens.access_token {
        return Err(anyhow!("token refresh returned the same access token"));
    }

    let path = path.to_owned();
    let serialized = serde_json::to_string_pretty(&raw)?;
    tokio::task::spawn_blocking(move || {
        fs_util::atomic_write(&path, &serialized)?;
        #[cfg(unix)]
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600))?;
        Ok::<(), anyhow::Error>(())
    })
    .await??;

    let new_tokens = read_tokens(&raw)?;
    let resp = fetch_usage_api(&new_tokens).await?;
    parse_usage_response(resp).await
}

pub async fn fetch_usage() -> Result<Option<RateLimits>> {
    let path = Tool::Codex.home_dir()?.join("auth.json");
    if !path.exists() {
        return Ok(None);
    }
    fetch_from_auth_path(&path).await
}

pub async fn fetch_usage_from_auth(path: &Path) -> Result<Option<RateLimits>> {
    if !path.exists() {
        return Ok(None);
    }
    fetch_from_auth_path(path).await
}

#[cfg(test)]
mod tests {
    use super::*;

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
        assert_eq!(tokens.refresh_token.as_deref(), Some("ref"));
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
        assert!(tokens.refresh_token.is_none());
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

    #[cfg(unix)]
    #[test]
    fn atomic_write_with_permissions_sets_0o600() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("auth.json");

        fs_util::atomic_write(&path, r#"{"tokens":{}}"#).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();

        let mode = fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "credential file should be owner-only (0o600)");
    }
}
