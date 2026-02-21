use std::collections::HashMap;
use std::path::Path;

use anyhow::{Result, anyhow};
use chrono::{DateTime, Utc};
use serde::Deserialize;
use serde_json::Value;

use super::keychain;
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
            let now_ms = Utc::now().timestamp_millis();
            if now_ms < 0 {
                return true;
            }
            (now_ms as u64).saturating_add(300_000) >= expires_at
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
    let expires_in = token_resp.expires_in.unwrap_or(3600).min(86400);
    let now_ms = Utc::now().timestamp_millis().max(0) as u64;
    let new_expires_at = now_ms + expires_in.saturating_mul(1000);
    oauth["expiresAt"] = Value::Number(new_expires_at.into());
    Ok(())
}

async fn get_access_token() -> Result<(String, ProfileInfo)> {
    let mut raw = keychain::read()?;
    let oauth = read_oauth(&raw)?;

    let info = ProfileInfo {
        plan_type: oauth.plan_type.clone(),
    };

    if !is_token_expired(&oauth) {
        return Ok((oauth.access_token, info));
    }

    // Token expired, refresh it
    let token_resp = refresh_token(&oauth).await?;
    let access_token = token_resp.access_token.clone();
    apply_token_response(&mut raw, &token_resp)?;
    keychain::write(&raw).map_err(|e| {
        anyhow!(
            "token refreshed but keychain write failed (re-authenticate): {}",
            e
        )
    })?;

    Ok((access_token, info))
}

pub async fn fetch_usage() -> Result<(UsageResponse, ProfileInfo)> {
    let (token, info) = get_access_token().await?;
    let usage = fetch_usage_with_token(&token).await?;
    Ok((usage, info))
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

    if !resp.status().is_success() {
        return Err(anyhow!(
            "usage API returned status {}: {}",
            resp.status(),
            resp.text().await.unwrap_or_default()
        ));
    }

    Ok(resp.json().await?)
}

async fn get_access_token_from_credentials(path: &Path) -> Result<(String, ProfileInfo)> {
    let content = std::fs::read_to_string(path)?;
    let mut raw: Value = serde_json::from_str(&content)?;
    let oauth = read_oauth(&raw)?;

    let info = ProfileInfo {
        plan_type: oauth.plan_type.clone(),
    };

    if !is_token_expired(&oauth) {
        return Ok((oauth.access_token, info));
    }

    // Token expired, refresh and update credentials.json
    let token_resp = refresh_token(&oauth)
        .await
        .map_err(|_| anyhow!("Refresh token expired (switch to this profile to re-auth)"))?;
    let access_token = token_resp.access_token.clone();
    apply_token_response(&mut raw, &token_resp)?;
    fs_util::atomic_write(path, &serde_json::to_string_pretty(&raw)?)?;

    Ok((access_token, info))
}

pub async fn fetch_all_profiles_usage() -> HashMap<String, Result<(UsageResponse, ProfileInfo)>> {
    let profiles = match Tool::Claude.list_profiles() {
        Ok(p) => p,
        Err(_) => return HashMap::new(),
    };
    let current = Tool::Claude.current_profile().ok().flatten();

    let mut handles = Vec::new();

    for profile in profiles {
        let is_current = current.as_deref() == Some(profile.as_str());
        handles.push(tokio::spawn(async move {
            let result = if is_current {
                fetch_usage().await
            } else {
                async {
                    let dir = Tool::Claude.profile_dir(&profile)?;
                    let creds_path = dir.join("credentials.json");
                    let (token, info) = get_access_token_from_credentials(&creds_path).await?;
                    let usage = fetch_usage_with_token(&token).await?;
                    Ok((usage, info))
                }
                .await
            };
            (profile, result)
        }));
    }

    let mut results = HashMap::new();
    for handle in handles {
        if let Ok((profile, result)) = handle.await {
            results.insert(profile, result);
        }
    }
    results
}

#[cfg(test)]
mod tests {
    use super::UsageResponse;

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
}
