use std::collections::HashMap;
use std::fs;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::time::Duration;

use anyhow::{Context, Result, anyhow};
use chrono::{DateTime, Utc};
use serde::Deserialize;
use serde_json::Value;

use crate::fs_util;
use crate::http::shared_client;
use crate::tool::Tool;

const CLIENT_ID: &str = "9d1c250a-e61b-44d9-88ed-5944d1962f5e";
const TOKEN_URL: &str = "https://console.anthropic.com/v1/oauth/token";

#[derive(Debug, Deserialize)]
struct OAuthData {
    #[serde(rename = "accessToken")]
    access_token: String,
    #[serde(rename = "refreshToken")]
    refresh_token: Option<String>,
    #[serde(rename = "expiresAt")]
    expires_at: Option<u64>,
    #[serde(rename = "planType")]
    plan_type: Option<String>,
}

#[derive(Debug, Deserialize)]
struct TokenResponse {
    access_token: String,
    refresh_token: Option<String>,
    expires_in: Option<u64>,
}

#[derive(Debug, Deserialize)]
pub struct UsageResponse {
    pub five_hour: UsageWindow,
    pub seven_day: UsageWindow,
}

#[derive(Debug, Deserialize)]
pub struct UsageWindow {
    pub utilization: f64,
    pub resets_at: Option<DateTime<Utc>>,
}

#[derive(Debug)]
pub struct RateLimitError {
    pub retry_after: Duration,
}

impl std::fmt::Display for RateLimitError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "rate limited (retry after {}s)",
            self.retry_after.as_secs()
        )
    }
}

impl std::error::Error for RateLimitError {}

pub struct ProfileInfo {
    pub plan_type: Option<String>,
}

fn read_oauth(raw: &Value) -> Result<OAuthData> {
    let oauth_value = raw
        .get("claudeAiOauth")
        .ok_or_else(|| anyhow!("no OAuth data in credentials"))?;
    Ok(serde_json::from_value(oauth_value.clone())?)
}

fn is_token_expired(oauth: &OAuthData) -> bool {
    match oauth.expires_at {
        // 5 minute buffer
        Some(expires_at) => {
            let now_ms = Utc::now().timestamp_millis().max(0) as u64;
            now_ms.saturating_add(300_000) >= expires_at
        }
        None => false,
    }
}

async fn refresh_token(oauth: &OAuthData) -> Result<TokenResponse> {
    let refresh_token = oauth
        .refresh_token
        .as_ref()
        .ok_or_else(|| anyhow!("no refresh token available"))?;

    let resp = shared_client()
        .post(TOKEN_URL)
        .form(&[
            ("grant_type", "refresh_token"),
            ("refresh_token", refresh_token.as_str()),
            ("client_id", CLIENT_ID),
        ])
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

fn apply_token_response(raw: &mut Value, token_resp: &TokenResponse) -> Result<()> {
    let oauth = raw
        .get_mut("claudeAiOauth")
        .ok_or_else(|| anyhow!("no claudeAiOauth key in credentials"))?;
    oauth["accessToken"] = Value::String(token_resp.access_token.clone());
    if let Some(new_refresh) = &token_resp.refresh_token {
        oauth["refreshToken"] = Value::String(new_refresh.clone());
    }
    let expires_in = token_resp.expires_in.unwrap_or(3600).clamp(60, 86400);
    let now_ms = Utc::now().timestamp_millis().max(0) as u64;
    let new_expires_at = now_ms + expires_in.saturating_mul(1000);
    oauth["expiresAt"] = Value::Number(new_expires_at.into());
    Ok(())
}

pub async fn fetch_usage_with_token(token: &str) -> Result<UsageResponse> {
    if token.is_empty() {
        return Err(anyhow!("access token is empty"));
    }

    let resp = shared_client()
        .get("https://api.anthropic.com/api/oauth/usage")
        .header("Authorization", format!("Bearer {}", token))
        .header("anthropic-beta", "oauth-2025-04-20")
        .send()
        .await?;

    if resp.status() == reqwest::StatusCode::TOO_MANY_REQUESTS {
        let retry_after_secs = resp
            .headers()
            .get("retry-after")
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.parse::<u64>().ok())
            .unwrap_or(60);
        return Err(RateLimitError {
            retry_after: Duration::from_secs(retry_after_secs),
        }
        .into());
    }

    if !resp.status().is_success() {
        return Err(anyhow!(
            "usage API returned status {}: {}",
            resp.status(),
            resp.text().await.unwrap_or_default()
        ));
    }

    Ok(resp.json().await?)
}

