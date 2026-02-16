use std::path::PathBuf;

use anyhow::{Result, anyhow};
use chrono::{DateTime, Utc};
use serde::Deserialize;
use serde_json::Value;

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
    #[serde(rename = "organizationName")]
    organization_name: Option<String>,
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
    pub resets_at: DateTime<Utc>,
}

pub struct ProfileInfo {
    #[allow(dead_code)]
    pub organization_name: Option<String>,
    pub plan_type: Option<String>,
}

fn creds_path() -> Result<PathBuf> {
    let current = Tool::Claude
        .current_profile()?
        .ok_or_else(|| anyhow!("no current profile set for Claude Code"))?;
    Ok(Tool::Claude.profile_dir(&current)?.join("credentials.json"))
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
            let now_ms = Utc::now().timestamp_millis() as u64;
            now_ms + 300_000 >= expires_at
        }
        None => false,
    }
}

async fn refresh_token(oauth: &OAuthData) -> Result<TokenResponse> {
    let refresh_token = oauth
        .refresh_token
        .as_ref()
        .ok_or_else(|| anyhow!("no refresh token available"))?;

    let client = reqwest::Client::new();
    let resp = client
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

fn update_credentials(path: &PathBuf, raw: &mut Value, token_resp: &TokenResponse) {
    if let Some(oauth) = raw.get_mut("claudeAiOauth") {
        oauth["accessToken"] = Value::String(token_resp.access_token.clone());
        if let Some(new_refresh) = &token_resp.refresh_token {
            oauth["refreshToken"] = Value::String(new_refresh.clone());
        }
        let expires_in = token_resp.expires_in.unwrap_or(3600);
        let new_expires_at = Utc::now().timestamp_millis() as u64 + expires_in * 1000;
        oauth["expiresAt"] = Value::Number(new_expires_at.into());
    }
    // Best-effort write; ignore errors
    let _ = std::fs::write(path, serde_json::to_string_pretty(raw).unwrap_or_default());
}

async fn get_access_token() -> Result<(String, ProfileInfo)> {
    let path = creds_path()?;
    let content = std::fs::read_to_string(&path)?;
    let mut raw: Value = serde_json::from_str(&content)?;
    let oauth = read_oauth(&raw)?;

    let info = ProfileInfo {
        organization_name: oauth.organization_name.clone(),
        plan_type: oauth.plan_type.clone(),
    };

    if !is_token_expired(&oauth) {
        return Ok((oauth.access_token, info));
    }

    // Token expired, refresh it
    let token_resp = refresh_token(&oauth).await?;
    let access_token = token_resp.access_token.clone();
    update_credentials(&path, &mut raw, &token_resp);

    Ok((access_token, info))
}

pub async fn fetch_usage() -> Result<(UsageResponse, ProfileInfo)> {
    let (token, info) = get_access_token().await?;

    let client = reqwest::Client::new();
    let resp = client
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

    let usage: UsageResponse = resp.json().await?;
    Ok((usage, info))
}