async fn get_access_token_from_credentials(
    path: &Path,
    is_current: bool,
) -> Result<(String, ProfileInfo)> {
    let content = tokio::fs::read_to_string(path).await?;
    let mut raw: Value = serde_json::from_str(&content)?;
    let oauth = read_oauth(&raw)?;

    let info = ProfileInfo {
        plan_type: oauth.plan_type.clone(),
    };

    // For the current profile, always use the token as-is. aip is read-only
    // for the current profile; if the token is actually expired server-side,
    // the usage API will return 429+retry-after:0, which triggers stale-cache
    // preservation. The user can manually refresh with 'r'.
    if is_current || !is_token_expired(&oauth) {
        return Ok((oauth.access_token, info));
    }

    let token_resp = refresh_token(&oauth)
        .await
        .context("Refresh token expired (switch to this profile to re-auth)")?;
    let access_token = token_resp.access_token.clone();
    apply_token_response(&mut raw, &token_resp)?;
    let new_content = serde_json::to_string_pretty(&raw)?;
    let path = path.to_owned();
    tokio::task::spawn_blocking(move || {
        fs_util::atomic_write(&path, &new_content)?;
        #[cfg(unix)]
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600))?;
        Ok::<(), anyhow::Error>(())
    })
    .await??;

    Ok((access_token, info))
}

pub async fn refresh_credentials_if_expired(path: &Path) -> Result<String> {
    let content = tokio::fs::read_to_string(path).await?;
    let mut raw: Value = serde_json::from_str(&content)?;
    let oauth = read_oauth(&raw)?;

    if !is_token_expired(&oauth) {
        return Ok(content);
    }

    let token_resp = refresh_token(&oauth).await?;
    apply_token_response(&mut raw, &token_resp)?;
    let refreshed = serde_json::to_string_pretty(&raw)?;
    let path = path.to_owned();
    let write_content = refreshed.clone();
    tokio::task::spawn_blocking(move || {
        fs_util::atomic_write(&path, &write_content)?;
        #[cfg(unix)]
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600))?;
        Ok::<(), anyhow::Error>(())
    })
    .await??;
    Ok(refreshed)
}

pub async fn fetch_all_profiles_usage() -> HashMap<String, Result<(UsageResponse, ProfileInfo)>> {
    // Sync Keychain credentials to current profile before fetching usage.
    // Claude Code updates the Keychain directly when refreshing tokens,
    // so the profile's credentials.json may be stale.
    // Run on a blocking thread to avoid stalling the Tokio worker with
    // the synchronous `security` subprocess call.
    let _ = tokio::task::spawn_blocking(super::profile::sync_keychain_to_current_profile).await;

    let current_profile = Tool::Claude.current_profile().ok().flatten();

    let profiles = match Tool::Claude.list_profiles() {
        Ok(p) => p,
        Err(_) => return HashMap::new(),
    };

    let mut handles = Vec::new();

    for profile in profiles {
        let is_current = current_profile.as_deref() == Some(profile.as_str());
        handles.push(tokio::spawn(async move {
            let result = async {
                let dir = Tool::Claude.profile_dir(&profile)?;
                let creds_path = dir.join("credentials.json");
                let (token, info) =
                    get_access_token_from_credentials(&creds_path, is_current).await?;
                let usage = fetch_usage_with_token(&token).await?;
                Ok((usage, info))
            }
            .await;
            (profile, result)
        }));
    }

    let mut results = HashMap::new();
    for handle in handles {
        match handle.await {
            Ok((profile, result)) => {
                results.insert(profile, result);
            }
            Err(e) => {
                eprintln!("profile task failed: {e}");
            }
        }
    }
    results
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- apply_token_response tests ---

    #[test]
    fn apply_token_response_normal_case_with_all_fields() {
        let mut raw: Value = serde_json::json!({
            "claudeAiOauth": {
                "accessToken": "old_access",
                "refreshToken": "old_refresh",
                "expiresAt": 0
            }
        });
        let token_resp = TokenResponse {
            access_token: "new_access".to_string(),
            refresh_token: Some("new_refresh".to_string()),
            expires_in: Some(7200),
        };
        apply_token_response(&mut raw, &token_resp).unwrap();

        let oauth = raw.get("claudeAiOauth").unwrap();
        assert_eq!(oauth["accessToken"], "new_access");
        assert_eq!(oauth["refreshToken"], "new_refresh");
        assert!(oauth["expiresAt"].as_u64().unwrap() > 0);
    }

    #[test]
    fn apply_token_response_expires_in_none_defaults_to_3600() {
        let mut raw: Value = serde_json::json!({
            "claudeAiOauth": {
                "accessToken": "old",
                "expiresAt": 0
            }
        });
        let token_resp = TokenResponse {
            access_token: "new".to_string(),
            refresh_token: None,
            expires_in: None,
        };
        let before_ms = Utc::now().timestamp_millis().max(0) as u64;
        apply_token_response(&mut raw, &token_resp).unwrap();
        let after_ms = Utc::now().timestamp_millis().max(0) as u64;

        let expires_at = raw["claudeAiOauth"]["expiresAt"].as_u64().unwrap();
        // Default 3600s = 3_600_000ms
        assert!(expires_at >= before_ms + 3_600_000);
        assert!(expires_at <= after_ms + 3_600_000);
    }

    #[test]
    fn apply_token_response_expires_in_zero_clamps_to_60() {
        let mut raw: Value = serde_json::json!({
            "claudeAiOauth": {
                "accessToken": "old",
                "expiresAt": 0
            }
        });
        let token_resp = TokenResponse {
            access_token: "new".to_string(),
            refresh_token: None,
            expires_in: Some(0),
        };
        let before_ms = Utc::now().timestamp_millis().max(0) as u64;
        apply_token_response(&mut raw, &token_resp).unwrap();
        let after_ms = Utc::now().timestamp_millis().max(0) as u64;

        let expires_at = raw["claudeAiOauth"]["expiresAt"].as_u64().unwrap();
        // Clamped to 60s = 60_000ms
        assert!(expires_at >= before_ms + 60_000);
        assert!(expires_at <= after_ms + 60_000);
    }

    #[test]
    fn apply_token_response_expires_in_large_clamps_to_86400() {
        let mut raw: Value = serde_json::json!({
            "claudeAiOauth": {
                "accessToken": "old",
                "expiresAt": 0
            }
        });
        let token_resp = TokenResponse {
            access_token: "new".to_string(),
            refresh_token: None,
            expires_in: Some(999999),
        };
        let before_ms = Utc::now().timestamp_millis().max(0) as u64;
        apply_token_response(&mut raw, &token_resp).unwrap();
        let after_ms = Utc::now().timestamp_millis().max(0) as u64;

        let expires_at = raw["claudeAiOauth"]["expiresAt"].as_u64().unwrap();
        // Clamped to 86400s = 86_400_000ms
        assert!(expires_at >= before_ms + 86_400_000);
        assert!(expires_at <= after_ms + 86_400_000);
    }

    #[test]
    fn apply_token_response_refresh_token_none_does_not_overwrite() {
        let mut raw: Value = serde_json::json!({
            "claudeAiOauth": {
                "accessToken": "old_access",
                "refreshToken": "existing_refresh",
                "expiresAt": 0
            }
        });
        let token_resp = TokenResponse {
            access_token: "new_access".to_string(),
            refresh_token: None,
            expires_in: Some(3600),
        };
        apply_token_response(&mut raw, &token_resp).unwrap();

        assert_eq!(raw["claudeAiOauth"]["refreshToken"], "existing_refresh");
    }

    // --- is_token_expired tests ---

    #[test]
    fn is_token_expired_none_returns_false() {
        let oauth = OAuthData {
            access_token: "tok".to_string(),
            refresh_token: None,
            expires_at: None,
            plan_type: None,
        };
        assert!(!is_token_expired(&oauth));
    }

    #[test]
    fn is_token_expired_far_past_returns_true() {
        let oauth = OAuthData {
            access_token: "tok".to_string(),
            refresh_token: None,
            expires_at: Some(0),
            plan_type: None,
        };
        assert!(is_token_expired(&oauth));
    }

    #[test]
    fn is_token_expired_far_future_returns_false() {
        let oauth = OAuthData {
            access_token: "tok".to_string(),
            refresh_token: None,
            expires_at: Some(u64::MAX),
            plan_type: None,
        };
        assert!(!is_token_expired(&oauth));
    }

    // --- read_oauth tests ---

    #[test]
    fn read_oauth_full_payload() {
        let raw: Value = serde_json::json!({
            "claudeAiOauth": {
                "accessToken": "acc",
                "refreshToken": "ref",
                "expiresAt": 1234567890,
                "planType": "pro"
            }
        });
        let oauth = read_oauth(&raw).unwrap();
        assert_eq!(oauth.access_token, "acc");
        assert_eq!(oauth.refresh_token.as_deref(), Some("ref"));
        assert_eq!(oauth.expires_at, Some(1234567890));
        assert_eq!(oauth.plan_type.as_deref(), Some("pro"));
    }

    #[test]
    fn read_oauth_missing_key_returns_error() {
        let raw: Value = serde_json::json!({});
        let err = read_oauth(&raw).unwrap_err();
        assert!(err.to_string().contains("no OAuth data"));
    }

    #[test]
    fn read_oauth_optional_fields_none_when_omitted() {
        let raw: Value = serde_json::json!({
            "claudeAiOauth": {
                "accessToken": "acc"
            }
        });
        let oauth = read_oauth(&raw).unwrap();
        assert_eq!(oauth.access_token, "acc");
        assert!(oauth.refresh_token.is_none());
        assert!(oauth.expires_at.is_none());
        assert!(oauth.plan_type.is_none());
    }

    #[test]
    fn usage_response_accepts_null_resets_at() {
        let payload = r#"{
            "five_hour": { "utilization": 0.0, "resets_at": null },
            "seven_day": { "utilization": 42.0, "resets_at": "2026-02-20T00:00:00+00:00" }
        }"#;

        let parsed: Result<UsageResponse, _> = serde_json::from_str(payload);

        assert!(parsed.is_ok());
        assert!(
            parsed
                .expect("usage payload should deserialize")
                .five_hour
                .resets_at
                .is_none()
        );
    }

    #[cfg(unix)]
    #[test]
    fn atomic_write_with_permissions_sets_0o600() {
        use std::os::unix::fs::PermissionsExt;

        use crate::fs_util;

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("credentials.json");

        fs_util::atomic_write(&path, r#"{"claudeAiOauth":{}}"#).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();

        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "credential file should be owner-only (0o600)");
    }
}
